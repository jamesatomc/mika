//! Client for uploading and downloading blobs to/from Mika storage nodes.
//!
//! The client erasure-codes a blob into slivers, distributes them to storage
//! nodes, collects write certificates, and can later reassemble a blob from the
//! slivers.

use crate::blob::{Blob, BlobID};
use crate::certificate::{WriteCertificate, WriteCertificateQuorum};
use crate::epoch::{Lease, current_epoch};
use crate::erasure::{ErasureParams, decode, encode, rejoin_shards, split_into_shards};
use crate::error::{MikaStorageError, Result};
use crate::sliver::Sliver;
use async_trait::async_trait;
use std::collections::HashMap;

/// Address of a storage node (host:port).
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

/// Asynchronous client interface to Mika storage nodes.
#[async_trait]
pub trait StorageClient: Send + Sync {
    /// Store a sliver on a node and return a write certificate.
    async fn store_sliver(&self, node: &NodeAddr, sliver: &Sliver) -> Result<StoreResult>;
    /// Retrieve a sliver from a node.
    async fn retrieve_sliver(
        &self,
        node: &NodeAddr,
        blob_id: &BlobID,
        shard_index: u32,
    ) -> Result<RetrieveResult>;
    /// Check whether a node holds a sliver.
    async fn has_sliver(&self, node: &NodeAddr, blob_id: &BlobID, shard_index: u32)
    -> Result<bool>;
}

/// Default in-memory / HTTP-backed client implementation.
#[derive(Debug, Clone)]
pub struct MikaClient {
    pub nodes: Vec<NodeAddr>,
    /// Simulated remote storage: node_id -> (blob_id, shard_index) -> sliver bytes.
    /// In production this would be real HTTP calls to each node.
    store: std::sync::Arc<tokio::sync::RwLock<HashMap<(String, u32), Sliver>>>,
}

impl MikaClient {
    pub fn new(nodes: Vec<NodeAddr>) -> Self {
        Self {
            nodes,
            store: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Erasure-code a blob and distribute its slivers across the configured nodes.
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

        let mut store = self.store.write().await;
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
            store.insert((node.0.clone(), i as u32), sliver);
            quorum.add(WriteCertificate {
                blob_id: blob.id,
                shard_index: i as u32,
                node_id: node.0.clone(),
                epoch: current_epoch(),
                sliver_hash: kanari_crypto::hash_data_blake3_array(shard),
                signature: vec![], // placeholder; real client signs with its key
            });
        }
        drop(store);

        let lease = Lease::new(blob.id, 86400, fee);
        Ok((quorum, lease))
    }

    /// Retrieve and reassemble a blob from storage nodes.
    pub async fn get_blob(&self, blob_id: &BlobID, params: &ErasureParams) -> Result<Blob> {
        let n = params.total_shards();
        let mut present: Vec<Option<Vec<u8>>> = vec![None; n];
        let store = self.store.read().await;
        for (i, present_slot) in present.iter_mut().enumerate().take(n) {
            // Try each node until we find the shard.
            for node in &self.nodes {
                if let Some(sliver) = store.get(&(node.0.clone(), i as u32))
                    && sliver.blob_id == *blob_id
                {
                    *present_slot = Some(sliver.data.clone());
                    break;
                }
            }
        }
        drop(store);

        let data_shards = decode(&present, params)?;
        let data = rejoin_shards(&data_shards, data_shards.iter().map(|s| s.len()).sum());
        Ok(Blob::new(data))
    }
}

#[async_trait::async_trait]
impl StorageClient for MikaClient {
    async fn store_sliver(&self, node: &NodeAddr, sliver: &Sliver) -> Result<StoreResult> {
        let mut store = self.store.write().await;
        store.insert((node.0.clone(), sliver.shard_index), sliver.clone());
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
        let store = self.store.read().await;
        let sliver = store
            .get(&(node.0.clone(), shard_index))
            .filter(|&s| s.blob_id == *blob_id)
            .ok_or_else(|| MikaStorageError::BlobNotFound(blob_id.to_hex()))?;
        Ok(RetrieveResult {
            node_id: node.0.clone(),
            shard_index,
            data: sliver.data.clone(),
        })
    }

    async fn has_sliver(
        &self,
        node: &NodeAddr,
        blob_id: &BlobID,
        shard_index: u32,
    ) -> Result<bool> {
        let store = self.store.read().await;
        Ok(store
            .get(&(node.0.clone(), shard_index))
            .map(|s| s.blob_id == *blob_id)
            .unwrap_or(false))
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
