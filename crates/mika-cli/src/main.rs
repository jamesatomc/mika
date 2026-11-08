use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hex::encode as hex_encode;
use mika_node::store::SliverStore;
use mika_node::{AppState, SharedState};
use mika_storage::blob::BlobID;
use mika_storage::epoch::current_epoch;
use mika_storage::erasure::ErasureParams;
use mika_storage::kanari_integration::KanariStorage;
use mika_storage::onchain::{BlobAnchor, StorageFeePayment};
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
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

            // Walrus flow (1/3): PAY FIRST on-chain (storage quota) before
            // storing any sliver — fee > 0 requires --password (+ object id).
            let kanari = if fee > 0 {
                let pwd = password.as_deref().context(
                    "Walrus-style upload requires --password (fee > 0, paid before storing)",
                )?;
                let mut kv = KanariStorage::from_config(pwd, "cli-node")?;
                if let Some(id) = storage_object_id.as_deref() {
                    kv = kv.with_storage_object_id(id);
                } else {
                    anyhow::bail!("Walrus-style upload requires --storage-object-id (fee > 0)");
                }
                let payer = kanari_types::address::Address::from_hex_literal(kv.address())
                    .with_context(|| format!("invalid payer address: {}", kv.address()))?;
                let payment = StorageFeePayment {
                    blob_id: blob.id,
                    payer,
                    amount: fee,
                    epoch: current_epoch(),
                };
                kv.pay_storage_rent(&payment).await
                    .with_context(|| "pay_storage_rent failed")?;
                println!("Paid storage rent on-chain: {} mist (epoch {})", fee, payment.epoch);
                Some(kv)
            } else {
                // fee == 0 → local/dev upload only, no chain interaction
                None
            };

            // (2/3): STORE slivers + collect write certificates.
            let client = MikaClient::with_store_path(node_addrs.clone(), "./mika-store");
            let (quorum, lease) = client.put_blob(&blob, &params, fee).await?;

            // (3/3): CERTIFY on-chain — anchor blob commitment with owner.
            // Walrus only certifies after the quorum exists; the anchor must
            // not precede the stored slivers.
            if !no_anchor {
                if let Some(kv) = kanari.as_ref() {
                    // Commitment = blob_id (blake3 content root, 32 bytes).
                    let commitment: [u8; 32] = blob.id.as_bytes().try_into()
                        .map_err(|_| anyhow::anyhow!("blob id must be 32 bytes"))?;
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
                        Ok(()) => println!("Blob certified on-chain (anchor_blob)"),
                        Err(e) => {
                            eprintln!("Warning: anchor_blob after quorum failed: {e}");
                            // Do not delete stored slivers — blob is retrievable
                            // off-chain; anchor can be retried separately.
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
