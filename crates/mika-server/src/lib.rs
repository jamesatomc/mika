//! Mika storage node server crate — P2P-only (no HTTP API, no web UI).
//!
//! The node mesh is a pure libp2p network:
//!
//! - control plane: `gossipsub` (`mika/slivers/v1`) for sliver announcements
//!   + KAD DHT for discovery, over `tcp`/`noise`/`yamux` (+ `dcutr`/`relay`/
//!     `ping`/`identify`);
//! - data plane: libp2p request-response protocols carry the sliver bytes
//!   directly between peers (`/mika/sliver-store/1.0.0` for ingress,
//!   `/mika/sliver-fetch/1.0.0` for peer-pull dissemination).
//!
//! Sliver bytes never go over gossipsub.
//!
//! Storage itself (`SliverStore`, fjall) lives in the `mika-storage` crate;
//! this crate re-exports it via `store_proxy`.

pub mod p2p;

use kanari_crypto::{CurveType, generate_keypair};
/// The real DB crate — sliver bytes live in `mika-storage`'s fjall
/// `SliverStore`; this crate is only the P2P mesh on top of it.
pub use mika_storage::store::SliverStore;
use move_core_types::account_address::AccountAddress;
use std::sync::{Arc, Mutex};

/// Unified application state for a P2P-only storage node.
#[derive(Clone)]
pub struct AppState {
    pub node_id: String,
    pub store: SliverStore,
    pub epoch: u64,
    /// This node's libp2p PeerId (filled in once the swarm starts).
    /// Carried in sliver announcements so peers fetch from us directly —
    /// no IP list, no HTTP.
    pub peer_id: String,
    /// Lease expiry (Walrus reclaim): blob_id hex → expires_at epoch.
    /// Populated on store; the reclaim loop deletes expired blobs' shards.
    pub expiry: Arc<Mutex<std::collections::HashMap<String, u64>>>,
    /// Node's signing key for write certificates (BFT quorum: each node signs
    /// its own certificate when storing a sliver).
    pub private_key: String,
    pub public_key: String,
    pub curve_type: CurveType,
    /// P2P control plane (gossipsub announcements + request-response).
    /// `None` in unit tests / `--no-p2p` mode; the node then only keeps its
    /// fjall store.
    pub p2p: Arc<Mutex<Option<crate::p2p::MikaP2P>>>,
    /// Committee registry snapshot (Walrus-style shared system object
    /// mirror): member keys whose votes count toward BFT quorum.
    pub committee: Arc<Vec<AccountAddress>>,
    /// Sliver announcements received over gossipsub, awaiting peer-pull.
    /// The drive loop drains this queue and fetches any sliver it doesn't
    /// hold yet from the announcing peer — so the uploader only talks to
    /// ONE entry peer and the mesh spreads the rest.
    pub pending: Arc<Mutex<Vec<crate::p2p::SliverAnnounce>>>,
}

impl AppState {
    pub fn new(node_id: String, store: SliverStore, epoch: u64) -> Self {
        let keypair = generate_keypair(CurveType::K256).expect("failed to generate node keypair");
        Self {
            node_id: node_id.clone(),
            store,
            epoch,
            expiry: Arc::new(Mutex::new(std::collections::HashMap::new())),
            private_key: keypair.private_key.to_string(),
            public_key: keypair.public_key,
            curve_type: CurveType::K256,
            p2p: Arc::new(Mutex::new(None)),
            peer_id: String::new(),
            pending: Arc::new(Mutex::new(Vec::new())),
            committee: Arc::new(Vec::new()),
        }
    }

    /// Queue a received gossipsub announcement for peer-pull (the drive
    /// loop in `mika-node` / `mika-cli node` drains this list).
    pub fn queue_announce(&self, ann: crate::p2p::SliverAnnounce) {
        if let Ok(mut q) = self.pending.lock() {
            q.push(ann);
        }
    }

    /// Take all queued announcements (leaves the queue empty).
    pub fn drain_pending(&self) -> Vec<crate::p2p::SliverAnnounce> {
        self.pending
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }
}

pub type SharedState = Arc<AppState>;
