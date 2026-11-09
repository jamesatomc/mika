//! Mika CLI — P2P-only node mesh client.
//!
//! Nodes are pure libp2p peers (no HTTP API, no web UI). The CLI pushes
//! slivers to ONE entry peer over the store protocol
//! (`/mika/sliver-store/1.0.0`) and the mesh disseminates them gossipsub +
//! fetch. `--nodes` therefore takes ONE multiaddr:
//! `/ip4/<ip>/tcp/<port>/p2p/<PeerId>` (the entry node's, printed at
//! startup by `mika-node`).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hex::encode as hex_encode;
use mika_core::KanariStorage;
use mika_core::erasure::{ErasureParams, rejoin_shards};
use mika_server::{AppState, SharedState};
use mika_storage::store::SliverStore;
use mika_types::blob::BlobID;
use mika_types::epoch::current_epoch;
use mika_types::onchain::BlobAnchor;
use mika_types::{Blob, StoragePayment};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "mika-cli")]
#[command(about = "Mika decentralized storage CLI (P2P-only nodes)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Upload a file (Walrus-style: store, then certify on-chain)
    Upload {
        /// Path to the file to upload
        #[arg(short, long)]
        file: PathBuf,
        /// Entry peer — full multiaddr /ip4/<ip>/tcp/<port>/p2p/<PeerId>,
        /// or short `host:port` (e.g. 127.0.0.1:34001) resolved via ./.mesh
        #[arg(short, long)]
        nodes: String,
        /// Number of data shards (k)
        #[arg(long, default_value_t = 3)]
        data_shards: usize,
        /// Number of parity shards (m)
        #[arg(long, default_value_t = 2)]
        parity_shards: usize,
        /// Storage fee paid on-chain (in mist) — must exceed 0 to upload (Walrus-style)
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        /// Blob flavor: binary (default) or pq (Dilithium-signed)
        #[arg(long, default_value = "binary")]
        blob_type: String,
        /// Keystore password (required when anchoring on-chain)
        #[arg(long)]
        password: Option<String>,
        /// Storage object ID on-chain (required when anchoring on-chain)
        #[arg(long)]
        storage_object_id: Option<String>,
        /// Committee object id (required when anchoring on-chain; PoA gate)
        #[arg(long)]
        committee_id: Option<String>,
        /// Skip on-chain certify (anchor) after storage — offline dev mode
        #[arg(long, default_value_t = false)]
        no_anchor: bool,
    },
    /// Download a file from the entry peer (it keeps every shard)
    Download {
        /// Blob ID (hex-encoded) of the file to download
        blob_id: String,
        /// Output file path
        #[arg(short, long)]
        output: PathBuf,
        /// Entry peer — full multiaddr /ip4/<ip>/tcp/<port>/p2p/<PeerId>,
        /// or short `host:port` (e.g. 127.0.0.1:34001) resolved via ./.mesh
        #[arg(short = 'n', long)]
        nodes: String,
        /// Number of data shards (k)
        #[arg(long, default_value_t = 3)]
        data_shards: usize,
        /// Number of parity shards (m)
        #[arg(long, default_value_t = 2)]
        parity_shards: usize,
    },
    /// Committee registry (Walrus system-object analogue)
    Committee {
        #[command(subcommand)]
        cmd: CommitteeCmd,
    },
    /// Start a Mika storage node (P2P-only)
    Node {
        /// Node ID
        #[arg(long, default_value = "node-1")]
        node_id: String,
        /// Data directory for storing slivers
        #[arg(long, default_value = "./mika-data")]
        data_dir: PathBuf,
        /// Disable P2P control plane (offline peer)
        #[arg(long, default_value_t = false)]
        no_p2p: bool,
        /// libp2p listen port (0 = random; multiaddr is logged)
        #[arg(long, default_value_t = 0)]
        p2p_listen: u16,
        /// Bootstrap multiaddr (repeatable)
        #[arg(long)]
        bootstrap: Vec<String>,
        /// Committee snapshot file (one `0x` address per line) — mirrors
        /// the on-chain `mika::committee` registry; membership size is logged
        #[arg(long)]
        committee_signers: Option<PathBuf>,
    },
    /// Get a storage proof for a sliver (via the entry peer)
    Proof {
        /// Blob ID (hex-encoded)
        blob_id: String,
        /// Shard index
        shard_index: u32,
        /// Entry peer — full multiaddr /ip4/<ip>/tcp/<port>/p2p/<PeerId>,
        /// or short `host:port` (e.g. 127.0.0.1:34001) resolved via ./.mesh
        #[arg(short = 'n', long)]
        nodes: String,
    },
    /// Anchor a blob's metadata on-chain via kanari-sdk
    Anchor {
        /// Blob ID (hex-encoded)
        blob_id: String,
        /// Owner address (hex)
        owner: String,
        /// Commitment hash (hex)
        commitment: String,
        /// Keystore password for decrypting the signing key
        #[arg(long)]
        password: String,
        /// Storage fee paid (in mist)
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// Buy storage quota on-chain (Walrus `buy_storage`)
    Buy {
        /// Quota amount (in mist units recorded on the Storage object)
        #[arg(short, long)]
        amount: u64,
        /// Keystore password for decrypting the signing key
        #[arg(long)]
        password: String,
        /// Storage object ID on-chain
        #[arg(long)]
        storage_object_id: String,
    },
    /// Reclaim an expired blob's anchor on-chain (refunds quota to Storage)
    Reclaim {
        /// Blob ID (hex-encoded) to reclaim
        blob_id: String,
        /// Storage object ID on-chain
        #[arg(long)]
        storage_object_id: String,
        /// Keystore password for decrypting the signing key
        #[arg(long)]
        password: String,
        /// Epoch timestamp (seconds) to check expiry against (defaults to now)
        #[arg(long)]
        now_epoch: Option<u64>,
    },
    /// Upgrade a blob to a new version (new content = new blob_id)
    Upgrade {
        /// Old blob ID (hex-encoded) to upgrade from
        old_blob_id: String,
        /// New blob ID (hex-encoded) to upgrade to
        new_blob_id: String,
        /// Owner address (hex)
        owner: String,
        /// Commitment hash (hex)
        commitment: String,
        /// Keystore password for decrypting the signing key
        #[arg(long)]
        password: String,
        /// Storage object ID on-chain
        #[arg(long)]
        storage_object_id: String,
        /// Storage fee paid (in mist)
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
}

#[derive(Subcommand)]
enum CommitteeCmd {
    /// Register a node key in the committee object (shared registry)
    Join {
        /// Committee object id (from `committee::init` event)
        #[arg(long)]
        committee_id: String,
        /// Node key address to register (hex)
        #[arg(long)]
        node_key: String,
        /// Shards this node serves (default 1)
        #[arg(long, default_value_t = 1)]
        shard_count: u64,
        /// Keystore password for decrypting the signing key
        #[arg(long)]
        password: String,
    },
    /// Remove a node key from the committee object
    Leave {
        #[arg(long)]
        committee_id: String,
        #[arg(long)]
        node_key: String,
        #[arg(long)]
        password: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Committee { cmd } => match cmd {
            CommitteeCmd::Join {
                committee_id,
                node_key,
                shard_count,
                password,
            } => {
                let storage = KanariStorage::from_config(&password, "cli-node")?;
                let tx = storage
                    .join_committee(&committee_id, &node_key, shard_count)
                    .await?;
                eprintln!("Joined committee {} (tx {})", committee_id, tx);
            }
            CommitteeCmd::Leave {
                committee_id,
                node_key,
                password,
            } => {
                let storage = KanariStorage::from_config(&password, "cli-node")?;
                let tx = storage.leave_committee(&committee_id, &node_key).await?;
                eprintln!("Left committee {} (tx {})", committee_id, tx);
            }
        },
        Commands::Buy {
            amount,
            password,
            storage_object_id,
        } => {
            let storage = KanariStorage::from_config(&password, "cli-node")?
                .with_storage_object_id(storage_object_id);
            let hash = storage.buy_storage(amount).await?;
            eprintln!("Bought storage quota on-chain: {} (tx {})", amount, hash);
        }

        Commands::Reclaim {
            blob_id,
            storage_object_id,
            password,
            now_epoch,
        } => {
            let blob_id = BlobID::try_from(blob_id.as_str())?;
            let storage = KanariStorage::from_config(&password, "cli-node")?
                .with_storage_object_id(storage_object_id.clone());
            let now = now_epoch.unwrap_or_else(current_epoch);
            let storage_id = hex::decode(storage_object_id.trim_start_matches("0x"))
                .with_context(|| format!("invalid storage object id: {}", storage_object_id))?;
            let request = mika_types::storage::ReclaimBlobRequest {
                storage_id,
                blob_id: blob_id.as_bytes().to_vec(),
                now_epoch: now,
            };
            storage.reclaim_blob(&request).await?;
            eprintln!(
                "Reclaimed blob {} on-chain (epoch {}): quota refunded",
                blob_id, now
            );
        }

        Commands::Upgrade {
            old_blob_id,
            new_blob_id,
            owner,
            commitment,
            password,
            storage_object_id,
            fee,
        } => {
            let old_blob_id = BlobID::try_from(old_blob_id.as_str())?;
            let new_blob_id = BlobID::try_from(new_blob_id.as_str())?;
            let owner_addr = kanari_types::address::Address::from_hex_literal(&owner)
                .with_context(|| format!("invalid owner address: {}", owner))?;
            let commitment_bytes: [u8; 32] = hex::decode(&commitment)
                .with_context(|| "commitment must be 64 hex chars (32 bytes)")?
                .try_into()
                .map_err(|_| anyhow::anyhow!("commitment must be exactly 32 bytes"))?;
            let storage = KanariStorage::from_config(&password, "cli-node")?
                .with_storage_object_id(storage_object_id.clone());
            let storage_id = hex::decode(storage_object_id.trim_start_matches("0x"))
                .with_context(|| format!("invalid storage object id: {}", storage_object_id))?;
            let request = mika_types::storage::UpgradeBlobRequest {
                storage_id,
                old_blob_id: old_blob_id.as_bytes().to_vec(),
                new_blob_id: new_blob_id.as_bytes().to_vec(),
                owner: owner_addr.to_bytes().to_vec(),
                commitment: commitment_bytes.to_vec(),
                epoch: current_epoch(),
                fee_paid: fee,
            };
            storage.upgrade_blob(&request).await?;
            eprintln!(
                "Upgraded blob {} -> {} on-chain (fee {} mist)",
                old_blob_id, new_blob_id, fee
            );
        }

        Commands::Anchor {
            blob_id,
            owner,
            commitment,
            password,
            fee,
        } => {
            let blob_id = BlobID::try_from(blob_id.as_str())?;
            let owner_addr = kanari_types::address::Address::from_hex_literal(&owner)
                .with_context(|| format!("invalid owner address: {}", owner))?;
            let commitment_bytes: [u8; 32] = hex::decode(&commitment)
                .with_context(|| "commitment must be 64 hex chars (32 bytes)")?
                .try_into()
                .map_err(|_| anyhow::anyhow!("commitment must be exactly 32 bytes"))?;

            // Load keypair from kanari config via kanari-common
            let storage = KanariStorage::from_config(&password, "cli-node")?;
            let anchor = BlobAnchor {
                blob_id,
                owner: owner_addr,
                commitment: commitment_bytes,
                epoch: current_epoch(),
                fee_paid: fee,
            };

            // Try to anchor (will fail with "not yet implemented" message,
            // but demonstrates the integration path)
            match storage.anchor_blob(&anchor).await {
                Ok(()) => {
                    eprintln!("Blob anchored on-chain");
                    eprintln!("  Blob ID: {}", blob_id);
                    eprintln!("  Owner: {}", owner);
                    eprintln!("  Commitment: {}", commitment);
                    eprintln!("  Fee paid: {} mist", fee);
                }
                Err(e) => {
                    eprintln!("Anchor not yet implemented against BlockchainEngine");
                    eprintln!("  Blob ID: {}", blob_id);
                    eprintln!("  Owner: {}", owner);
                    eprintln!("  Commitment: {}", commitment);
                    eprintln!("  Fee paid: {} mist", fee);
                    eprintln!("  Error: {}", e);
                }
            }
        }

        Commands::Upload {
            file,
            nodes,
            data_shards,
            parity_shards,
            fee,
            blob_type,
            password,
            storage_object_id,
            committee_id,
            no_anchor,
        } => {
            let data = tokio::fs::read(&file)
                .await
                .with_context(|| format!("reading file {:?}", file))?;

            let flavor = mika_types::BlobType::from_str(&blob_type)
                .with_context(|| format!("bad blob-type '{blob_type}' (binary|pq)"))?;
            let mut blob = match flavor {
                mika_types::BlobType::Pq => Blob::pq(data),
                mika_types::BlobType::Binary => Blob::new(data),
            };
            let params = ErasureParams::new(data_shards, parity_shards)?;

            // Walrus order for this contract is: STORE → CERTIFY (anchor with
            // fee_paid). `pay_storage_rent` requires the anchor to exist
            // (it does `find_anchor_index`), so paying first aborts
            // (COMMITTED/FAILED on explorer). The fee is recorded by the
            // anchor itself (`fee_paid`), so no separate pay Tx is needed
            // for a first upload. `pay_storage_rent` remains for renewals
            // (top-up fee_paid after the anchor exists).
            let kanari = if !no_anchor {
                let pwd = password.as_deref().context(
                    "Walrus-style certify requires --password (anchor needs the signer)",
                )?;
                let mut kv = KanariStorage::from_config(pwd, "cli-node")?;
                if let Some(id) = storage_object_id.as_deref() {
                    kv = kv.with_storage_object_id(id);
                } else {
                    anyhow::bail!("Walrus-style certify requires --storage-object-id");
                }
                Some(kv)
            } else {
                // --no-anchor → local/dev upload only, no chain interaction
                None
            };

            // (1/2): STORE — P2P push: encode locally, send every sliver
            // once to the entry peer; that peer announces each shard and
            // the mesh spreads the rest (peer-pull). The CLI never speaks
            // to any other node.
            let shards = mika_core::erasure::split_into_shards(&blob.data, params.data_shards)
                .with_context(|| "split")?;
            // Red Stuff 2D (Walrus-style): primary + secondary grids, 2n
            // slivers total. Primary indices 0..n, secondary n..2n — the
            // entry peer stores every sliver and announces them all.
            let encoded = mika_core::erasure::redstuff_encode(&shards, &params)
                .with_context(|| "redstuff encode")?;
            let n = params.total_shards() as u32;
            let slivers: Vec<mika_types::sliver::Sliver> = encoded
                .primary
                .iter()
                .enumerate()
                .chain(
                    encoded
                        .secondary
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i + n as usize, s)),
                )
                .map(|(i, shard)| {
                    mika_types::sliver::Sliver::new(
                        blob.id,
                        i as u32,
                        params.total_shards() as u32,
                        params.data_shards as u32,
                        params.parity_shards as u32,
                        blob.data.len() as u64,
                        shard.clone(),
                    )
                })
                .collect();

            let certs = mika_server::p2p::push_slivers(&nodes, slivers)
                .await
                .with_context(|| format!("push slivers to entry peer {nodes}"))?;
            let mut quorum = mika_core::certificate::WriteCertificateQuorum::new(
                blob.id,
                params.total_shards() as u32,
                params.data_shards as u32,
                params.parity_shards as u32,
                blob.data.len() as u64,
            );
            for cert in certs {
                quorum.add(cert);
            }
            let lease = mika_types::epoch::Lease::new_for_epochs(
                blob.id,
                mika_types::epoch::epochs_for_fee(fee),
                fee,
            );
            eprintln!("  P2P push committed: {} certs", quorum.certificates.len());

            // PQ blob: Dilithium-sign the payload (Walrus PQ 4X/5X path).
            if blob.flavor == mika_types::BlobType::Pq {
                if let Some(_kv) = kanari.as_ref() {
                    // Sign with the uploader key (Dilithium3) and attach to blob.
                    use kanari_crypto::CurveType;
                    let keypair = kanari_crypto::generate_keypair(CurveType::Dilithium3)
                        .with_context(|| "Dilithium keypair generation failed")?;
                    let sig = kanari_crypto::signatures::sign_message(
                        &keypair.private_key,
                        &blob.data,
                        CurveType::Dilithium3,
                    )
                    .with_context(|| "PQ Dilithium signing failed")?;
                    blob.pq_signature = sig;
                    eprintln!(
                        "  PQ blob: Dilithium3 sig {} bytes (pubkey {})",
                        blob.pq_signature.len(),
                        &keypair.public_key[..16.min(keypair.public_key.len())]
                    );
                } else {
                    anyhow::bail!("PQ upload requires --password (Dilithium signer)");
                }
            }
            // BFT quorum: certificates are already signed by storage nodes.
            // Verify all signatures and confirm we have a quorum of distinct
            // nodes (Walrus: 2f+1 committee signatures).
            if let Some(_kv) = kanari.as_ref() {
                let ok = mika_core::kanari_integration::verify_quorum(&quorum)?;
                eprintln!(
                    "  Quorum: {} certs, {} distinct nodes, verified={}",
                    quorum.certificates.len(),
                    quorum.distinct_nodes(),
                    ok
                );
                if !ok {
                    anyhow::bail!("quorum verification failed: invalid certificate signature");
                }
            }

            // (2/3): CERTIFY on-chain — anchor blob commitment with owner
            // (records `fee_paid = fee` on-chain at upload time — this is
            // the "pay on upload" step for Walrus-style certify).
            if !no_anchor && let Some(kv) = kanari.as_ref() {
                let commitment = mika_core::commitment::blob_commitment(&blob.data, &params);
                let owner = kanari_types::address::Address::from_hex_literal(kv.address())
                    .with_context(|| format!("invalid owner address: {}", kv.address()))?;
                let anchor = BlobAnchor {
                    blob_id: blob.id,
                    owner,
                    commitment,
                    epoch: current_epoch(),
                    fee_paid: fee,
                };
                let anchored = match kv.anchor_blob(&anchor).await {
                    Ok(()) => {
                        eprintln!("Paid + certified on-chain: {} mist", fee);
                        eprintln!("  anchor_blob (certify) committed");
                        true
                    }
                    Err(e) => {
                        // Idempotent on retry: an already-anchored blob aborts
                        // on-chain (E_BLOB_ALREADY_ANCHORED) — treat as ok.
                        eprintln!("Warning: anchor_blob after quorum failed: {e}");
                        false
                    }
                };
                // (3/3): PoA — aggregate the 2/3 write-certificate quorum and
                // record it on-chain (Walrus availability certificate).
                if anchored {
                    match mika_core::certificate::PoaCert::aggregate(
                        &quorum,
                        commitment,
                        current_epoch(),
                    ) {
                        Ok(poa) => match kv
                            .record_availability(committee_id.as_deref().unwrap_or(""), &poa)
                            .await
                        {
                            Ok(tx) => eprintln!(
                                "  PoA recorded on-chain ({} distinct signers, tx {})",
                                poa.signer_count, tx
                            ),
                            Err(e) => eprintln!("Warning: record_availability failed: {e}"),
                        },
                        Err(e) => {
                            eprintln!("Warning: PoA quorum not met, skipping record: {e}")
                        }
                    }
                }
            }

            eprintln!("Upload complete");
            eprintln!("  Blob ID: {}", blob.id);
            eprintln!("  Size: {} bytes", blob.data.len());
            eprintln!("  Shards: {} data + {} parity", data_shards, parity_shards);
            eprintln!("  Entry peer: {}", nodes);
            eprintln!("  Certificates collected: {}", quorum.certificates.len());
            eprintln!("  Lease expires at epoch: {}", lease.expires_at);
            eprintln!("  Fee paid: {} mist", lease.fee_paid);
        }

        Commands::Download {
            blob_id,
            output,
            nodes,
            data_shards,
            parity_shards,
        } => {
            let blob_id = BlobID::try_from(blob_id.as_str())?;
            let params = ErasureParams::new(data_shards, parity_shards)?;

            // Red Stuff 2D index space: primary 0..n, secondary n..2n.
            let n = params.total_shards() as u32;
            let all_shards: Vec<u32> = (0..2 * n).collect();
            let fetched = mika_server::p2p::fetch_slivers(&nodes, blob_id, all_shards)
                .await
                .with_context(|| format!("fetch from entry peer {nodes}"))?;
            let mut primary: Vec<Option<Vec<u8>>> = vec![None; params.total_shards()];
            let mut secondary: Vec<Option<Vec<u8>>> = vec![None; params.total_shards()];
            let mut original_len: Option<usize> = None;
            for sliver in fetched {
                if sliver.blob_id != blob_id {
                    continue;
                }
                let idx = sliver.shard_index as usize;
                if idx < n as usize {
                    primary[idx] = Some(sliver.data.clone());
                } else if idx < 2 * n as usize {
                    secondary[idx - n as usize] = Some(sliver.data.clone());
                }
                if original_len.is_none() {
                    original_len = Some(sliver.original_len as usize);
                }
            }
            let data_shards = mika_core::erasure::redstuff_decode(&primary, &secondary, &params)
                .with_context(|| "decode shards")?;
            let original_len =
                original_len.unwrap_or_else(|| data_shards.iter().map(|s| s.len()).sum());
            let data = rejoin_shards(&data_shards, original_len);
            tokio::fs::write(&output, &data)
                .await
                .with_context(|| format!("writing file {:?}", output))?;

            eprintln!("Download complete");
            eprintln!("  Blob ID: {}", blob_id);
            eprintln!("  Output: {:?}", output);
            eprintln!("  Size: {} bytes", data.len());
        }

        Commands::Node {
            node_id,
            data_dir,
            no_p2p,
            p2p_listen,
            bootstrap,
            committee_signers,
        } => {
            let committee_snapshot = if let Some(path) = &committee_signers {
                let committee =
                    mika_types::committee::load_committee_file(&path.to_string_lossy())?;
                eprintln!(
                    "mika committee: {} member(s) from {}",
                    committee.len(),
                    path.display()
                );
                committee
            } else {
                Vec::new()
            };
            let store = SliverStore::new(data_dir.clone())?;
            let epoch = current_epoch();
            let mut app_state = AppState::new(node_id.clone(), store, epoch);
            // Build the swarm first so announcements can carry our PeerId.
            let p2p_ready = if !no_p2p {
                use libp2p_identity::Keypair;
                let keypair = Keypair::generate_ed25519();
                let mut p2p = mika_server::p2p::MikaP2P::new(keypair, p2p_listen)?;
                eprintln!("mika p2p: peer {}", p2p.peer_id);
                for addr in p2p.swarm.listeners() {
                    eprintln!("mika p2p: multiaddr {}", addr);
                }
                p2p.dial_bootstrap(&bootstrap);
                app_state.peer_id = p2p.peer_id.to_string();
                Some(p2p)
            } else {
                None
            };
            app_state.committee = std::sync::Arc::new(committee_snapshot);
            let state: SharedState = Arc::new(app_state);
            if let Some(p2p) = p2p_ready {
                *state.p2p.lock().unwrap() = Some(p2p);
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
                            eprintln!(
                                "mika p2p: pulled sliver {}#{} into store",
                                sliver.blob_id, sliver.shard_index
                            );
                        }
                        for ann in drive_state.drain_pending() {
                            if ann.node_id == drive_state.node_id || ann.peer_id.is_empty() {
                                continue;
                            }
                            let Ok(blob_id) =
                                mika_types::blob::BlobID::try_from(ann.blob_id.as_str())
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
                                        let mut sorted: Vec<_> =
                                            committee.iter().cloned().collect();
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
                // BFT self-vote loop (mirror of mika-node).
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
                            let voter = mika_types::committee::node_key_for_public_key(
                                &vote_state.public_key,
                            )
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
            }
            eprintln!(
                "Starting Mika node {} (p2p: {})",
                node_id,
                if no_p2p { "disabled" } else { "enabled" }
            );
            eprintln!("  Data dir: {:?}", data_dir);
            std::future::pending::<()>().await;
        }

        Commands::Proof {
            blob_id,
            shard_index,
            nodes,
        } => {
            let blob_id = BlobID::try_from(blob_id.as_str())?;
            let fetched = mika_server::p2p::fetch_slivers(&nodes, blob_id, vec![shard_index])
                .await
                .with_context(|| format!("fetch from entry peer {nodes}"))?;
            let sliver = fetched
                .into_iter()
                .find(|s| s.blob_id == blob_id && s.shard_index == shard_index)
                .ok_or_else(|| anyhow::anyhow!("sliver {}#{} not found", blob_id, shard_index))?;
            let digest_arr = kanari_crypto::hash_data_blake3_array(&sliver.data);
            eprintln!(
                "{}",
                serde_json::json!({
                    "blob_id": blob_id.to_hex(),
                    "shard_index": shard_index,
                    "entry_peer": nodes,
                    "sliver_digest": hex_encode(digest_arr),
                    "sliver_size": sliver.data.len(),
                    "epoch": current_epoch(),
                })
            );
        }
    }

    Ok(())
}
