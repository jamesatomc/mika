//! Local erasure-code cache + blob assembly for Mika.
//!
//! The mesh transport lives in `mika-server::p2p` (libp2p request-response);
//! this crate only owns the pure data side: shard splitting/encoding, the
//! fjall-backed local cache, and reassembly from fetched slivers.

use crate::certificate::bft_threshold;
use crate::certificate::{WriteCertificate, WriteCertificateQuorum};
use crate::erasure::{ErasureParams, decode, encode, rejoin_shards, split_into_shards};
use async_trait::async_trait;
use fjall::{Database, Keyspace};
use mika_types::blob::{Blob, BlobID};
use mika_types::epoch::{Lease, current_epoch};
use mika_types::error::{MikaStorageError, Result};
use mika_types::sliver::Sliver;
use std::path::PathBuf;

/// Address of a storage node (host:port) — kept for the local cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeAddr(pub String);

impl NodeAddr {
    pub fn new(addr: impl Into<String>) -> Self {
        Self(addr.into())
    }
}

impl std::fmt::Display for NodeAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Result of storing a sliver on a node.
#[derive(Debug, Clone)]
pub struct StoreResult {
    pub node_id: String,
    pub shard_index: u32,
    pub blob_id: BlobID,
}

/// Result of retrieving a sliver from a node.
pub struct RetrieveResult {
    pub node_id: String,
    pub shard_index: u32,
    pub data: Vec<u8>,
}

/// Asynchronous client interface to Mika storage nodes (local cache side).
#[async_trait]
pub trait StorageClient: Send + Sync {
    /// Store a sliver in the local cache.
    async fn store_sliver(&self, node: &NodeAddr, sliver: &Sliver) -> Result<StoreResult>;
    /// Retrieve a sliver from the local cache.
    async fn retrieve_sliver(
        &self,
        node: &NodeAddr,
        blob_id: &BlobID,
        shard_index: u32,
    ) -> Result<RetrieveResult>;
    /// Check whether a sliver is in the local cache.
    async fn has_sliver(&self, node: &NodeAddr, blob_id: &BlobID, shard_index: u32)
    -> Result<bool>;
}

/// Default fjall-backed client: local cache + erasure assembly. Sliver
/// bytes travel over P2P (see `mika-server::p2p`); the mesh keeps copies
/// in each node's own `SliverStore`.
#[derive(Clone)]
#[allow(dead_code)]
pub struct MikaClient {
    pub nodes: Vec<NodeAddr>,
    db: Database,
    keyspace: Keyspace,
}

impl MikaClient {
    /// Create a client with a temporary on-disk fjall store.
    #[allow(deprecated)]
    pub fn new(nodes: Vec<NodeAddr>) -> Self {
        let dir = tempfile::tempdir()
            .expect("failed to create temp dir")
            .into_path();
        Self::from_dir(nodes, dir)
    }

    /// Create a client that persists its store to `path`, loading any existing
    /// data so uploads and lookups survive across separate CLI invocations.
    pub fn with_store_path(nodes: Vec<NodeAddr>, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let _ = std::fs::create_dir_all(&path);
        Self::from_dir(nodes, path)
    }

    fn from_dir(nodes: Vec<NodeAddr>, dir: PathBuf) -> Self {
        let db = Database::builder(&dir)
            .open()
            .expect("failed to open fjall database");
        let keyspace = db
            .keyspace("slivers", Default::default)
            .expect("failed to open keyspace");
        Self {
            nodes,
            db,
            keyspace,
        }
    }

    /// Encode a sliver key: blob_id (32 bytes) || shard_index (4 bytes LE).
    fn sliver_key(node_id: &str, shard_index: u32) -> Vec<u8> {
        let mut key = node_id.as_bytes().to_vec();
        key.push(0xFF); // separator
        key.extend_from_slice(&shard_index.to_le_bytes());
        key
    }

    /// Erasure-code a blob and cache one sliver per node locally. Transport
    /// is external (P2P push via `mika-server::p2p::push_slivers`); the
    /// certificates collected there are what satisfy the BFT quorum.
    pub async fn put_blob(
        &self,
        blob: &Blob,
        params: &ErasureParams,
        fee: u64,
    ) -> Result<(WriteCertificateQuorum, Lease)> {
        if self.nodes.is_empty() {
            return Err(MikaStorageError::NodeUnavailable(
                "no storage nodes configured".into(),
            ));
        }
        if blob.data.len() as u64 > params.data_shards as u64 * 1024 * 1024 {
            return Err(MikaStorageError::Erasure(
                "blob too large for configured shards".into(),
            ));
        }

        let shards = split_into_shards(&blob.data, params.data_shards)?;
        let encoded = encode(&shards, params)?;

        let mut quorum = WriteCertificateQuorum::new(
            blob.id,
            params.total_shards() as u32,
            params.data_shards as u32,
            params.parity_shards as u32,
            blob.data.len() as u64,
        );

        for (i, shard) in encoded.iter().enumerate() {
            let node = &self.nodes[i % self.nodes.len()];
            let sliver = Sliver::new(
                blob.id,
                i as u32,
                params.total_shards() as u32,
                params.data_shards as u32,
                params.parity_shards as u32,
                blob.data.len() as u64,
                shard.clone(),
            );
            let key = Self::sliver_key(&node.0, i as u32);
            let bytes = sliver.to_bytes()?;
            self.keyspace
                .insert(key, bytes)
                .map_err(|e| MikaStorageError::Io(e.to_string()))?;
            // Unsigned local certificate (cache placeholder).
            quorum.add(WriteCertificate {
                blob_id: blob.id,
                shard_index: i as u32,
                node_id: node.0.clone(),
                epoch: current_epoch(),
                sliver_hash: kanari_crypto::hash_data_blake3_array(shard),
                signature: vec![],
                public_key: String::new(),
                curve_type: "K256".to_string(),
            });
        }

        // BFT quorum check (Walrus): need 2f+1 distinct nodes among the
        // configured committee (f = (n-1)/3) AND at least k shards for decode.
        let distinct = quorum.distinct_nodes();
        let required = params.data_shards.max(bft_threshold(self.nodes.len()));
        let all_signed = quorum.certificates.iter().all(|c| !c.signature.is_empty());
        if all_signed {
            let verified = quorum.verified_count()?;
            if verified < required || distinct < required {
                return Err(MikaStorageError::InsufficientShards {
                    have: verified.min(distinct),
                    need: required,
                });
            }
        } else if distinct < required {
            return Err(MikaStorageError::InsufficientShards {
                have: distinct,
                need: required,
            });
        }

        // Lease TTL is epoch-driven (Walrus): fee buys whole epochs.
        let lease = Lease::new_for_epochs(blob.id, mika_types::epoch::epochs_for_fee(fee), fee);
        Ok((quorum, lease))
    }

    /// Reassemble a blob from slivers held in the local cache.
    pub async fn get_blob(&self, blob_id: &BlobID, params: &ErasureParams) -> Result<Blob> {
        let n = params.total_shards();
        let mut present: Vec<Option<Vec<u8>>> = vec![None; n];
        let mut original_len: Option<usize> = None;

        for (i, present_slot) in present.iter_mut().enumerate().take(n) {
            for node in &self.nodes {
                let key = Self::sliver_key(&node.0, i as u32);
                if let Some(bytes) = self
                    .keyspace
                    .get(key)
                    .map_err(|e| MikaStorageError::Io(e.to_string()))?
                {
                    let sliver = Sliver::from_bytes(&bytes)
                        .map_err(|e| MikaStorageError::Serialization(e.to_string()))?;
                    if sliver.blob_id == *blob_id {
                        *present_slot = Some(sliver.data);
                        if original_len.is_none() {
                            original_len = Some(sliver.original_len as usize);
                        }
                        break;
                    }
                }
            }
        }

        let data_shards = decode(&present, params)?;
        let original_len =
            original_len.unwrap_or_else(|| data_shards.iter().map(|s| s.len()).sum());
        let data = rejoin_shards(&data_shards, original_len);
        Ok(Blob::new(data))
    }
}

#[async_trait::async_trait]
impl StorageClient for MikaClient {
    async fn store_sliver(&self, node: &NodeAddr, sliver: &Sliver) -> Result<StoreResult> {
        let key = Self::sliver_key(&node.0, sliver.shard_index);
        let bytes = sliver.to_bytes()?;
        self.keyspace
            .insert(key, bytes)
            .map_err(|e| MikaStorageError::Io(e.to_string()))?;
        Ok(StoreResult {
            node_id: node.0.clone(),
            shard_index: sliver.shard_index,
            blob_id: sliver.blob_id,
        })
    }

    async fn retrieve_sliver(
        &self,
        node: &NodeAddr,
        blob_id: &BlobID,
        shard_index: u32,
    ) -> Result<RetrieveResult> {
        let key = Self::sliver_key(&node.0, shard_index);
        let bytes = self
            .keyspace
            .get(key)
            .map_err(|e| MikaStorageError::Io(e.to_string()))?
            .ok_or_else(|| MikaStorageError::BlobNotFound(blob_id.to_hex()))?;
        let sliver = Sliver::from_bytes(&bytes)
            .map_err(|e| MikaStorageError::Serialization(e.to_string()))?;
        Ok(RetrieveResult {
            node_id: node.0.clone(),
            shard_index,
            data: sliver.data,
        })
    }

    async fn has_sliver(
        &self,
        node: &NodeAddr,
        blob_id: &BlobID,
        shard_index: u32,
    ) -> Result<bool> {
        let key = Self::sliver_key(&node.0, shard_index);
        match self
            .keyspace
            .get(key)
            .map_err(|e| MikaStorageError::Io(e.to_string()))?
        {
            Some(bytes) => {
                let sliver = Sliver::from_bytes(&bytes)
                    .map_err(|e| MikaStorageError::Serialization(e.to_string()))?;
                Ok(sliver.blob_id == *blob_id)
            }
            None => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_and_get_blob() {
        let nodes = vec![
            NodeAddr::new("node-a:1234".to_string()),
            NodeAddr::new("node-b:1234".to_string()),
            NodeAddr::new("node-c:1234".to_string()),
        ];
        let client = MikaClient::new(nodes);
        let blob = Blob::new(b"hello mika storage".repeat(50).to_vec());
        let params = ErasureParams::new(3, 2).unwrap();
        let (_quorum, _lease) = client.put_blob(&blob, &params, 1000).await.unwrap();
        let fetched = client.get_blob(&blob.id, &params).await.unwrap();
        assert_eq!(fetched.data, blob.data);
    }
}
