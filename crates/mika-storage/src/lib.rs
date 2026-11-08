//! Mika storage library: Walrus-style decentralized, erasure-coded object storage.
//!
//! # Architecture
//!
//! Mika splits blobs into erasure-coded slivers and distributes them across
//! storage nodes. On-chain, only the blob commitment, owner pointer and lease
//! are stored; the bulk data lives off-chain. Storage rent is paid in KANARI
//! native coin, and blob ownership can be transferred with a native coin
//! transfer.
//!
//! ## Two layers, both transferable
//!
//! 1. **On-chain**: native KANARI coin objects (existing kanari-core), transferred
//!    via `changeset.transfer` / `execute_backend_native_transfer`.
//! 2. **Off-chain (Mika)**: erasure-coded blobs on storage nodes; ownership
//!    proven by on-chain anchor + write-certificate quorum.

pub mod blob;
pub mod certificate;
pub mod commitment;
pub mod epoch;
pub mod erasure;
pub mod error;
pub mod onchain;
pub mod sliver;
pub mod storage_client;

#[cfg(feature = "onchain")]
pub mod kanari_integration;

pub use blob::{Blob, BlobID};
pub use certificate::{ReadCertificate, WriteCertificate, WriteCertificateQuorum};
pub use epoch::{Epoch, Lease, current_epoch};
pub use erasure::{ErasureParams, decode, encode, rejoin_shards, split_into_shards};
pub use error::{MikaStorageError, Result};
pub use onchain::{BlobAnchor as StorageAnchor, StorageFeePayment, StoragePayment};
pub use sliver::Sliver;
pub use storage_client::{MikaClient, NodeAddr, RetrieveResult, StorageClient, StoreResult};

#[cfg(feature = "onchain")]
pub use kanari_integration::KanariStorage;
