//! Blob and BlobID types.
//!
//! A `Blob` is the unit of storage in Mika. It is addressed by a content-addressed
//! `BlobID` (BLAKE3 hash of the canonical encoding) so identical content always
//! resolves to the same identifier.

use crate::error::{MikaStorageError, Result};
use serde::{Deserialize, Serialize};

/// Content-addressed identifier for a stored blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobID(pub [u8; 32]);

impl BlobID {
    /// Compute the BlobID for arbitrary content using BLAKE3 via kanari-crypto.
    pub fn from_content(content: &[u8]) -> Self {
        let id = kanari_crypto::hash_data_blake3_array(content);
        Self(id)
    }

    /// Compute the BlobID for a serialized value.
    pub fn from_serialized<T: serde::Serialize>(value: &T) -> Result<Self> {
        let bytes =
            bcs::to_bytes(value).map_err(|e| MikaStorageError::Serialization(e.to_string()))?;
        Ok(Self::from_content(&bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Display for BlobID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl TryFrom<&str> for BlobID {
    type Error = MikaStorageError;

    fn try_from(s: &str) -> Result<Self> {
        let bytes = hex::decode(s).map_err(|e| MikaStorageError::InvalidBlobId(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(MikaStorageError::InvalidBlobId(format!(
                "expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(&bytes);
        Ok(Self(id))
    }
}

/// Blob flavor: binary (classic) or post-quantum (Dilithium-signed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BlobType {
    #[default]
    Binary,
    Pq,
}

impl std::str::FromStr for BlobType {
    type Err = crate::error::MikaStorageError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "pq" | "dilithium" | "post-quantum" => Ok(BlobType::Pq),
            _ => Ok(BlobType::Binary),
        }
    }
}

/// A stored blob: its identifier and raw content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub id: BlobID,
    pub data: Vec<u8>,
    /// Walrus-style flavor (binary / post-quantum).
    #[serde(default)]
    pub flavor: BlobType,
    /// Dilithium signature over `data` for PQ blobs (empty for binary).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pq_signature: Vec<u8>,
}

impl Blob {
    /// Create a new blob (binary flavor), computing its content-addressed ID.
    pub fn new(data: Vec<u8>) -> Self {
        let id = BlobID::from_content(&data);
        Self {
            id,
            data,
            flavor: BlobType::Binary,
            pq_signature: vec![],
        }
    }

    /// Create a PQ-flavored blob (Dilithium-signed path).
    pub fn pq(data: Vec<u8>) -> Self {
        let id = BlobID::from_content(&data);
        Self {
            id,
            data,
            flavor: BlobType::Pq,
            pq_signature: vec![],
        }
    }

    /// Attach a Dilithium signature (from [`signatures::sign_message`]).
    pub fn with_pq_signature(mut self, signature: Vec<u8>) -> Self {
        self.pq_signature = signature;
        self
    }

    /// Sign a PQ blob's data with a Dilithium keypair (Walrus PQ 4X/5X).
    /// Binary blobs return `Ok(vec![])` (no signature).
    pub fn sign_pq(
        &self,
        private_key_hex: &str,
        curve: kanari_crypto::CurveType,
    ) -> Result<Vec<u8>> {
        if self.flavor != BlobType::Pq {
            return Ok(vec![]);
        }
        kanari_crypto::signatures::sign_message(private_key_hex, &self.data, curve)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))
    }

    /// Verify a PQ blob's Dilithium signature.
    pub fn verify_pq(
        &self,
        signature: &[u8],
        public_key_hex: &str,
        curve: kanari_crypto::CurveType,
    ) -> Result<bool> {
        if self.flavor != BlobType::Pq {
            return Ok(true);
        }
        kanari_crypto::signatures::verify_signature_with_curve(
            public_key_hex,
            &self.data,
            signature,
            curve,
        )
        .map_err(|e| MikaStorageError::Crypto(e.to_string()))
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_id_is_deterministic() {
        let a = Blob::new(b"hello world".to_vec());
        let b = Blob::new(b"hello world".to_vec());
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn blob_id_changes_with_content() {
        let a = Blob::new(b"hello".to_vec());
        let b = Blob::new(b"world".to_vec());
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn blob_id_round_trip_hex() {
        let blob = Blob::new(b"round trip".to_vec());
        let parsed = BlobID::try_from(&blob.id.to_hex() as &str).unwrap();
        assert_eq!(parsed, blob.id);
    }
}
