//! Client for uploading and downloading blobs to/from Mika storage nodes.
//!
//! The client erasure-codes a blob into slivers, distributes them to storage
//! nodes, collects write certificates, and can later reassemble a blob from
//! the slivers. Slivers are persisted in an on-disk fjall key-value store so
//! uploads survive across separate CLI invocations.

use crate::certificate::{WriteCertificate, WriteCertificateQuorum};
use crate::erasure::{ErasureParams, decode, encode, rejoin_shards, split_into_shards};
use async_trait::async_trait;
use fjall::{Database, Keyspace};
use mika_types::blob::{Blob, BlobID};
use mika_types::epoch::{Lease, current_epoch};
use mika_types::error::{MikaStorageError, Result};
use mika_types::sliver::Sliver;
use std::path::PathBuf;

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

/// Default fjall-backed client implementation.
///
/// Slivers are persisted in an on-disk fjall LSM-tree keyed by
/// `blob_id (32 bytes) || shard_index (4 bytes LE)`.
///
/// **Walrus dissemination:** `put_blob` / `get_blob` / `store_sliver` also
/// replicate over HTTP to the configured nodes (`PUT /slivers/...`,
/// `GET /slivers/...`). Local fjall remains as cache/offline fallback so
/// single-node dev and unit tests (fake `node-a:1234` hosts) keep working.
/// Node URLs without a scheme default to `http://{host}:{port}`.
#[derive(Clone)]
#[allow(dead_code)]
pub struct MikaClient {
    pub nodes: Vec<NodeAddr>,
    db: Database,
    keyspace: Keyspace,
    #[cfg(feature = "onchain")]
    http: reqwest::Client,
}

impl MikaClient {
    /// Create a client with a temporary on-disk fjall store.
    #[allow(deprecated)]
    pub fn new(nodes: Vec<NodeAddr>) -> Self {
        let dir = tempfile::tempdir()
            .expect("failed to create temp dir")
            .into_path();
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
            #[cfg(feature = "onchain")]
            http: reqwest::Client::new(),
        }
    }

    /// Create a client that persists its store to `path`, loading any existing
    /// data so uploads and lookups survive across separate CLI invocations.
    pub fn with_store_path(nodes: Vec<NodeAddr>, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let _ = std::fs::create_dir_all(&path);
        let db = Database::builder(&path)
            .open()
            .expect("failed to open fjall database");
        let keyspace = db
            .keyspace("slivers", Default::default)
            .expect("failed to open keyspace");
        Self {
            nodes,
            db,
            keyspace,
            #[cfg(feature = "onchain")]
            http: reqwest::Client::new(),
        }
    }

    /// Normalize a node address to an HTTP base URL (`http://host:port`).
    #[cfg(feature = "onchain")]
    fn node_url(node: &NodeAddr) -> String {
        let s = node.0.trim();
        if s.starts_with("http://") || s.starts_with("https://") {
            s.trim_end_matches('/').to_string()
        } else {
            format!("http://{s}")
        }
    }

    /// Try `PUT /slivers/{blob_id}/{idx}` on the node. Returns true on success.
    #[cfg(feature = "onchain")]
    async fn http_put(&self, node: &NodeAddr, sliver: &Sliver) -> bool {
        let url = format!(
            "{}/slivers/{}/{}",
            Self::node_url(node),
            sliver.blob_id.to_hex(),
            sliver.shard_index
        );
        match self
            .http
            .put(&url)
            .header("content-type", "application/octet-stream")
            .body(sliver.data.clone())
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => true,
            Ok(resp) => {
                eprintln!("mika: PUT {url} -> {}", resp.status());
                false
            }
            Err(e) => {
                eprintln!("mika: PUT {url} failed ({e}) — local cache only");
                false
            }
        }
    }

    #[cfg(feature = "onchain")]
    async fn http_get(&self, node: &NodeAddr, blob_id: &BlobID, shard: u32) -> Option<Vec<u8>> {
        let url = format!(
            "{}/slivers/{}/{}",
            Self::node_url(node),
            blob_id.to_hex(),
            shard
        );
        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => resp.bytes().await.ok().map(|b| b.to_vec()),
            _ => None,
        }
    }

    /// Encode a sliver key: blob_id (32 bytes) || shard_index (4 bytes LE).
    fn sliver_key(node_id: &str, shard_index: u32) -> Vec<u8> {
        let mut key = node_id.as_bytes().to_vec();
        key.push(0xFF); // separator
        key.extend_from_slice(&shard_index.to_le_bytes());
        key
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
            // Walrus dissemination: replicate to the node's HTTP store too.
            #[cfg(feature = "onchain")]
            let _ = self.http_put(node, &sliver).await;
            quorum.add(WriteCertificate {
                blob_id: blob.id,
                shard_index: i as u32,
                node_id: node.0.clone(),
                epoch: current_epoch(),
                sliver_hash: kanari_crypto::hash_data_blake3_array(shard),
                signature: vec![], // placeholder; real client signs with its key
                public_key: String::new(),
                curve_type: "K256".to_string(),
            });
        }

        // Lease TTL is epoch-driven (Walrus): fee buys whole epochs.
        let lease = Lease::new_for_epochs(blob.id, mika_types::epoch::epochs_for_fee(fee), fee);
        Ok((quorum, lease))
    }

    /// Retrieve and reassemble a blob from storage nodes.
    pub async fn get_blob(&self, blob_id: &BlobID, params: &ErasureParams) -> Result<Blob> {
        let n = params.total_shards();
        let mut present: Vec<Option<Vec<u8>>> = vec![None; n];
        let mut original_len: Option<usize> = None;

        for (i, present_slot) in present.iter_mut().enumerate().take(n) {
            // Try each node until we find the shard (Walrus: HTTP first,
            // local fjall cache second).
            for node in &self.nodes {
                #[cfg(feature = "onchain")]
                if let Some(bytes) = self.http_get(node, blob_id, i as u32).await
                    && let Ok(sliver) = Sliver::from_bytes(&bytes)
                    && sliver.blob_id == *blob_id
                {
                    *present_slot = Some(sliver.data.clone());
                    if original_len.is_none() {
                        original_len = Some(sliver.original_len as usize);
                    }
                    break;
                }
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
        #[cfg(feature = "onchain")]
        let _ = self.http_put(node, sliver).await;
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
        #[cfg(feature = "onchain")]
        if let Some(bytes) = self.http_get(node, blob_id, shard_index).await {
            let sliver = Sliver::from_bytes(&bytes)
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?;
            return Ok(RetrieveResult {
                node_id: node.0.clone(),
                shard_index,
                data: sliver.data,
            });
        }
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
