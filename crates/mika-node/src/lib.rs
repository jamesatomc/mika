//! Mika storage node crate.
//!
//! Runs a storage node that holds erasure-coded slivers for the Mika
//! decentralized storage layer and proves their availability.

pub mod proof;
pub mod server;
pub mod store;
pub mod web;

use crate::store::SliverStore;
use std::sync::Arc;

/// Unified application state shared by both the sliver storage API and the
/// web-facing API.
#[derive(Clone)]
pub struct AppState {
    pub node_id: String,
    pub store: SliverStore,
    pub epoch: u64,
}

impl AppState {
    pub fn new(node_id: String, store: SliverStore, epoch: u64) -> Self {
        Self {
            node_id,
            store,
            epoch,
        }
    }
}

pub type SharedState = Arc<AppState>;
