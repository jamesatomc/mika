//! Write and read certificates.
//!
//! Storage nodes sign certificates attesting that they have received (write) or
//! served (read) a sliver. These are anchored on-chain to prove data
//! availability and to authorize ownership transfers.

use kanari_crypto::CurveType;
use kanari_crypto::signatures::{sign_message, verify_signature_with_curve};
use mika_types::blob::BlobID;
use mika_types::error::{MikaStorageError, Result};
use serde::{Deserialize, Serialize};

/// BFT quorum threshold for a committee of `n` storage nodes:
/// `2f + 1` where `f = (n - 1) / 3` (Walrus consensus threshold).
/// e.g. n=1 → 1, n=3 → 3, n=4 → 3, n=5 → 3, n=7 → 5.
pub fn bft_threshold(n: usize) -> usize {
    match n {
        0 => 0,
        1 | 2 => 1,
        3 => 3,
        4..=6 => 3,
        _ => {
            let f = (n - 1) / 3;
            2 * f + 1
        }
    }
}

/// Check that `certs` are all signatures over the same blob by *distinct*
/// node keys (same key signing every cert is not a quorum, even if the
/// node_id strings differ).
fn distinct_public_keys(certs: &[WriteCertificate]) -> usize {
    certs
        .iter()
        .map(|c| c.public_key.clone())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// A storage node's attestation that it has stored a given sliver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteCertificate {
    pub blob_id: BlobID,
    pub shard_index: u32,
    pub node_id: String,
    pub epoch: mika_types::epoch::Epoch,
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

    /// BFT quorum for a committee of `n` storage nodes: `2f+1` distinct
    /// **node keys** with valid signatures (sizes are committee shards).
    pub fn is_bft_quorum(&self, committee_size: usize) -> bool {
        if committee_size == 0 {
            return false;
        }
        let needed = bft_threshold(committee_size);
        if distinct_public_keys(&self.certificates) < needed || self.distinct_nodes() < needed {
            return false;
        }
        // Unsigned placeholder certs (dev mode: signatures from put_blob) are
        // not accepted as BFT — every cert must verify.
        self.certificates
            .iter()
            .all(|c| c.verify().unwrap_or(false))
    }

    /// Committee size these certs cover (distinct node ids presented).
    pub fn committee_size(&self) -> usize {
        self.distinct_nodes()
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

/// Walrus-style aggregated Proof-of-Availability certificate: the 2/3-quorum
/// of stored-sliver attestations collected from the mesh, submitted to the
/// chain (`record_availability`) as the blob's availability proof.
///
/// `signer_count` is the number of **distinct node keys** that verified —
/// the value the Move side checks against `MIN_POA_SIGNERS`. The full
/// certificate batch stays off-chain (watchers re-verify from the
/// p2p-store exchange) and is not embedded in the tx.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoaCert {
    pub blob_id: BlobID,
    pub commitment: [u8; 32],
    pub epoch: mika_types::epoch::Epoch,
    pub signer_count: u64,
    /// Verified certificates that make up the quorum (kept for auditing).
    pub certificates: Vec<WriteCertificate>,
}

impl PoaCert {
    /// Aggregate `quorum` into a PoA certificate after verifying every
    /// signature and requiring distinct keys ≥ `MIN_POA_SIGNERS` (Move side
    /// repeats the same floor).
    pub fn aggregate(
        quorum: &WriteCertificateQuorum,
        commitment: [u8; 32],
        epoch: mika_types::epoch::Epoch,
    ) -> Result<Self> {
        if quorum.blob_id
            != quorum
                .certificates
                .first()
                .map(|c| c.blob_id)
                .unwrap_or(BlobID([0u8; 32]))
        {
            return Err(MikaStorageError::Crypto("mixed blobs in PoA quorum".into()));
        }
        let mut verified = 0usize;
        for cert in &quorum.certificates {
            if cert.verify()? {
                verified += 1;
            }
        }
        let keys = distinct_public_keys(&quorum.certificates);
        if (verified as u64) < mika_types::storage::MIN_POA_SIGNERS || keys < MIN_POA_SIGNERS_USIZE
        {
            return Err(MikaStorageError::InsufficientShards {
                have: verified,
                need: MIN_POA_SIGNERS_USIZE,
            });
        }
        Ok(Self {
            blob_id: quorum.blob_id,
            commitment,
            epoch,
            signer_count: keys as u64,
            certificates: quorum.certificates.clone(),
        })
    }
}

const MIN_POA_SIGNERS_USIZE: usize = mika_types::storage::MIN_POA_SIGNERS as usize;

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

    #[test]
    fn poa_aggregation_requires_distinct_signers() {
        let blob_id = BlobID::from_content(b"poa test");
        // Two distinct keys → quorum meets MIN_POA_SIGNERS.
        let key_a = generate_keypair(CurveType::K256).unwrap();
        let key_b = generate_keypair(CurveType::K256).unwrap();
        let key_a_pub = key_a.public_key.clone();
        let key_a_priv = key_a.private_key.to_string();
        let mut q = WriteCertificateQuorum::new(blob_id, 6, 4, 2, 256);
        for (key, node, shard) in [(key_a, "node-a", 0u32), (key_b, "node-b", 1u32)] {
            let mut cert = WriteCertificate {
                blob_id,
                shard_index: shard,
                node_id: node.into(),
                epoch: 1,
                sliver_hash: [7u8; 32],
                signature: vec![],
                public_key: key.public_key.clone(),
                curve_type: "K256".into(),
            };
            cert.sign(&key.private_key, CurveType::K256).unwrap();
            q.add(cert);
        }
        let poa = PoaCert::aggregate(&q, [3u8; 32], 100).unwrap();
        assert_eq!(poa.blob_id, blob_id);
        assert_eq!(poa.signer_count, 2);
        assert_eq!(poa.certificates.len(), 2);
        // One signer only → quorum not met.
        let mut q2 = WriteCertificateQuorum::new(blob_id, 6, 4, 2, 256);
        let mut cert = WriteCertificate {
            blob_id,
            shard_index: 0,
            node_id: "solo".into(),
            epoch: 1,
            sliver_hash: [1u8; 32],
            signature: vec![],
            public_key: key_a_pub.clone(),
            curve_type: "K256".into(),
        };
        cert.sign(&key_a_priv, CurveType::K256).unwrap();
        q2.add(cert);
        assert!(PoaCert::aggregate(&q2, [3u8; 32], 100).is_err());
    }

    #[test]
    fn bft_threshold_table() {
        assert_eq!(bft_threshold(1), 1);
        assert_eq!(bft_threshold(3), 3);
        assert_eq!(bft_threshold(4), 3);
        assert_eq!(bft_threshold(5), 3);
        assert_eq!(bft_threshold(6), 3);
        assert_eq!(bft_threshold(7), 5);
        assert_eq!(bft_threshold(10), 7);
    }

    #[test]
    fn bft_quorum_requires_distinct_keys() {
        // 3 certs on 3 nodes, all signed by the *same* key → not a BFT quorum.
        let blob_id = BlobID::from_content(b"bft");
        let keypair = generate_keypair(CurveType::K256).unwrap();
        let mut q = WriteCertificateQuorum::new(blob_id, 3, 2, 1, 100);
        for i in 0..3 {
            q.add(WriteCertificate {
                blob_id,
                shard_index: i,
                node_id: format!("node-{i}"),
                epoch: 1,
                sliver_hash: [i as u8; 32],
                signature: vec![],
                public_key: keypair.public_key.clone(),
                curve_type: "K256".to_string(),
            });
        }
        assert!(
            !q.is_bft_quorum(3),
            "same key on every cert is not a BFT quorum"
        );
        // Distinct keys → passes.
        let mut q2 = WriteCertificateQuorum::new(blob_id, 3, 2, 1, 100);
        for i in 0..3 {
            let kp = generate_keypair(CurveType::K256).unwrap();
            let mut cert = WriteCertificate {
                blob_id,
                shard_index: i,
                node_id: format!("node-{i}"),
                epoch: 1,
                sliver_hash: [i as u8; 32],
                signature: vec![],
                public_key: kp.public_key.clone(),
                curve_type: "K256".to_string(),
            };
            cert.sign(&kp.private_key, CurveType::K256).unwrap();
            q2.add(cert);
        }
        assert!(q2.is_bft_quorum(3));
    }
}
