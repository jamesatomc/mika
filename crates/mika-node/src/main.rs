//! Mika storage node binary — P2P-only (no HTTP API, no web UI).
//!
//! A node is a pure libp2p peer:
//!   - gossipsub `mika/slivers/v1` announcements (control plane),
//!   - request-response `/mika/sliver-store/1.0.0` (ingress: clients push
//!     slivers, node signs write certificates) and
//!     `/mika/sliver-fetch/1.0.0` (dissemination: peers pull missing shards).
//!
//! One drive task pumps the swarm: inbound requests are serviced inline,
//! announcements are queued and any missing sliver is fetched from the
//! announcing peer. A reclaim loop sweeps expired leases.

use clap::Parser;
use mika_server::{AppState, SharedState};
use mika_storage::store::SliverStore;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser)]
#[command(name = "mika-node")]
#[command(about = "Mika storage node (P2P-only)")]
struct Cli {
    /// Node identifier
    #[arg(long, default_value = "node-1")]
    node_id: String,

    /// On-disk data directory
    #[arg(long, default_value = "./mika-data")]
    data_dir: PathBuf,

    /// Disable P2P (node then only keeps its fjall store — offline peer)
    #[arg(long, default_value_t = false)]
    no_p2p: bool,

    /// libp2p listen port (0 = random; the chosen multiaddr is logged)
    #[arg(long, default_value_t = 0)]
    p2p_listen: u16,

    /// Bootstrap multiaddr `/ip4/<ip>/tcp/<port>/p2p/<PeerId>` (repeatable)
    #[arg(long)]
    bootstrap: Vec<String>,

    /// Committee snapshot file (one `0x` address per line): mirrors the
    /// on-chain `mika::committee` registry. The size is logged; peers whose
    /// keys are in the file are the committee their certs aggregate for.
    #[arg(long)]
    committee_signers: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| anyhow::anyhow!("setting tracer: {}", e))?;

    let cli = Cli::parse();

    let committee_snapshot = if let Some(path) = &cli.committee_signers {
        let committee = mika_types::committee::load_committee_file(&path.to_string_lossy())?;
        info!(
            "mika committee: {} member(s) from {}",
            committee.len(),
            path.display()
        );
        committee
    } else {
        Vec::new()
    };

    // Build the swarm first so we know our PeerId for announcements.
    let p2p_ready = if !cli.no_p2p {
        use libp2p_identity::Keypair;
        // Ephemeral ed25519 keypair — PeerId changes per process; the
        // logged multiaddr is the only stable handle for clients.
        let keypair = Keypair::generate_ed25519();
        let mut p2p = mika_server::p2p::MikaP2P::new(keypair, cli.p2p_listen)?;
        info!("mika p2p: peer {}", p2p.peer_id);
        for addr in p2p.swarm.listeners() {
            info!("mika p2p: multiaddr {}", addr);
        }
        p2p.dial_bootstrap(&cli.bootstrap);
        Some(p2p)
    } else {
        None
    };

    let store = SliverStore::new(cli.data_dir.clone())?;
    let epoch = mika_types::epoch::current_epoch();
    let mut app_state = AppState::new(cli.node_id.clone(), store, epoch);
    if let Some(p) = p2p_ready.as_ref() {
        app_state.peer_id = p.peer_id.to_string();
    }
    app_state.committee = Arc::new(committee_snapshot);
    let state: SharedState = Arc::new(app_state);

    if let Some(p2p) = p2p_ready {
        *state.p2p.lock().unwrap() = Some(p2p);
        // Swarm drive loop: events + peer-pull dissemination.
        let drive_state = state.clone();
        tokio::spawn(async move {
            loop {
                let fetched = {
                    let mut guard = drive_state.p2p.lock().unwrap();
                    let mut p2p = guard.take();
                    let f = p2p
                        .as_mut()
                        .map(|p| p.drive_events(&drive_state))
                        .unwrap_or_default();
                    *guard = p2p;
                    f
                };
                for sliver in &fetched {
                    let _ = drive_state.store.put(sliver).await;
                    drive_state.expiry.lock().unwrap().insert(
                        sliver.blob_id.to_hex(),
                        drive_state.epoch + mika_types::epoch::EPOCH_SECONDS,
                    );
                    info!(blob = %sliver.blob_id, shard = sliver.shard_index, "mika p2p: pulled sliver into store");
                }
                // Drain queued announcements → fetch what we're missing.
                for ann in drive_state.drain_pending() {
                    if ann.node_id == drive_state.node_id || ann.peer_id.is_empty() {
                        continue;
                    }
                    let Ok(blob_id) = mika_types::blob::BlobID::try_from(ann.blob_id.as_str())
                    else {
                        continue;
                    };
                    if drive_state.store.has(&blob_id, ann.shard_index).await {
                        continue;
                    }
                    let mut guard = drive_state.p2p.lock().unwrap();
                    if let Some(p) = guard.as_mut() {
                        // Walrus-style routing: fetch from the shard's
                        // assigned node when known, else the announcer.
                        let assigned_peer = {
                            let committee = drive_state.committee.clone();
                            if committee.is_empty() {
                                None
                            } else {
                                let mut sorted: Vec<_> = committee.iter().cloned().collect();
                                sorted.sort_by_key(|a| a.to_string());
                                let key = sorted[(ann.shard_index as usize) % sorted.len()]
                                    .to_hex_literal();
                                p.assigned_peer(&key).map(|peer| peer.to_string())
                            }
                        };
                        p.send_fetch(
                            assigned_peer.as_deref().unwrap_or(&ann.peer_id),
                            ann.blob_id,
                            ann.shard_index,
                        );
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
    }

    // BFT self-vote loop: periodically sign and gossip the availability
    // state root for every blob we hold. When 2/3 of the committee votes
    // the same root, the blob is committed (Walrus-style PoA at runtime).
    let vote_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let blobs: Vec<_> = vote_state.store.list_blobs().await.unwrap_or_default();
            for blob_id in blobs {
                let present = vote_state.store.shards_for_blob(&blob_id).await;
                if present.is_empty() {
                    continue;
                }
                let root = mika_server::p2p::state_root_for(&present);
                let voter = mika_types::committee::node_key_for_public_key(&vote_state.public_key)
                    .map(|v| v.into_bytes())
                    .unwrap_or([0u8; 32]);
                let mut msg = Vec::new();
                msg.extend_from_slice(blob_id.to_hex().as_bytes());
                msg.extend_from_slice(&root);
                msg.extend_from_slice(&vote_state.epoch.to_le_bytes());
                msg.extend_from_slice(&voter);
                let digest = kanari_crypto::hash_data_blake3_array(&msg);
                let signature = kanari_crypto::signatures::sign_message(
                    &vote_state.private_key,
                    &digest,
                    vote_state.curve_type,
                )
                .unwrap_or_default();
                let vote = mika_server::p2p::AvailabilityVote {
                    blob_id: blob_id.to_hex(),
                    state_root: root,
                    epoch: vote_state.epoch,
                    voter,
                    public_key: vote_state.public_key.clone(),
                    signature,
                };
                let mut guard = vote_state.p2p.lock().unwrap();
                if let Some(p) = guard.as_mut() {
                    p.publish_vote(&vote);
                }
            }
        }
    });

    // Walrus reclaim loop: sweep expired leases, delete their shards.
    let reclaim_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let now = mika_types::epoch::current_epoch();
            let expired: Vec<String> = {
                let expiry = reclaim_state.expiry.lock().unwrap();
                expiry
                    .iter()
                    .filter_map(|(id, exp)| if *exp <= now { Some(id.clone()) } else { None })
                    .collect()
            };
            let expired_count = expired.len();
            for id in expired {
                if let Ok(bid) = mika_types::blob::BlobID::try_from(id.as_str()) {
                    let _ = reclaim_state.store.delete_blob(&bid).await;
                    reclaim_state.expiry.lock().unwrap().remove(&id);
                }
            }
            if expired_count > 0 {
                info!("mika reclaim: {} blobs purged", expired_count);
            }
        }
    });

    info!(
        "mika-node {} running (p2p: {})",
        cli.node_id,
        if cli.no_p2p { "disabled" } else { "enabled" }
    );
    std::future::pending::<()>().await;
    #[allow(unreachable_code)]
    Ok(())
}
