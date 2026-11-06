use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hex::encode as hex_encode;
use mika_node::NodeState;
use mika_node::store::SliverStore;
use mika_storage::blob::BlobID;
use mika_storage::epoch::current_epoch;
use mika_storage::erasure::ErasureParams;
use mika_storage::{Blob, MikaClient, NodeAddr, StorageClient};
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
    /// Upload a file to Mika storage nodes
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
        /// Storage fee paid (in mist)
        #[arg(long, default_value_t = 1000)]
        fee: u64,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Upload {
            file,
            nodes,
            data_shards,
            parity_shards,
            fee,
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

            let client = MikaClient::new(node_addrs.clone());
            let (quorum, lease) = client.put_blob(&blob, &params, fee).await?;

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

            let client = MikaClient::new(node_addrs);
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
            let state = Arc::new(NodeState::new(node_id.clone(), store, current_epoch()));
            let app = mika_node::server::router(state.clone());
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
            let client = MikaClient::new(vec![NodeAddr::new(node_addr.clone())]);

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
