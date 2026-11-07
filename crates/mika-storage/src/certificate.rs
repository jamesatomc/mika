//! Write and read certificates.
//!
//! Storage nodes sign certificates attesting that they have received (write) or
//! served (read) a sliver. These are anchored on-chain to prove data
//! availability and to authorize ownership transfers.

use crate::blob::BlobID;
use crate::error::{MikaStorageError, Result};
use kanari_crypto::CurveType;
use kanari_crypto::hash_data_blake3_array;
use kanari_crypto::signatures::{sign_message, verify_signature_with_curve};
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
    pub public_key: String,
    pub curve_type: String,
}

impl WriteCertificate {
    /// Sign this certificate with the given private key hex and curve type.
    pub fn sign(&mut self, private_key_hex: &str, curve_type: CurveType) -> Result<()> {
        let message = self.cert_message();
        let sig = sign_message(private_key_hex, &message, curve_type)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;
        self.signature = sig;
        self.curve_type = format!("{:?}", curve_type);
        Ok(())
    }

    /// Verify this certificate's signature against its public key and curve type.
    pub fn verify(&self) -> Result<bool> {
        if self.signature.is_empty() {
            return Ok(false);
        }
        let message = self.cert_message();
        let curve: CurveType = self
            .curve_type
            .parse()
            .map_err(|_| MikaStorageError::Crypto("invalid curve type".into()))?;
        verify_signature_with_curve(&self.public_key, &message, &self.signature, curve)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))
    }

    /// Serialize the certificate fields (excluding signature and public_key)
    /// into a deterministic byte message for signing/verification.
    fn cert_message(&self) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(self.blob_id.as_bytes());
        msg.extend_from_slice(&self.shard_index.to_le_bytes());
        msg.extend_from_slice(self.node_id.as_bytes());
        msg.extend_from_slice(&self.epoch.to_le_bytes());
        msg.extend_from_slice(&self.sliver_hash);
        msg
    }
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

    /// Sign all certificates in this quorum with the given private key and curve.
    pub fn sign_all(&mut self, private_key_hex: &str, curve_type: CurveType) -> Result<()> {
        for cert in self.certificates.iter_mut() {
            cert.sign(private_key_hex, curve_type)?;
        }
        Ok(())
    }

    /// Verify all certificates in this quorum.
    pub fn verify_all(&self) -> Result<bool> {
        for cert in self.certificates.iter() {
            if !cert.verify()? {
                return Ok(false);
            }
        }
        Ok(true)
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

    /// Count verified certificates.
    pub fn verified_count(&self) -> Result<usize> {
        let mut count = 0;
        for cert in self.certificates.iter() {
            if cert.verify()? {
                count += 1;
            }
        }
        Ok(count)
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
    use kanari_crypto::{CurveType, generate_keypair};

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
            public_key: String::new(),
            curve_type: String::new(),
        });
        q.add(WriteCertificate {
            blob_id,
            shard_index: 1,
            node_id: "node-b".into(),
            epoch: 1,
            sliver_hash: [2u8; 32],
            signature: vec![],
            public_key: String::new(),
            curve_type: String::new(),
        });
        q.add(WriteCertificate {
            blob_id,
            shard_index: 2,
            node_id: "node-c".into(),
            epoch: 1,
            sliver_hash: [3u8; 32],
            signature: vec![],
            public_key: String::new(),
            curve_type: String::new(),
        });
        assert!(q.has_quorum(3));
    }

    #[test]
    fn sign_and_verify_certificate() {
        let blob_id = BlobID::from_content(b"sign test");
        let keypair = generate_keypair(CurveType::K256).unwrap();
        let mut cert = WriteCertificate {
            blob_id,
            shard_index: 0,
            node_id: "node-1".into(),
            epoch: 42,
            sliver_hash: [9u8; 32],
            signature: vec![],
            public_key: keypair.public_key.clone(),
            curve_type: "K256".to_string(),
        };
        assert!(!cert.verify().unwrap());
        cert.sign(&keypair.private_key, CurveType::K256).unwrap();
        assert!(cert.verify().unwrap());
    }

    #[test]
    fn sign_and_verify_quorum() {
        let blob_id = BlobID::from_content(b"quorum sign test");
        let keypair = generate_keypair(CurveType::K256).unwrap();
        let mut q = WriteCertificateQuorum::new(blob_id, 3, 2, 1, 100);
        q.add(WriteCertificate {
            blob_id,
            shard_index: 0,
            node_id: "node-1".into(),
            epoch: 1,
            sliver_hash: [1u8; 32],
            signature: vec![],
            public_key: keypair.public_key.clone(),
            curve_type: "K256".to_string(),
        });
        q.sign_all(&keypair.private_key, CurveType::K256).unwrap();
        assert!(q.verify_all().unwrap());
        assert_eq!(q.verified_count().unwrap(), 1);
    }
}
