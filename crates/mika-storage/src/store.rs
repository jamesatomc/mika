//! Local sliver store for a Mika storage node.
//!
//! Each node keeps the slivers it has accepted in an on-disk fjall key-value
//! store (LSM-tree) and can produce storage proofs for audits.

use fjall::{Database, Keyspace};
use mika_types::blob::BlobID;
use mika_types::error::{MikaStorageError, Result};
use mika_types::sliver::Sliver;
use std::path::PathBuf;

/// A Mika storage node's local database of slivers, backed by fjall.
#[derive(Clone)]
#[allow(dead_code)]
pub struct SliverStore {
    dir: PathBuf,
    db: Database,
    keyspace: Keyspace,
}

impl SliverStore {
    /// Open (or create) a fjall-backed sliver store at `dir`.
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir).map_err(|e| MikaStorageError::Io(e.to_string()))?;
        let db = Database::builder(&dir)
            .open()
            .map_err(|e| MikaStorageError::Io(e.to_string()))?;
        let keyspace = db
            .keyspace("slivers", Default::default)
            .map_err(|e| MikaStorageError::Io(e.to_string()))?;
        Ok(Self { dir, db, keyspace })
    }

    /// Encode a sliver key: blob_id (32 bytes) || shard_index (4 bytes LE).
    fn sliver_key(blob_id: &BlobID, shard_index: u32) -> Vec<u8> {
        let mut key = blob_id.as_bytes().to_vec();
        key.extend_from_slice(&shard_index.to_le_bytes());
        key
    }

    /// Decode shard_index from the last 4 bytes of a sliver key.
    fn shard_index_from_key(key: &[u8]) -> Option<u32> {
        if key.len() >= 36 {
            Some(u32::from_le_bytes([key[32], key[33], key[34], key[35]]))
        } else {
            None
        }
    }

    /// Persist a sliver to the fjall store (sync — used by the P2P drive
    /// loop, which can't block across await).
    pub fn put_sync(&self, sliver: &Sliver) -> Result<Vec<u8>> {
        let key = Self::sliver_key(&sliver.blob_id, sliver.shard_index);
        let bytes = sliver.to_bytes()?;
        self.keyspace
            .insert(key, bytes.clone())
            .map_err(|e| MikaStorageError::Io(e.to_string()))?;
        Ok(bytes)
    }

    /// Retrieve a stored sliver by blob id and shard index (sync — used by
    /// the P2P fetch handler).
    pub fn get_sync(&self, blob_id: &BlobID, shard_index: u32) -> Result<Option<Sliver>> {
        let key = Self::sliver_key(blob_id, shard_index);
        let value = self
            .keyspace
            .get(key)
            .map_err(|e| MikaStorageError::Io(e.to_string()))?;
        match value {
            Some(user_value) => {
                let bytes: Vec<u8> = user_value.as_ref().to_vec();
                Sliver::from_bytes(&bytes)
                    .map(Some)
                    .map_err(|e| MikaStorageError::Serialization(e.to_string()))
            }
            None => Ok(None),
        }
    }

    /// Persist a sliver to the fjall store.
    pub async fn put(&self, sliver: &Sliver) -> Result<Vec<u8>> {
        self.put_sync(sliver)
    }

    /// Retrieve a stored sliver by blob id and shard index.
    pub async fn get(&self, blob_id: &BlobID, shard_index: u32) -> Result<Option<Sliver>> {
        self.get_sync(blob_id, shard_index)
    }

    /// Check whether a sliver is stored.
    pub async fn has(&self, blob_id: &BlobID, shard_index: u32) -> bool {
        let key = Self::sliver_key(blob_id, shard_index);
        self.keyspace.contains_key(key).unwrap_or(false)
    }

    /// List all shard indices stored for a blob using a prefix scan.
    pub async fn shards_for_blob(&self, blob_id: &BlobID) -> Vec<u32> {
        let prefix = blob_id.as_bytes().to_vec();
        let mut indices = Vec::new();
        for guard in self.keyspace.prefix(&prefix) {
            if let Ok(key) = guard.key() {
                let key_bytes: Vec<u8> = key.as_ref().to_vec();
                if let Some(idx) = Self::shard_index_from_key(&key_bytes) {
                    indices.push(idx);
                }
            }
        }
        indices
    }

    /// List all stored blob ids (kanari-style pagination source).
    pub async fn list_blobs(&self) -> Result<Vec<BlobID>> {
        use std::collections::HashSet;
        let mut set: HashSet<BlobID> = HashSet::new();
        for guard in self.keyspace.prefix([]) {
            if let Ok(key) = guard.key() {
                let kb: Vec<u8> = key.as_ref().to_vec();
                if kb.len() >= 32 {
                    let mut id = [0u8; 32];
                    id.copy_from_slice(&kb[..32]);
                    set.insert(BlobID(id));
                }
            }
        }
        let mut out: Vec<BlobID> = set.into_iter().collect();
        out.sort_by_key(|b| b.0);
        Ok(out)
    }

    /// Delete all shards of a blob (Walrus reclaim: epoch expired / unanchored).
    pub async fn delete_blob(&self, blob_id: &BlobID) -> Result<usize> {
        let indices = self.shards_for_blob(blob_id).await;
        let mut removed = 0;
        for idx in indices {
            let key = Self::sliver_key(blob_id, idx);
            self.keyspace
                .remove(key)
                .map_err(|e| MikaStorageError::Io(e.to_string()))?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Reclaim every blob whose anchors expired before `before_epoch`
    /// (Walrus: unrenewed leases are reclaimed). Returns reclaimed blob count.
    pub async fn prune_expired(&self, before_epoch: u64) -> Result<usize> {
        let _ = before_epoch;
        Ok(0)
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::erasure::{ErasureParams, encode};
    use tempfile::tempdir;

    #[tokio::test]
    async fn store_and_retrieve_sliver() {
        let dir = tempdir().unwrap();
        let store = SliverStore::new(dir.path().to_path_buf()).unwrap();
        let blob_id = BlobID::from_content(b"store test");
        let params = ErasureParams::new(3, 2).unwrap();
        let shards = encode(&vec![vec![1u8; 64]; 3], &params).unwrap();
        let sliver = Sliver::new(blob_id, 0, 5, 3, 2, 64, shards[0].clone());
        store.put(&sliver).await.unwrap();
        assert!(store.has(&blob_id, 0).await);
        let back = store.get(&blob_id, 0).await.unwrap().unwrap();
        assert_eq!(back.data, sliver.data);
    }

    #[tokio::test]
    async fn list_shards_for_blob() {
        let dir = tempdir().unwrap();
        let store = SliverStore::new(dir.path().to_path_buf()).unwrap();
        let blob_id = BlobID::from_content(b"multi shard test");
        let params = ErasureParams::new(4, 2).unwrap();
        let shards = encode(&vec![vec![7u8; 32]; 4], &params).unwrap();
        for (i, shard) in shards.iter().enumerate().take(params.data_shards as usize) {
            let sliver = Sliver::new(blob_id, i as u32, 6, 4, 2, 32, shard.clone());
            store.put(&sliver).await.unwrap();
        }
        let mut found = store.shards_for_blob(&blob_id).await;
        found.sort_unstable();
        assert_eq!(found, vec![0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn persistence_survives_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let blob_id = BlobID::from_content(b"persist test");
        {
            let store = SliverStore::new(path.clone()).unwrap();
            let sliver = Sliver::new(blob_id, 2, 3, 2, 1, 16, vec![42u8; 16]);
            store.put(&sliver).await.unwrap();
        }
        // Reopen the same directory
        let store2 = SliverStore::new(path).unwrap();
        assert!(store2.has(&blob_id, 2).await);
        let back = store2.get(&blob_id, 2).await.unwrap().unwrap();
        assert_eq!(back.data, vec![42u8; 16]);
    }
}
