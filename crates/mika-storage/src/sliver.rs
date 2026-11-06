//! A `Sliver` is a single erasure-coded fragment of a blob destined for one
//! storage node.

use crate::blob::BlobID;
use crate::error::{MikaStorageError, Result};
use serde::{Deserialize, Serialize};

/// One erasure-coded fragment stored on a single storage node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sliver {
    /// The blob this sliver belongs to.
    pub blob_id: BlobID,
    /// Index of this shard within the erasure-coded set (0..total_shards).
    pub shard_index: u32,
    /// Total number of shards in the set (data + parity).
    pub total_shards: u32,
    /// Number of data shards (k).
    pub data_shards: u32,
    /// Number of parity shards (m).
    pub parity_shards: u32,
    /// Original (unpadded) length of the blob, used to trim on reassembly.
    pub original_len: u64,
    /// The raw shard bytes.
    pub data: Vec<u8>,
}

impl Sliver {
    pub fn new(
        blob_id: BlobID,
        shard_index: u32,
        total_shards: u32,
        data_shards: u32,
        parity_shards: u32,
        original_len: u64,
        data: Vec<u8>,
    ) -> Self {
        Self {
            blob_id,
            shard_index,
            total_shards,
            data_shards,
            parity_shards,
            original_len,
            data,
        }
    }

    /// Serialize this sliver for transport to a storage node.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        bcs::to_bytes(self).map_err(|e| MikaStorageError::Serialization(e.to_string()))
    }

    /// Deserialize a sliver received from a storage node.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        bcs::from_bytes(bytes).map_err(|e| MikaStorageError::Serialization(e.to_string()))
    }

    pub fn is_parity(&self) -> bool {
        self.shard_index as usize >= self.data_shards as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliver_round_trip() {
        let blob_id = crate::blob::BlobID::from_content(b"test blob");
        let sliver = Sliver::new(blob_id, 1, 6, 4, 2, 256, vec![42u8; 64]);
        let bytes = sliver.to_bytes().unwrap();
        let back = Sliver::from_bytes(&bytes).unwrap();
        assert_eq!(back, sliver);
        assert!(!back.is_parity());

        let parity = Sliver::new(blob_id, 4, 6, 4, 2, 256, vec![7u8; 64]);
        assert!(parity.is_parity());
    }
}
