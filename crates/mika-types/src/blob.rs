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

/// A stored blob: its identifier and raw content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub id: BlobID,
    pub data: Vec<u8>,
}

impl Blob {
    /// Create a new blob, computing its content-addressed ID.
    pub fn new(data: Vec<u8>) -> Self {
        let id = BlobID::from_content(&data);
        Self { id, data }
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
