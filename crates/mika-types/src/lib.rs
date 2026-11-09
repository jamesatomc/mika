//! Mika Move types — Rust mirrors of the on-chain `mika::storage` Move module.
//!
//! Like `kanari-types` mirrors `Coin`/`Transfer`, this crate holds only the
//! data-layer types that cross the chain boundary: content-addressed blobs,
//! slivers, anchors, fee payments and storage epochs. No engine, no I/O.

pub mod blob;
pub mod committee;
pub mod epoch;
pub mod error;
pub mod onchain;
pub mod sliver;
pub mod storage;

pub use blob::{Blob, BlobID, BlobType};
pub use epoch::{Epoch, Lease, current_epoch};
pub use error::{MikaStorageError, Result};
pub use onchain::{BlobAnchor as StorageAnchor, StorageFeePayment, StoragePayment};
pub use sliver::Sliver;
