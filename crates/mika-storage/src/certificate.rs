//! Write and read certificates.
//!
//! Storage nodes sign certificates attesting that they have received (write) or
//! served (read) a sliver. These are anchored on-chain to prove data
//! availability and to authorize ownership transfers.

use crate::blob::BlobID;
use crate::error::{MikaStorageError, Result};
use kanari_crypto::hash_data_blake3_array;
use serde::{Deserialize, Serialize};

/// A storage node's attestation that it has stored a given sliver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteCertificate {
    pub blob_id: BlobID,
    pub shard_index: u32,
    pub node_id: String,
    pub epoch: crate::epoch::Epoch,
    pub sliver_hash: [u8; 32],
    pub signature: Vec<u8>,
}

/// A quorum of write certificates for one blob, sufficient to anchor it on-chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteCertificateQuorum {
    pub blob_id: BlobID,
    pub certificates: Vec<WriteCertificate>,
    pub total_shards: u32,
    pub data_shards: u32,
    pub parity_shards: u32,
    pub original_len: u64,
}

impl WriteCertificateQuorum {
    pub fn new(
        blob_id: BlobID,
        total_shards: u32,
        data_shards: u32,
        parity_shards: u32,
        original_len: u64,
    ) -> Self {
        Self {
            blob_id,
            certificates: Vec::new(),
            total_shards,
            data_shards,
            parity_shards,
            original_len,
        }
    }

    pub fn add(&mut self, cert: WriteCertificate) {
        self.certificates.push(cert);
    }

    pub fn distinct_nodes(&self) -> usize {
        self.certificates
            .iter()
            .map(|c| c.node_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    pub fn has_quorum(&self, required: usize) -> bool {
        self.distinct_nodes() >= required
    }
}

/// Proof that a client retrieved a blob's slivers from storage nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadCertificate {
    pub blob_id: BlobID,
    pub shard_indices: Vec<u32>,
    pub reconstructed_hash: [u8; 32],
}

impl ReadCertificate {
    pub fn verify_reconstruction(&self, data: &[u8]) -> Result<()> {
        let digest = hash_data_blake3_array(data);
        if digest != self.reconstructed_hash {
            return Err(MikaStorageError::InvalidReadCertificate(
                "reconstructed hash does not match certificate".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quorum_tracking() {
        let blob_id = BlobID::from_content(b"quorum test");
        let mut q = WriteCertificateQuorum::new(blob_id, 6, 4, 2, 256);
        assert!(!q.has_quorum(3));
        q.add(WriteCertificate {
            blob_id,
            shard_index: 0,
            node_id: "node-a".into(),
            epoch: 1,
            sliver_hash: [1u8; 32],
            signature: vec![],
        });
        q.add(WriteCertificate {
            blob_id,
            shard_index: 1,
            node_id: "node-b".into(),
            epoch: 1,
            sliver_hash: [2u8; 32],
            signature: vec![],
        });
        assert!(!q.has_quorum(3));
        q.add(WriteCertificate {
            blob_id,
            shard_index: 2,
            node_id: "node-c".into(),
            epoch: 1,
            sliver_hash: [3u8; 32],
            signature: vec![],
        });
        assert!(q.has_quorum(3));
    }
}
