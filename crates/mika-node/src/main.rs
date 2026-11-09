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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| anyhow::anyhow!("setting tracer: {}", e))?;

    let cli = Cli::parse();

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
                        p.send_fetch(&ann.peer_id, ann.blob_id, ann.shard_index);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
    }

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
