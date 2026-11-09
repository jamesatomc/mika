//! Mika core engine — erasure coding, 2D commitments, certificates and the
//! storage client / on-chain integration.
//!
//! Depends on `mika-types` for the Move data types (blob/sliver/anchor).

pub mod certificate;
pub mod commitment;
pub mod erasure;
#[cfg(any(feature = "onchain", test))]
pub mod rpc_client;
pub mod storage_client;

#[cfg(feature = "onchain")]
pub mod kanari_integration;

pub use certificate::{ReadCertificate, WriteCertificate, WriteCertificateQuorum};
pub use erasure::{ErasureParams, decode, encode, rejoin_shards, split_into_shards};
pub use storage_client::{MikaClient, NodeAddr, RetrieveResult, StorageClient, StoreResult};

#[cfg(feature = "onchain")]
pub use kanari_integration::KanariStorage;

pub use mika_types::{
    Blob, BlobID, Epoch, Lease, MikaStorageError, Result, Sliver, StorageAnchor, StorageFeePayment,
    StoragePayment, current_epoch,
};
