//! Mika core engine — Red Stuff 2D erasure coding, 2D commitments,
//! certificates/PoA and the on-chain integration.
//!
//! Depends on `mika-types` for the Move data types (blob/sliver/anchor),
//! `mika-storage` for the fjall DB layer (`SliverStore` facade).

pub mod certificate;
pub mod commitment;
pub mod erasure;

#[cfg(feature = "onchain")]
pub mod kanari_integration;

pub use certificate::{WriteCertificate, WriteCertificateQuorum};
pub use erasure::{
    ErasureParams, decode, encode, redstuff_decode, redstuff_encode, redstuff_recover_primary,
    rejoin_shards, split_into_shards,
};

/// The engine's DB layer: the fjall-backed `SliverStore` from `mika-storage`
/// (the DB crate). Engine code calls the store through this facade instead
/// of raw fjall, so `mika-core` never re-implements key encodings.
pub use mika_storage::store::SliverStore;

#[cfg(feature = "onchain")]
pub use kanari_integration::KanariStorage;

pub use mika_types::{Blob, BlobID, Epoch, Lease, Sliver, current_epoch};
