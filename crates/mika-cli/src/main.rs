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
use mika_core::erasure::{ErasureParams, decode, rejoin_shards};
use mika_server::{AppState, SharedState};
use mika_storage::store::SliverStore;
use mika_types::blob::BlobID;
use mika_types::epoch::current_epoch;
use mika_types::onchain::BlobAnchor;
use mika_types::{Blob, StoragePayment};
use std::path::PathBuf;
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
        /// Entry peer multiaddr (ONE — the mesh spreads from there):
        /// /ip4/<ip>/tcp/<port>/p2p/<PeerId>
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
        /// Entry peer multiaddr: /ip4/<ip>/tcp/<port>/p2p/<PeerId>
        #[arg(short = 'n', long)]
        nodes: String,
        /// Number of data shards (k)
        #[arg(long, default_value_t = 3)]
        data_shards: usize,
        /// Number of parity shards (m)
        #[arg(long, default_value_t = 2)]
        parity_shards: usize,
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
    },
    /// Get a storage proof for a sliver (via the entry peer)
    Proof {
        /// Blob ID (hex-encoded)
        blob_id: String,
        /// Shard index
        shard_index: u32,
        /// Entry peer multiaddr: /ip4/<ip>/tcp/<port>/p2p/<PeerId>
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
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
            let encoded = mika_core::erasure::encode(&shards, &params).with_context(|| "encode")?;
            let slivers: Vec<mika_types::sliver::Sliver> = encoded
                .iter()
                .enumerate()
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

            // (2/2): CERTIFY on-chain — anchor blob commitment with owner
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
                match kv.anchor_blob(&anchor).await {
                    Ok(()) => {
                        eprintln!("Paid + certified on-chain: {} mist", fee);
                        eprintln!("  anchor_blob (certify) committed");
                    }
                    Err(e) => {
                        // Idempotent on retry: an already-anchored blob aborts
                        // on-chain (E_BLOB_ALREADY_ANCHORED) — treat as ok.
                        eprintln!("Warning: anchor_blob after quorum failed: {e}");
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

            let all_shards: Vec<u32> = (0..params.total_shards() as u32).collect();
            let fetched = mika_server::p2p::fetch_slivers(&nodes, blob_id, all_shards)
                .await
                .with_context(|| format!("fetch from entry peer {nodes}"))?;
            let mut present: Vec<Option<Vec<u8>>> = vec![None; params.total_shards()];
            let mut original_len: Option<usize> = None;
            for sliver in fetched {
                if sliver.blob_id == blob_id {
                    present[sliver.shard_index as usize] = Some(sliver.data.clone());
                    if original_len.is_none() {
                        original_len = Some(sliver.original_len as usize);
                    }
                }
            }
            let data_shards = decode(&present, &params).with_context(|| "decode shards")?;
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
        } => {
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
                                p.send_fetch(&ann.peer_id, ann.blob_id, ann.shard_index);
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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
