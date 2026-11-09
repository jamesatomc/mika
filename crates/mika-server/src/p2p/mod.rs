//! libp2p node-to-node fabric for Mika (modeled on `kanari-node::p2p`).
//!
//! - Control plane: `gossipsub` (signed) for `mika/slivers/v1` announcements,
//!   `kad` (DHT) for blob→peer discovery, `identify`/`ping`/`dcutr`/`relay`
//!   for connectivity. Announcements carry the announcer's `peer_id`, never
//!   sliver bytes.
//! - Data plane: two request-response protocols carry the bytes peer-to-peer:
//!   `/mika/sliver-store/1.0.0` (ingress: client pushes a sliver, node stores
//!   it and answers with its signed write certificate) and
//!   `/mika/sliver-fetch/1.0.0` (dissemination: a peer that missed a sliver
//!   pulls it from the announcer).
//!
//! Transport on both sides is identical: `tcp` + `noise` (ed25519) + `yamux`.

use anyhow::Result;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, FutureExt, StreamExt};
use libp2p_core::{Multiaddr, PeerId, Transport, upgrade};
use libp2p_dcutr as dcutr;
use libp2p_gossipsub as gossipsub;
use libp2p_gossipsub::{IdentTopic, MessageAuthenticity, ValidationMode};
use libp2p_identify as identify;
use libp2p_identity::Keypair;
use libp2p_kad::{self as kad, store::MemoryStore};
use libp2p_noise as noise;
use libp2p_ping as ping;
use libp2p_relay as relay;
use libp2p_request_response as rrp;
use libp2p_swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p_tcp as tcp;
use libp2p_yamux as yamux;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{debug, info, warn};

pub const SLIVERS_TOPIC: &str = "mika/slivers/v1";
pub const VOTES_TOPIC: &str = "mika/votes/v1";
pub const STORE_PROTOCOL: &str = "/mika/sliver-store/1.0.0";
pub const FETCH_PROTOCOL: &str = "/mika/sliver-fetch/1.0.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SliverAnnounce {
    pub blob_id: String,
    pub shard_index: u32,
    pub node_id: String,
    pub epoch: u64,
    /// libp2p PeerId of the holder (peers fetch directly from it).
    pub peer_id: String,
    /// Canonical committee node_key (hex) of the holder — the shard
    /// assignment router (Walrus-style map from shard → node).
    pub node_key: String,
}

/// Request for the fetch protocol: which shard do we want?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SliverFetchRequest {
    pub blob_id: String,
    pub shard_index: u32,
}

/// Application-level BFT availability vote (committee consensus analogue):
/// a committee member's signed claim of the shard-bitmap state root for one
/// blob. `state_root` = BLAKE3(concatenated present-shard indices).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilityVote {
    pub blob_id: String,
    pub state_root: [u8; 32],
    pub epoch: u64,
    pub voter: [u8; 32],
    pub public_key: String,
    pub signature: Vec<u8>,
}

/// Compute the availability state root for a blob from its present shards.
pub fn state_root_for(present_shards: &[u32]) -> [u8; 32] {
    let mut bytes = Vec::new();
    let mut sorted = present_shards.to_vec();
    sorted.sort_unstable();
    for s in sorted {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    kanari_crypto::hash_data_blake3_array(&bytes)
}

// ---------------------------------------------------------------------------
// Length-prefixed framing for the rrp codecs: [u32 LE length][payload].
// The small side (request) is JSON, the big side (sliver bytes) is raw —
// no base64/number-array blowup.
// ---------------------------------------------------------------------------

fn read_frame(buf: &[u8]) -> std::io::Result<Vec<u8>> {
    if buf.len() < 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "frame too short",
        ));
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "frame truncated",
        ));
    }
    Ok(buf[4..4 + len].to_vec())
}

fn write_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

type IoResult<T> = std::io::Result<T>;

/// Codec for the store protocol: request = raw sliver bytes (the big side),
/// response = the node's signed write certificate as JSON (small side).
#[derive(Debug, Clone, Default)]
pub struct StoreCodec;

impl rrp::Codec for StoreCodec {
    type Protocol = &'static str;
    type Request = Vec<u8>;
    type Response = String;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> IoResult<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        read_frame(&buf)
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> IoResult<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        let payload = read_frame(&buf)?;
        Ok::<String, std::io::Error>(String::from_utf8(payload).unwrap_or_default())
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> IoResult<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        io.write_all(&write_frame(&req)).await?;
        io.flush().await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> IoResult<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        io.write_all(&write_frame(res.as_bytes())).await?;
        io.flush().await
    }
}

/// Codec for the fetch protocol: request = JSON `SliverFetchRequest` (small
/// side), response = raw sliver bytes (the big side).
#[derive(Debug, Clone, Default)]
pub struct FetchCodec;

impl rrp::Codec for FetchCodec {
    type Protocol = &'static str;
    type Request = SliverFetchRequest;
    type Response = Vec<u8>;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> IoResult<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        let payload = read_frame(&buf)?;
        serde_json::from_slice::<SliverFetchRequest>(&payload)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> IoResult<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        read_frame(&buf)
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> IoResult<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let payload = serde_json::to_vec(&req).unwrap_or_default();
        io.write_all(&write_frame(&payload)).await?;
        io.flush().await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> IoResult<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        io.write_all(&write_frame(&res)).await?;
        io.flush().await
    }
}

#[derive(NetworkBehaviour)]
#[behaviour(prelude = "libp2p_swarm::derive_prelude")]
pub struct MikaBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<MemoryStore>,
    pub dcutr: dcutr::Behaviour,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub relay: relay::Behaviour,
    pub store: rrp::Behaviour<StoreCodec>,
    pub fetch: rrp::Behaviour<FetchCodec>,
}

pub struct MikaP2P {
    pub swarm: Swarm<MikaBehaviour>,
    pub topic: IdentTopic,
    pub votes_topic: IdentTopic,
    pub peer_id: PeerId,
    /// Outbound fetch requests in flight: request id → (peer, blob, shard).
    pending_fetch: HashMap<rrp::OutboundRequestId, (PeerId, String, u32)>,
    /// De-dup set so repeated announcements don't re-request.
    inflight: HashSet<(PeerId, String, u32)>,
    /// BFT availability votes: (blob_id, state_root) → distinct voters.
    votes: HashMap<(String, [u8; 32]), HashSet<[u8; 32]>>,
    /// (blob_id, state_root) pairs that reached 2/3 committee BFT commit.
    committed: HashSet<(String, [u8; 32])>,
    /// Committee node_key (hex) → libp2p PeerId learnt from announcements.
    /// Used to forward store requests to the shard's assigned node.
    peers_by_key: HashMap<String, PeerId>,
    /// Store requests in flight for forwarding; response is proxied to the
    /// original client's channel.
    pending_forwards: HashMap<rrp::OutboundRequestId, rrp::ResponseChannel<String>>,
}

fn build_gossipsub(keypair: Keypair) -> Result<(gossipsub::Behaviour, IdentTopic, IdentTopic)> {
    let config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(1))
        .validation_mode(ValidationMode::Strict)
        .message_id_fn(|m: &gossipsub::Message| {
            gossipsub::MessageId::from(kanari_crypto::hash_data_blake3(&m.data))
        })
        .mesh_n_low(6)
        .mesh_n(12)
        .mesh_n_high(24)
        .gossip_factor(0.25)
        .flood_publish(false)
        .build()
        .map_err(|e| anyhow::anyhow!("Gossipsub config error: {}", e))?;
    let mut gossipsub = gossipsub::Behaviour::new(MessageAuthenticity::Signed(keypair), config)
        .map_err(|e| anyhow::anyhow!("Failed to create gossipsub: {}", e))?;
    let topic = IdentTopic::new(SLIVERS_TOPIC);
    gossipsub.subscribe(&topic)?;
    let votes_topic = IdentTopic::new(VOTES_TOPIC);
    gossipsub.subscribe(&votes_topic)?;
    Ok((gossipsub, topic, votes_topic))
}

impl MikaP2P {
    pub fn new(keypair: Keypair, listen_port: u16) -> Result<Self> {
        let local_peer_id = PeerId::from(keypair.public());
        info!("mika p2p: local peer id {}", local_peer_id);

        let transport = tcp::tokio::Transport::default()
            .upgrade(upgrade::Version::V1)
            .authenticate(noise::Config::new(&keypair)?)
            .multiplex(yamux::Config::default())
            .boxed();
        let (gossipsub, topic, votes_topic) = build_gossipsub(keypair.clone())?;

        let store = rrp::Behaviour::new(
            [(STORE_PROTOCOL, rrp::ProtocolSupport::Full)],
            rrp::Config::default(),
        );
        let fetch = rrp::Behaviour::new(
            [(FETCH_PROTOCOL, rrp::ProtocolSupport::Full)],
            rrp::Config::default(),
        );

        let mut kademlia = kad::Behaviour::new(local_peer_id, MemoryStore::new(local_peer_id));
        kademlia.set_mode(Some(kad::Mode::Server));
        let dcutr = dcutr::Behaviour::new(local_peer_id);
        let identify = identify::Behaviour::new(identify::Config::new(
            "/mika/1.0.0".to_string(),
            keypair.public(),
        ));
        let ping = ping::Behaviour::new(ping::Config::new());
        let relay = relay::Behaviour::new(local_peer_id, relay::Config::default());

        let behaviour = MikaBehaviour {
            gossipsub,
            kademlia,
            dcutr,
            identify,
            ping,
            relay,
            store,
            fetch,
        };
        let mut swarm = Swarm::new(
            transport,
            behaviour,
            local_peer_id,
            libp2p_swarm::Config::with_tokio_executor()
                .with_idle_connection_timeout(Duration::from_secs(60)),
        );
        swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{listen_port}").parse()?)?;
        Ok(Self {
            swarm,
            topic,
            votes_topic,
            peer_id: local_peer_id,
            pending_fetch: HashMap::new(),
            inflight: HashSet::new(),
            votes: HashMap::new(),
            committed: HashSet::new(),
            peers_by_key: HashMap::new(),
            pending_forwards: HashMap::new(),
        })
    }

    pub fn dial_bootstrap(&mut self, addrs: &[String]) {
        for s in addrs {
            match s.parse::<Multiaddr>() {
                Ok(addr) => {
                    info!("mika p2p: dialing bootstrap {}", addr);
                    self.swarm.dial(addr).ok();
                }
                Err(e) => warn!("mika p2p: bad bootstrap addr {s}: {e}"),
            }
        }
    }

    pub fn announce_sliver(&mut self, ann: &SliverAnnounce) {
        let data = serde_json::to_vec(ann).unwrap_or_default();
        if let Err(e) = self
            .swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), data)
        {
            debug!("mika p2p: publish sliver announce: {e}");
        }
    }

    /// Publish a signed availability vote to the committee.
    pub fn publish_vote(&mut self, vote: &AvailabilityVote) {
        let data = serde_json::to_vec(vote).unwrap_or_default();
        if let Err(e) = self
            .swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.votes_topic.clone(), data)
        {
            debug!("mika p2p: publish vote: {e}");
        }
    }

    /// Record a received vote; returns `true` once this vote completes the
    /// 2/3 committee BFT commit for the (blob, state_root).
    fn record_vote(
        &mut self,
        vote: AvailabilityVote,
        committee: &[move_core_types::account_address::AccountAddress],
    ) -> bool {
        // Bind wire claims to a verified K256 signature over
        // blob_id || state_root || epoch || voter, and to the advertised
        // committee identity (node_key == BLAKE3(pubkey)).
        let declared: [u8; 32] =
            match mika_types::committee::node_key_for_public_key(&vote.public_key) {
                Ok(a) => a.into_bytes(),
                Err(_) => return false,
            };
        if declared != vote.voter {
            return false;
        }
        let mut msg = Vec::new();
        msg.extend_from_slice(vote.blob_id.as_bytes());
        msg.extend_from_slice(&vote.state_root);
        msg.extend_from_slice(&vote.epoch.to_le_bytes());
        msg.extend_from_slice(&vote.voter);
        let digest = kanari_crypto::hash_data_blake3_array(&msg);
        if !kanari_crypto::signatures::verify_signature_with_curve(
            &vote.public_key,
            &digest,
            &vote.signature,
            kanari_crypto::CurveType::K256,
        )
        .unwrap_or(false)
        {
            debug!("mika p2p: bad vote signature for {}", vote.blob_id);
            return false;
        }
        let key = (vote.blob_id, vote.state_root);
        let voters = self.votes.entry(key.clone()).or_default();
        let before = voters.len();
        voters.insert(vote.voter);
        if voters.len() == before {
            return false;
        }
        let size = committee.len().max(1);
        let needed = mika_types::committee::bft_quorum_threshold(size);
        if voters.len() >= needed && !self.committed.contains(&key) {
            self.committed.insert(key.clone());
            info!(blob = %key.0, root = %hex::encode(key.1), voters = voters.len(), needed, "mika bft: availability committed");
            return true;
        }
        false
    }

    /// Peer assigned to a committee node_key (hex), learnt from announces.
    pub fn assigned_peer(&self, node_key_hex: &str) -> Option<PeerId> {
        self.peers_by_key.get(node_key_hex).copied()
    }

    /// Send a fetch request to a peer for one missing sliver.
    /// De-duplicated via the in-flight set; returns `false` when already
    /// requested (or the peer id is unparseable).
    pub fn send_fetch(&mut self, peer_id: &str, blob_id: String, shard_index: u32) -> bool {
        let Ok(peer) = peer_id.parse::<PeerId>() else {
            return false;
        };
        let key = (peer, blob_id.clone(), shard_index);
        if self.inflight.contains(&key) {
            return false;
        }
        self.inflight.insert(key.clone());
        let req = SliverFetchRequest {
            blob_id,
            shard_index,
        };
        let rid = self.swarm.behaviour_mut().fetch.send_request(&peer, req);
        self.pending_fetch.insert(rid, key);
        true
    }

    /// Drain pending swarm events synchronously (no await) and return the
    /// slivers fetched from peers (outbound fetch responses — each is a
    /// complete serialized `Sliver`, self-describing its blob/shard).
    ///
    /// Inbound requests are serviced inline: store requests are persisted
    /// and answered with a signed write certificate; fetch requests are
    /// answered straight from the local store. Announcements are queued
    /// onto `state.pending` for the node drive loop.
    pub fn drive_events(&mut self, state: &crate::AppState) -> Vec<mika_types::sliver::Sliver> {
        let mut fetched = Vec::new();
        while let Some(Some(event)) = self.swarm.next().now_or_never() {
            match event {
                SwarmEvent::Behaviour(MikaBehaviourEvent::Gossipsub(
                    gossipsub::Event::Message { message, .. },
                )) => {
                    if message.topic.to_string() == VOTES_TOPIC {
                        if let Ok(vote) = serde_json::from_slice::<AvailabilityVote>(&message.data)
                        {
                            // Committee membership: only votes from members
                            // (or anyone when unconfigured) are counted.
                            let committee = state.committee.clone();
                            self.record_vote(vote, &committee);
                        }
                    } else if let Ok(ann) = serde_json::from_slice::<SliverAnnounce>(&message.data)
                    {
                        info!(
                            "mika p2p: sliver {}#{} at {} (epoch {})",
                            ann.blob_id, ann.shard_index, ann.node_id, ann.epoch
                        );
                        if !ann.peer_id.is_empty()
                            && !ann.node_key.is_empty()
                            && let Ok(pid) = ann.peer_id.parse::<PeerId>()
                        {
                            self.peers_by_key.insert(ann.node_key.clone(), pid);
                        }
                        state.queue_announce(ann);
                    }
                }
                SwarmEvent::Behaviour(MikaBehaviourEvent::Store(rrp::Event::Message {
                    peer,
                    message:
                        rrp::Message::Request {
                            request, channel, ..
                        },
                    ..
                })) => {
                    // Ingress: persist the sliver, track lease expiry, sign a
                    // write certificate for the BFT quorum.
                    if let Ok(sliver) = mika_types::sliver::Sliver::from_bytes(&request) {
                        // Walrus-style assignment gate: when a committee is
                        // configured, each sliver belongs to ONE member only
                        // (round-robin by shard). If this is not it, forward
                        // the store request (and the original client's
                        // response channel) to the assigned peer. Unmapped
                        // peers fall back to local store (availability first).
                        let committee = state.committee.clone();
                        let own_node_key =
                            mika_types::committee::node_key_for_public_key(&state.public_key)
                                .map(|k| k.to_hex_literal())
                                .unwrap_or_default();
                        let assigned = if committee.is_empty() {
                            own_node_key.clone()
                        } else {
                            let mut sorted: Vec<move_core_types::account_address::AccountAddress> =
                                committee.iter().cloned().collect();
                            sorted.sort_by_key(|a| a.to_string());
                            sorted[(sliver.shard_index as usize) % sorted.len()].to_hex_literal()
                        };
                        if !committee.is_empty() && assigned != own_node_key {
                            if let Some(&target_peer) = self.peers_by_key.get(&assigned) {
                                info!(peer = %peer, blob = %sliver.blob_id, shard = sliver.shard_index, assigned = %assigned, "mika p2p: forwarding sliver to assigned node");
                                let rid = self
                                    .swarm
                                    .behaviour_mut()
                                    .store
                                    .send_request(&target_peer, request.clone());
                                self.pending_forwards.insert(rid, channel);
                                continue;
                            }
                            tracing::warn!(blob = %sliver.blob_id, shard = sliver.shard_index, assigned = %assigned, "mika p2p: assigned peer unknown — storing locally");
                        }
                        let _ = state.store.put_sync(&sliver);
                        state.expiry.lock().unwrap().insert(
                            sliver.blob_id.to_hex(),
                            state.epoch + mika_types::epoch::EPOCH_SECONDS,
                        );
                        // NOTE: announce inline — `state.announce_sliver`
                        // would lock `state.p2p`, which the drive loop
                        // already holds while calling this method.
                        let ann = SliverAnnounce {
                            blob_id: sliver.blob_id.to_hex(),
                            shard_index: sliver.shard_index,
                            node_id: state.node_id.clone(),
                            epoch: state.epoch,
                            peer_id: state.peer_id.clone(),
                            node_key: own_node_key,
                        };
                        self.announce_sliver(&ann);
                        let sliver_hash = kanari_crypto::hash_data_blake3_array(&sliver.data);
                        let mut cert = mika_core::certificate::WriteCertificate {
                            blob_id: sliver.blob_id,
                            shard_index: sliver.shard_index,
                            node_id: state.node_id.clone(),
                            epoch: state.epoch,
                            sliver_hash,
                            signature: vec![],
                            public_key: state.public_key.clone(),
                            curve_type: format!("{:?}", state.curve_type),
                        };
                        let _ = cert.sign(&state.private_key, state.curve_type);
                        let payload = serde_json::to_string(&cert).unwrap_or_default();
                        info!(peer = %peer, blob = %sliver.blob_id, shard = sliver.shard_index, "mika p2p: stored sliver via store protocol");
                        let _ = self
                            .swarm
                            .behaviour_mut()
                            .store
                            .send_response(channel, payload);
                    }
                }
                SwarmEvent::Behaviour(MikaBehaviourEvent::Store(rrp::Event::Message {
                    message:
                        rrp::Message::Response {
                            request_id,
                            response,
                        },
                    ..
                })) => {
                    // Forwarded store requests: proxy the assignee's
                    // certificate back to the original client.
                    if let Some(channel) = self.pending_forwards.remove(&request_id) {
                        let _ = self
                            .swarm
                            .behaviour_mut()
                            .store
                            .send_response(channel, response);
                    }
                }
                SwarmEvent::Behaviour(MikaBehaviourEvent::Fetch(rrp::Event::Message {
                    peer,
                    message:
                        rrp::Message::Request {
                            request, channel, ..
                        },
                    ..
                })) => {
                    // Dissemination: answer straight from the local store.
                    let bytes = mika_types::blob::BlobID::try_from(request.blob_id.as_str())
                        .ok()
                        .and_then(|bid| {
                            state
                                .store
                                .get_sync(&bid, request.shard_index)
                                .ok()
                                .flatten()
                        })
                        .and_then(|s| s.to_bytes().ok())
                        .unwrap_or_default();
                    info!(peer = %peer, blob = %request.blob_id, shard = request.shard_index, "mika p2p: serving sliver via fetch protocol");
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .fetch
                        .send_response(channel, bytes);
                }
                SwarmEvent::Behaviour(MikaBehaviourEvent::Fetch(rrp::Event::Message {
                    message:
                        rrp::Message::Response {
                            request_id,
                            response,
                        },
                    ..
                })) => {
                    if let Some(key) = self.pending_fetch.remove(&request_id) {
                        self.inflight.remove(&key);
                    }
                    if !response.is_empty()
                        && let Ok(sliver) = mika_types::sliver::Sliver::from_bytes(&response)
                    {
                        fetched.push(sliver);
                    }
                }
                SwarmEvent::Behaviour(MikaBehaviourEvent::Fetch(rrp::Event::OutboundFailure {
                    request_id,
                    ..
                })) => {
                    if let Some(key) = self.pending_fetch.remove(&request_id) {
                        self.inflight.remove(&key);
                    }
                }
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    info!("mika p2p: connected {peer_id}")
                }
                _ => {}
            }
        }
        fetched
    }
}

// ---------------------------------------------------------------------------
// Client helpers for `mika-cli`: a temporary swarm that dials one entry
// peer and pushes/fetches sliver bytes. The client never needs gossipsub.
// ---------------------------------------------------------------------------

#[derive(NetworkBehaviour)]
#[behaviour(prelude = "libp2p_swarm::derive_prelude")]
struct ClientBehaviour {
    store: rrp::Behaviour<StoreCodec>,
    fetch: rrp::Behaviour<FetchCodec>,
}

async fn connect_client(multiaddr: &str) -> Result<Swarm<ClientBehaviour>> {
    let addr: Multiaddr = multiaddr.parse()?;
    let keypair = Keypair::generate_ed25519();
    let transport = tcp::tokio::Transport::default()
        .upgrade(upgrade::Version::V1)
        .authenticate(noise::Config::new(&keypair)?)
        .multiplex(yamux::Config::default())
        .boxed();
    let store = rrp::Behaviour::new(
        [(STORE_PROTOCOL, rrp::ProtocolSupport::Full)],
        rrp::Config::default(),
    );
    let fetch = rrp::Behaviour::new(
        [(FETCH_PROTOCOL, rrp::ProtocolSupport::Full)],
        rrp::Config::default(),
    );
    let behaviour = ClientBehaviour { store, fetch };
    let mut swarm = Swarm::new(
        transport,
        behaviour,
        PeerId::from(keypair.public()),
        libp2p_swarm::Config::with_tokio_executor(),
    );
    swarm.dial(addr)?;
    // Wait for the connection (requests fail fast if the peer is unreachable).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("mika p2p client: timed out connecting to {multiaddr}");
        }
        let event = tokio::time::timeout(Duration::from_secs(2), swarm.select_next_some())
            .await
            .map_err(|_| anyhow::anyhow!("mika p2p client: timed out connecting to {multiaddr}"))?;
        if matches!(event, SwarmEvent::ConnectionEstablished { .. }) {
            return Ok(swarm);
        }
    }
}

fn peer_from_multiaddr(multiaddr: &str) -> Result<PeerId> {
    multiaddr
        .parse::<Multiaddr>()?
        .iter()
        .find_map(|p| match p {
            libp2p_core::multiaddr::Protocol::P2p(hash) => Some(hash),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("multiaddr needs /p2p/<PeerId>: {multiaddr}"))
}

/// Resolve a `--nodes` value to a usable multiaddr.
///
/// - Full multiaddr (`/ip4/<ip>/tcp/<port>/p2p/<PeerId>`) — used as-is.
/// - `host:port` (e.g. `127.0.0.1:34003`) — looked up in an external
///   mesh map (`MIKA_MESH_FILE` env var, else `./.mesh`, else
///   `~/.config/mika/mesh`) so you don't have to paste 60-char peer ids.
///   The map is operator-owned data (one `name <multiaddr>` per line,
///   written by your own tooling) — the code never generates it.
pub fn resolve_multiaddr(input: &str) -> Result<String> {
    let input = input.trim();
    if input.contains("/p2p/") {
        return Ok(input.to_string());
    }
    let map = resolve_mesh_map()?;
    for (hostport, multiaddr) in &map {
        if hostport == input {
            return Ok(multiaddr.clone());
        }
    }
    anyhow::bail!(
        "cannot resolve '{input}' to a peer.\n\
         Pass a full multiaddr (/ip4/<ip>/tcp/<port>/p2p/<PeerId>) or make sure the mesh map\n\
         exists at $MIKA_MESH_FILE, ./.mesh, or ~/.config/mika/mesh with\n\
         `n1 /ip4/<ip>/tcp/<port>/p2p/<PeerId>` lines."
    )
}

/// Read the operator-owned mesh map as a `host:port -> multiaddr` map.
fn resolve_mesh_map() -> Result<Vec<(String, String)>> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("MIKA_MESH_FILE") {
        candidates.push(std::path::PathBuf::from(p));
    }
    candidates.push(std::path::PathBuf::from(".mesh"));
    if let Ok(p) = mika_common::mesh_map_path() {
        candidates.push(p);
    }
    let mut content = None;
    for path in candidates {
        if let Ok(c) = std::fs::read_to_string(&path) {
            content = Some(c);
            break;
        }
    }
    let content = content.ok_or_else(|| {
        anyhow::anyhow!("mesh map not found ($MIKA_MESH_FILE, ./.mesh, ~/.config/mika/mesh)")
    })?;
    let mut out = Vec::new();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let _name = parts.next();
        let Some(addr) = parts.next() else { continue };
        // /ip4/<host>/tcp/<port>/p2p/<PeerId>
        let stripped = addr.trim_start_matches("/ip4/");
        if let Some((rest, port_part)) = stripped.split_once("/tcp/") {
            let host = rest.split('/').next().unwrap_or(rest);
            let port = port_part.split('/').next().unwrap_or("");
            out.push((format!("{host}:{port}"), addr.to_string()));
        }
    }
    Ok(out)
}

/// Push slivers to one entry peer over the store protocol and return the
/// write certificates it signed (BFT quorum input). Errors when the peer
/// is unreachable or a response never arrives.
pub async fn push_slivers(
    multiaddr: &str,
    slivers: Vec<mika_types::sliver::Sliver>,
) -> Result<Vec<mika_core::certificate::WriteCertificate>> {
    let multiaddr = resolve_multiaddr(multiaddr)?;
    let mut swarm = connect_client(&multiaddr).await?;
    let peer = peer_from_multiaddr(&multiaddr)?;
    for sliver in &slivers {
        let bytes = sliver.to_bytes()?;
        swarm.behaviour_mut().store.send_request(&peer, bytes);
    }
    let mut certs = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while certs.len() < slivers.len() {
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!(
                "mika p2p client: timed out waiting for certificates ({}/{})",
                certs.len(),
                slivers.len()
            );
        }
        let event = swarm.select_next_some().await;
        if let SwarmEvent::Behaviour(ClientBehaviourEvent::Store(rrp::Event::Message {
            message: rrp::Message::Response { response, .. },
            ..
        })) = event
            && let Ok(cert) =
                serde_json::from_str::<mika_core::certificate::WriteCertificate>(&response)
        {
            certs.push(cert);
        }
    }
    Ok(certs)
}

/// Fetch slivers for one blob from one entry peer over the fetch protocol.
/// Returns the slivers that were found (empty response = not held).
pub async fn fetch_slivers(
    multiaddr: &str,
    blob_id: mika_types::blob::BlobID,
    shard_indices: Vec<u32>,
) -> Result<Vec<mika_types::sliver::Sliver>> {
    let multiaddr = resolve_multiaddr(multiaddr)?;
    let mut swarm = connect_client(&multiaddr).await?;
    let peer = peer_from_multiaddr(&multiaddr)?;
    for idx in &shard_indices {
        swarm.behaviour_mut().fetch.send_request(
            &peer,
            SliverFetchRequest {
                blob_id: blob_id.to_hex(),
                shard_index: *idx,
            },
        );
    }
    let mut out = Vec::new();
    let mut answered = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while answered < shard_indices.len() {
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!(
                "mika p2p client: timed out fetching shards ({answered}/{})",
                shard_indices.len()
            );
        }
        let event = swarm.select_next_some().await;
        if let SwarmEvent::Behaviour(ClientBehaviourEvent::Fetch(rrp::Event::Message {
            message: rrp::Message::Response { response, .. },
            ..
        })) = event
        {
            answered += 1;
            if !response.is_empty()
                && let Ok(sliver) = mika_types::sliver::Sliver::from_bytes(&response)
            {
                out.push(sliver);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_name_is_namespaced() {
        assert!(super::SLIVERS_TOPIC.starts_with("mika/"));
        assert!(super::STORE_PROTOCOL.starts_with("/mika/"));
        assert!(super::FETCH_PROTOCOL.starts_with("/mika/"));
    }

    #[test]
    fn frame_round_trip() {
        let payload = b"sliver-bytes".to_vec();
        let framed = write_frame(&payload);
        assert_eq!(read_frame(&framed).unwrap(), payload);
        assert!(read_frame(&framed[..2]).is_err());
    }

    #[test]
    fn resolve_multiaddr_full_passes_through() {
        let addr = "/ip4/127.0.0.1/tcp/34001/p2p/12D3KooWExampleExampleExampleExampleExamplePeer";
        assert_eq!(resolve_multiaddr(addr).unwrap(), addr);
    }

    #[test]
    fn resolve_mesh_map_parses_host_port() {
        let content = "n1 /ip4/127.0.0.1/tcp/34001/p2p/12D3KooWExampleExampleExamplePeer\nn2 /ip4/127.0.0.1/tcp/34002/p2p/12D3KooWExample2ExampleExamplePeer";
        let mut out = Vec::new();
        for line in content.lines() {
            let mut parts = line.split_whitespace();
            let _name = parts.next();
            let Some(addr) = parts.next() else { continue };
            let stripped = addr.trim_start_matches("/ip4/");
            if let Some((rest, port_part)) = stripped.split_once("/tcp/") {
                let host = rest.split('/').next().unwrap_or(rest);
                let port = port_part.split('/').next().unwrap_or("");
                out.push((format!("{host}:{port}"), addr.to_string()));
            }
        }
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "127.0.0.1:34001");
        assert_eq!(out[1].0, "127.0.0.1:34002");
    }

    #[test]
    fn announce_carries_peer_id() {
        let ann = SliverAnnounce {
            blob_id: "abcd".into(),
            shard_index: 2,
            node_id: "n1".into(),
            epoch: 5,
            peer_id: "12D3KooWExamplePeerIdExamplePeerIdExamplePeerIdExamplePeer".into(),
            node_key: "0x1".into(),
        };
        let v = serde_json::to_value(&ann).unwrap();
        assert!(v.get("peer_id").is_some());
        let back: SliverAnnounce = serde_json::from_value(v).unwrap();
        assert_eq!(back.peer_id, ann.peer_id);
    }
}
