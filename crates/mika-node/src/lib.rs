//! Mika storage node crate.
//!
//! Runs a storage node that holds erasure-coded slivers for the Mika
//! decentralized storage layer and proves their availability.

pub mod proof;
pub mod server;
pub mod store;

pub use server::NodeState;
pub use store::SliverStore;
