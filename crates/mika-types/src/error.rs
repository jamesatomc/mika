//! Error types for the Mika storage layer.

use thiserror::Error;

/// Top-level error for all Mika storage operations.
#[derive(Error, Debug)]
pub enum MikaStorageError {
    #[error("blob not found: {0}")]
    BlobNotFound(String),
    #[error("invalid blob id: {0}")]
    InvalidBlobId(String),
    #[error("erasure coding error: {0}")]
    Erasure(String),
    #[error("insufficient shards: have {have}, need {need}")]
    InsufficientShards { have: usize, need: usize },
    #[error("storage node unavailable: {0}")]
    NodeUnavailable(String),
    #[error("write certificate invalid: {0}")]
    InvalidWriteCertificate(String),
    #[error("epoch expired")]
    EpochExpired,
    #[error("insufficient balance for storage fee: need {need}, have {have}")]
    InsufficientBalance { need: u64, have: u64 },
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("on-chain error: {0}")]
    OnChain(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<std::io::Error> for MikaStorageError {
    fn from(e: std::io::Error) -> Self {
        MikaStorageError::Io(e.to_string())
    }
}

impl From<bcs::Error> for MikaStorageError {
    fn from(e: bcs::Error) -> Self {
        MikaStorageError::Serialization(e.to_string())
    }
}

impl From<hex::FromHexError> for MikaStorageError {
    fn from(e: hex::FromHexError) -> Self {
        MikaStorageError::InvalidBlobId(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, MikaStorageError>;
