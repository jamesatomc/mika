//! Local sliver store for a Mika storage node.
//!
//! Each node keeps the slivers it has accepted in an on-disk key-value store
//! (backed by a temporary directory in this scaffold) and can produce storage
//! proofs for audits.

use mika_storage::blob::BlobID;
use mika_storage::error::Result;
use mika_storage::sliver::Sliver;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory record of a stored sliver plus its on-disk path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSliver {
    pub sliver: Sliver,
    pub path: PathBuf,
}

/// A Mika storage node's local database of slivers.
#[derive(Debug, Clone)]
pub struct SliverStore {
    dir: PathBuf,
    index: Arc<RwLock<HashMap<(BlobID, u32), StoredSliver>>>,
}

impl SliverStore {
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| mika_storage::error::MikaStorageError::Io(e.to_string()))?;
        Ok(Self {
            dir,
            index: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Persist a sliver to disk and update the in-memory index.
    pub async fn put(&self, sliver: &Sliver) -> Result<PathBuf> {
        let file_name = format!("{}_{}.sliver", sliver.blob_id.to_hex(), sliver.shard_index);
        let path = self.dir.join(file_name);
        let bytes = sliver.to_bytes()?;
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| mika_storage::error::MikaStorageError::Io(e.to_string()))?;

        let stored = StoredSliver {
            sliver: sliver.clone(),
            path: path.clone(),
        };
        self.index
            .write()
            .await
            .insert((sliver.blob_id, sliver.shard_index), stored);
        Ok(path)
    }

    /// Retrieve a stored sliver by blob id and shard index.
    pub async fn get(&self, blob_id: &BlobID, shard_index: u32) -> Result<Option<Sliver>> {
        let index = self.index.read().await;
        Ok(index
            .get(&(*blob_id, shard_index))
            .map(|s| s.sliver.clone()))
    }

    /// Check whether a sliver is stored.
    pub async fn has(&self, blob_id: &BlobID, shard_index: u32) -> bool {
        self.index
            .read()
            .await
            .contains_key(&(*blob_id, shard_index))
    }

    /// List all shard indices stored for a blob.
    pub async fn shards_for_blob(&self, blob_id: &BlobID) -> Vec<u32> {
        let index = self.index.read().await;
        index
            .keys()
            .filter_map(|(bid, idx)| if *bid == *blob_id { Some(*idx) } else { None })
            .collect()
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_storage::blob::BlobID;
    use mika_storage::erasure::{ErasureParams, encode};
    use mika_storage::sliver::Sliver;
    use tempfile::tempdir;

    #[tokio::test]
    async fn store_and_retrieve_sliver() {
        let dir = tempdir().unwrap();
        let store = SliverStore::new(dir.path().to_path_buf()).unwrap();
        let blob_id = BlobID::from_content(b"store test");
        let params = ErasureParams::new(3, 2).unwrap();
        let shards = encode(&[vec![1u8; 64]], &params).unwrap();
        let sliver = Sliver::new(blob_id, 0, 5, 3, 2, 64, shards[0].clone());
        store.put(&sliver).await.unwrap();
        assert!(store.has(&blob_id, 0).await);
        let back = store.get(&blob_id, 0).await.unwrap().unwrap();
        assert_eq!(back.data, sliver.data);
    }
}
