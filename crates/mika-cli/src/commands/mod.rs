//! Command tree (kanari-style): `mika-cli <group> <verb> [flags]`.

pub mod committee;
pub mod network;
pub mod storage;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mika-cli")]
#[command(about = "Mika decentralized storage CLI (P2P-only nodes)", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level groups, kanari-style: `mika-cli <group> <verb> [flags]`.
#[derive(Subcommand)]
pub enum Commands {
    /// Storage operations (upload/download/proof/anchor/buy/reclaim/upgrade)
    Storage {
        #[command(subcommand)]
        cmd: StorageCmd,
    },
    /// Network operations (start a storage node)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    /// Committee registry (Walrus system-object analogue)
    Committee {
        #[command(subcommand)]
        cmd: CommitteeCmd,
    },
}

#[derive(Subcommand)]
pub enum StorageCmd {
    /// Upload a file (Walrus-style: store → certify → PoA)
    Upload {
        /// -f, --file: path to the file to upload
        #[arg(short = 'f', long)]
        file: PathBuf,
        /// -n, --nodes: entry peer (full multiaddr or `host:port`, e.g. 127.0.0.1:34001)
        #[arg(short = 'n', long)]
        nodes: String,
        /// Number of data shards (k)
        #[arg(long, default_value_t = 3)]
        data_shards: usize,
        /// Number of parity shards (m)
        #[arg(long, default_value_t = 2)]
        parity_shards: usize,
        /// Storage fee paid on-chain (in mist)
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        /// Blob flavor: binary (default) or pq (Dilithium-signed)
        #[arg(long, default_value = "binary")]
        blob_type: String,
        /// -p, --password: keystore password (required when anchoring on-chain)
        #[arg(short = 'p', long)]
        password: Option<String>,
        /// Storage object ID on-chain (required when anchoring on-chain)
        #[arg(long)]
        storage_object_id: Option<String>,
        /// Committee object id (required when anchoring on-chain; PoA gate)
        #[arg(long)]
        committee_id: Option<String>,
        /// --no-anchor: skip on-chain certify (anchor) — offline dev mode
        #[arg(long, default_value_t = false)]
        no_anchor: bool,
    },
    /// Download a file from the entry peer (it keeps every shard)
    Download {
        /// Blob ID (hex-encoded) of the file to download
        blob_id: String,
        /// -o, --output: output file path
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// -n, --nodes: entry peer (full multiaddr or `host:port`, e.g. 127.0.0.1:34001)
        #[arg(short = 'n', long)]
        nodes: String,
        /// Number of data shards (k)
        #[arg(long, default_value_t = 3)]
        data_shards: usize,
        /// Number of parity shards (m)
        #[arg(long, default_value_t = 2)]
        parity_shards: usize,
    },
    /// Get a storage proof for a sliver (via the entry peer)
    Proof {
        /// Blob ID (hex-encoded)
        blob_id: String,
        /// Shard index
        shard_index: u32,
        /// -n, --nodes: entry peer (full multiaddr or `host:port`, e.g. 127.0.0.1:34001)
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
        /// -p, --password: keystore password for decrypting the signing key
        #[arg(short = 'p', long)]
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
pub enum NetworkCmd {
    /// Start a Mika storage node (P2P-only)
    Start {
        /// --node-id: node identifier
        #[arg(long, default_value = "node-1")]
        node_id: String,
        /// --data-dir: directory for the fjall sliver store
        #[arg(long, default_value = "./mika-data")]
        data_dir: PathBuf,
        /// --no-p2p: disable P2P control plane (offline peer)
        #[arg(long, default_value_t = false)]
        no_p2p: bool,
        /// --p2p-listen: libp2p listen port (0 = random; multiaddr is logged)
        #[arg(long, default_value_t = 0)]
        p2p_listen: u16,
        /// --bootstrap: bootstrap multiaddr (repeatable)
        #[arg(long)]
        bootstrap: Vec<String>,
        /// --committee-signers: committee snapshot file (`0x` per line)
        #[arg(long)]
        committee_signers: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum CommitteeCmd {
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
