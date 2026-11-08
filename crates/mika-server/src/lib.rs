//! Mika storage node server crate.
//!
//! HTTP data plane (axum) for erasure-coded slivers + web API, backed by a
//! fjall `SliverStore`, plus the libp2p control plane (`p2p`) for
//! node-to-node sliver announcements (gossipsub) and DHT discovery.

pub mod p2p;
pub mod proof;
pub mod server;
pub mod store;
pub mod web;

use crate::store::SliverStore;
use std::sync::{Arc, Mutex};

/// Unified application state shared by both the sliver storage API and the
/// web-facing API.
#[derive(Clone)]
pub struct AppState {
    pub node_id: String,
    pub store: SliverStore,
    pub epoch: u64,
    /// P2P control plane (gossipsub announcements). `None` in unit tests /
    /// `--no-p2p` mode; HTTP data plane works without it.
    pub p2p: Arc<Mutex<Option<crate::p2p::MikaP2P>>>,
}

impl AppState {
    pub fn new(node_id: String, store: SliverStore, epoch: u64) -> Self {
        Self {
            node_id,
            store,
            epoch,
            p2p: Arc::new(Mutex::new(None)),
        }
    }

    /// Publish a sliver announcement (no-op when P2P is disabled).
    pub fn announce_sliver(&self, blob_id: &mika_storage::blob::BlobID, shard_index: u32) {
        if let Ok(mut guard) = self.p2p.lock()
            && let Some(p) = guard.as_mut()
        {
            let ann = crate::p2p::SliverAnnounce {
                blob_id: blob_id.to_hex(),
                shard_index,
                node_id: self.node_id.clone(),
                epoch: self.epoch,
            };
            p.announce_sliver(&ann);
        }
    }
}

pub type SharedState = Arc<AppState>;
