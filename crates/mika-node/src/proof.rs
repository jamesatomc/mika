//! Storage proofs: a node proves it still holds a sliver by returning a
//! BLAKE3 digest over the sliver bytes plus its shard index.

use mika_storage::blob::BlobID;
use mika_storage::error::Result;
use serde::{Deserialize, Serialize};

/// A storage node's proof that it holds a given sliver at a given epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageProof {
    pub blob_id: BlobID,
    pub shard_index: u32,
    pub node_id: String,
    pub epoch: u64,
    pub sliver_digest: [u8; 32],
}

impl StorageProof {
    pub fn new(
        blob_id: BlobID,
        shard_index: u32,
        node_id: &str,
        epoch: u64,
        sliver: &[u8],
    ) -> Self {
        let digest = kanari_crypto::hash_data_blake3_array(sliver);
        Self {
            blob_id,
            shard_index,
            node_id: node_id.to_string(),
            epoch,
            sliver_digest: digest,
        }
    }

    pub fn verify(&self, sliver: &[u8]) -> Result<()> {
        let digest = kanari_crypto::hash_data_blake3_array(sliver);
        let mut expected = [0u8; 32];
        expected.copy_from_slice(&digest);
        if expected != self.sliver_digest {
            return Err(mika_storage::error::MikaStorageError::Crypto(
                "sliver digest mismatch in storage proof".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_verify_round_trip() {
        let blob_id = BlobID::from_content(b"proof test");
        let sliver = b"some sliver bytes";
        let proof = StorageProof::new(blob_id, 0, "node-1", 1, sliver);
        assert!(proof.verify(sliver).is_ok());
        let bad = b"tampered sliver bytes";
        assert!(proof.verify(bad).is_err());
    }
}
