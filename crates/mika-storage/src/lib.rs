//! Mika storage library — compatibility shim + DB layer.
//!
//! The real crates are now:
//! * `mika-types` — Move type bindings (blob/sliver/onchain/error/epoch)
//! * `mika-core`  — erasure/commitment/certificate/client/integration
//!
//! This crate keeps the DB layer (`store::SliverStore`, the fjall-backed
//! sliver database) and re-exports both layers so existing
//! `use mika_storage::...` code keeps compiling.

pub mod store;

// Re-export the type layer (Move bindings, like kanari-types).
pub use mika_types::{
    Blob, BlobID, Epoch, Lease, MikaStorageError, Result, Sliver, StorageAnchor, StorageFeePayment,
    StoragePayment, current_epoch,
};

// Re-export the core layer (erasure/commitment/client).
pub use mika_core::{
    ErasureParams, MikaClient, NodeAddr, ReadCertificate, RetrieveResult, StorageClient,
    StoreResult, WriteCertificate, WriteCertificateQuorum, decode, encode, rejoin_shards,
    split_into_shards,
};

#[cfg(feature = "onchain")]
pub use mika_core::KanariStorage;

// Keep the old module paths working (mika_storage::blob::BlobID, etc.)
pub mod blob {
    pub use mika_types::blob::*;
}
pub mod epoch {
    pub use mika_types::epoch::*;
}
pub mod error {
    pub use mika_types::error::*;
}
pub mod onchain {
    pub use mika_types::onchain::*;
}
pub mod sliver {
    pub use mika_types::sliver::*;
}
pub mod certificate {
    pub use mika_core::certificate::*;
}
pub mod commitment {
    pub use mika_core::commitment::*;
}
pub mod erasure {
    pub use mika_core::erasure::*;
}
pub mod storage_client {
    pub use mika_core::storage_client::*;
}
#[cfg(feature = "onchain")]
pub mod kanari_integration {
    pub use mika_core::kanari_integration::*;
}
