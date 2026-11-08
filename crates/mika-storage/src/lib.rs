//! Mika storage library — DB layer.
//!
//! The real crates are now:
//! * `mika-types` — Move type bindings (blob/sliver/onchain/error/epoch)
//! * `mika-core`  — erasure/commitment/certificate/client/integration
//!
//! This crate keeps only the DB (`store::SliverStore`, the fjall-backed
//! sliver database) plus compatibility re-exports for existing imports.

pub mod store;
