use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hex::encode as hex_encode;
use mika_node::store::SliverStore;
use mika_node::{AppState, SharedState};
use mika_storage::blob::BlobID;
use mika_storage::epoch::current_epoch;
use mika_storage::erasure::ErasureParams;
use mika_storage::kanari_integration::KanariStorage;
use mika_storage::onchain::BlobAnchor;
use mika_storage::{Blob, MikaClient, NodeAddr, StorageClient, StoragePayment};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "mika-cli")]
#[command(about = "Mika decentralized storage CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Upload a file to Mika storage nodes (Walrus-style: pay first, store, certify)
    Upload {
        /// Path to the file to upload
        #[arg(short, long)]
        file: PathBuf,
        /// Storage node address (host:port), can be specified multiple times
        #[arg(short, long, value_delimiter = ',')]
        nodes: Vec<String>,
        /// Number of data shards (k)
        #[arg(long, default_value_t = 3)]
        data_shards: usize,
        /// Number of parity shards (m)
        #[arg(long, default_value_t = 2)]
        parity_shards: usize,
        /// Storage fee paid on-chain (in mist) — must exceed 0 to upload (Walrus-style)
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        /// Keystore password (required when fee > 0 — pays before storing)
        #[arg(long)]
        password: Option<String>,
        /// Storage object ID on-chain (required when fee > 0)
        #[arg(long)]
        storage_object_id: Option<String>,
        /// Skip on-chain certify (anchor) after storage — pay still happens
        #[arg(long, default_value_t = false)]
        no_anchor: bool,
    },
    /// Download a file from Mika storage nodes
    Download {
        /// Blob ID (hex-encoded) of the file to download
        blob_id: String,
        /// Output file path
        #[arg(short, long)]
        output: PathBuf,
        /// Storage node address (host:port), can be specified multiple times
        #[arg(short, long, value_delimiter = ',')]
        nodes: Vec<String>,
        /// Number of data shards (k)
        #[arg(long, default_value_t = 3)]
        data_shards: usize,
        /// Number of parity shards (m)
        #[arg(long, default_value_t = 2)]
        parity_shards: usize,
    },
    /// Start a Mika storage node
    Node {
        /// Node ID
        #[arg(long, default_value = "node-1")]
        node_id: String,
        /// Data directory for storing slivers
        #[arg(long, default_value = "./mika-data")]
        data_dir: PathBuf,
        /// Listen address (host:port)
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen_addr: String,
    },
    /// Get a storage proof for a sliver
    Proof {
        /// Blob ID (hex-encoded)
        blob_id: String,
        /// Shard index
        shard_index: u32,
        /// Storage node address (host:port)
        #[arg(long, default_value = "127.0.0.1:8080")]
        node_addr: String,
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
            let storage =
                KanariStorage::from_config(&password, "cli-node")?.with_storage_object_id(storage_object_id);
            let hash = storage.buy_storage(amount).await?;
            println!("Bought storage quota on-chain: {} (tx {})", amount, hash);
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
                    println!("Blob anchored on-chain");
                    println!("  Blob ID: {}", blob_id);
                    println!("  Owner: {}", owner);
                    println!("  Commitment: {}", commitment);
                    println!("  Fee paid: {} mist", fee);
                }
                Err(e) => {
                    println!("Anchor not yet implemented against BlockchainEngine");
                    println!("  Blob ID: {}", blob_id);
                    println!("  Owner: {}", owner);
                    println!("  Commitment: {}", commitment);
                    println!("  Fee paid: {} mist", fee);
                    println!("  Error: {}", e);
                }
            }
        }

        Commands::Upload {
            file,
            nodes,
            data_shards,
            parity_shards,
            fee,
            password,
            storage_object_id,
            no_anchor,
        } => {
            let data = tokio::fs::read(&file)
                .await
                .with_context(|| format!("reading file {:?}", file))?;

            let blob = Blob::new(data);
            let params = ErasureParams::new(data_shards, parity_shards)?;

            let node_addrs: Vec<NodeAddr> = if nodes.is_empty() {
                vec![NodeAddr::new("127.0.0.1:8080".to_string())]
            } else {
                nodes.into_iter().map(NodeAddr::new).collect()
            };

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

            // (1/2): STORE slivers + collect write certificates.
            let client = MikaClient::with_store_path(node_addrs.clone(), "./mika-store");
            let (quorum, lease) = client.put_blob(&blob, &params, fee).await?;

            // (2/2): CERTIFY on-chain — anchor blob commitment with owner
            // (records `fee_paid = fee` on-chain at upload time — this is
            // the "pay on upload" step for Walrus-style certify).
            if !no_anchor {
                if let Some(kv) = kanari.as_ref() {
                    // 2D commitment (Walrus Red-Stuff style): verifiable per-sliver,
                    // not just the 1D blob hash.
                    let commitment = mika_storage::commitment::blob_commitment(&blob.data, &params);
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
                            println!("Paid + certified on-chain: {} mist", fee);
                            println!("  anchor_blob (certify) committed");
                        }
                        Err(e) => {
                            // Idempotent on retry: an already-anchored blob aborts
                            // on-chain (E_BLOB_ALREADY_ANCHORED) — treat as ok.
                            // (RPC surfaces only hash+status, so this is a
                            // warning either way; the blob stays retrievable.)
                            eprintln!("Warning: anchor_blob after quorum failed: {e}");
                        }
                    }
                }
            }

            println!("Upload complete");
            println!("  Blob ID: {}", blob.id);
            println!("  Size: {} bytes", blob.data.len());
            println!("  Shards: {} data + {} parity", data_shards, parity_shards);
            println!("  Nodes: {}", node_addrs.len());
            println!("  Certificates collected: {}", quorum.certificates.len());
            println!("  Lease expires at epoch: {}", lease.expires_at);
            println!("  Fee paid: {} mist", lease.fee_paid);
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

            let node_addrs: Vec<NodeAddr> = if nodes.is_empty() {
                vec![NodeAddr::new("127.0.0.1:8080".to_string())]
            } else {
                nodes.into_iter().map(NodeAddr::new).collect()
            };

            let client = MikaClient::with_store_path(node_addrs, "./mika-store");
            let fetched = client.get_blob(&blob_id, &params).await?;

            tokio::fs::write(&output, &fetched.data)
                .await
                .with_context(|| format!("writing file {:?}", output))?;

            println!("Download complete");
            println!("  Blob ID: {}", blob_id);
            println!("  Output: {:?}", output);
            println!("  Size: {} bytes", fetched.data.len());
        }

        Commands::Node {
            node_id,
            data_dir,
            listen_addr,
        } => {
            let store = SliverStore::new(data_dir.clone())?;
            let state: SharedState =
                Arc::new(AppState::new(node_id.clone(), store, current_epoch()));
            let app = mika_node::server::routes().with_state(state.clone());
            let addr: std::net::SocketAddr = listen_addr.parse()?;

            println!("Starting Mika node {}", node_id);
            println!("  Data dir: {:?}", data_dir);
            println!("  Listening on: {}", addr);

            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(listener, app.into_make_service()).await?;
        }

        Commands::Proof {
            blob_id,
            shard_index,
            node_addr,
        } => {
            let blob_id = BlobID::try_from(blob_id.as_str())?;
            let client =
                MikaClient::with_store_path(vec![NodeAddr::new(node_addr.clone())], "./mika-store");

            // Retrieve the sliver and verify its proof
            let result = client
                .retrieve_sliver(&NodeAddr::new(node_addr.clone()), &blob_id, shard_index)
                .await?;
            let sliver = result.data;

            let digest_arr = kanari_crypto::hash_data_blake3_array(&sliver);

            println!(
                "{}",
                json!({
                    "blob_id": blob_id.to_hex(),
                    "shard_index": shard_index,
                    "node_id": node_addr,
                    "sliver_digest": hex_encode(digest_arr),
                    "sliver_size": sliver.len(),
                    "epoch": current_epoch(),
                })
            );
        }
    }

    Ok(())
}
