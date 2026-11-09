//! Mika storage library — DB layer.
//!
//! The real crates are now:
//! * `mika-types` — Move type bindings (blob/sliver/onchain/error/epoch)
//! * `mika-core`  — erasure/commitment/certificate/client/integration

pub mod store;

pub use store::SliverStore;
