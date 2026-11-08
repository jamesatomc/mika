//! libp2p node-to-node control plane for Mika (modeled on `kanari-node::p2p`).
//!
//! - Data plane stays HTTP (`PUT/GET /slivers/...`) — sliver bytes never go
//!   over gossipsub.
//! - Control plane is libp2p: `identify` + `noise` (ed25519) + `yamux`,
//!   `gossipsub` (signed) for `mika/slivers/v1` announcements, `kad`
//!   (DHT) for blob→node discovery, `ping`/`dcutr`/`relay` for connectivity.

use anyhow::Result;
use futures::{FutureExt, StreamExt};
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
use libp2p_swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p_tcp as tcp;
use libp2p_yamux as yamux;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info, warn};

pub const SLIVERS_TOPIC: &str = "mika/slivers/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SliverAnnounce {
    pub blob_id: String,
    pub shard_index: u32,
    pub node_id: String,
    pub epoch: u64,
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
}

pub struct MikaP2P {
    pub swarm: Swarm<MikaBehaviour>,
    pub topic: IdentTopic,
    pub peer_id: PeerId,
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
        let mut gossipsub =
            gossipsub::Behaviour::new(MessageAuthenticity::Signed(keypair.clone()), config)
                .map_err(|e| anyhow::anyhow!("Failed to create gossipsub: {}", e))?;
        let topic = IdentTopic::new(SLIVERS_TOPIC);
        gossipsub.subscribe(&topic)?;

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
            peer_id: local_peer_id,
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

    /// Drain pending swarm events synchronously (no await).
    /// Data-plane slivers stay over HTTP; this only logs announcements.
    pub fn poll_once_sync(&mut self) {
        use futures::StreamExt;
        while let Some(Some(event)) = self.swarm.next().now_or_never() {
            match event {
                SwarmEvent::Behaviour(MikaBehaviourEvent::Gossipsub(
                    gossipsub::Event::Message { message, .. },
                )) => {
                    if let Ok(ann) = serde_json::from_slice::<SliverAnnounce>(&message.data) {
                        info!(
                            "mika p2p: sliver {}#{} at {} (epoch {})",
                            ann.blob_id, ann.shard_index, ann.node_id, ann.epoch
                        );
                    }
                }
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    info!("mika p2p: connected {peer_id}")
                }
                _ => {}
            }
        }
    }

    /// Poll once (call in a tokio loop with the HTTP server).
    pub async fn poll_once(&mut self) {
        tokio::select! {
            event = self.swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(MikaBehaviourEvent::Gossipsub(gossipsub::Event::Message { message, .. })) => {
                    if let Ok(ann) = serde_json::from_slice::<SliverAnnounce>(&message.data) {
                        info!("mika p2p: sliver {}#{} at {} (epoch {})",
                            ann.blob_id, ann.shard_index, ann.node_id, ann.epoch);
                    }
                }
                SwarmEvent::ConnectionEstablished { peer_id, .. } => info!("mika p2p: connected {peer_id}"),
                _ => {}
            },
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn topic_name_is_namespaced() {
        assert!(super::SLIVERS_TOPIC.starts_with("mika/"));
    }
}
