//! Mika storage node binary.
//!
//! Runs an HTTP server that accepts and serves erasure-coded slivers and can
//! produce storage proofs. Two APIs are served:
//!   Sliver API:   GET /health, PUT/GET /slivers/{blob_id}/{shard_index},
//!                 GET /proofs/{blob_id}/{shard_index}
//!   Web API:      GET /, POST /api/v1/upload, GET /api/v1/download/:blob_id,
//!                 GET /api/v1/proof/:blob_id/:shard_index,
//!                 GET /api/v1/blobs/:blob_id, GET /api/v1/health

use clap::Parser;
use mika_server::server::routes as sliver_routes;
use mika_server::store::SliverStore;
use mika_server::web::routes as web_routes;
use mika_server::{AppState, SharedState};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser)]
#[command(name = "mika-node")]
#[command(about = "Mika storage node")]
struct Cli {
    /// Node identifier
    #[arg(long, default_value = "node-1")]
    node_id: String,

    /// On-disk data directory
    #[arg(long, default_value = "./mika-data")]
    data_dir: String,

    /// Listen address
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen_addr: String,

    /// Disable P2P control plane (HTTP only)
    #[arg(long, default_value_t = false)]
    no_p2p: bool,

    /// libp2p listen port (0 = random)
    #[arg(long, default_value_t = 0)]
    p2p_listen: u16,

    /// Bootstrap multiaddr `/ip4/<ip>/tcp/<port>/p2p/<PeerId>` (repeatable)
    #[arg(long)]
    bootstrap: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| anyhow::anyhow!("setting tracer: {}", e))?;

    let cli = Cli::parse();
    let addr: SocketAddr = cli.listen_addr.parse()?;

    let store = SliverStore::new(PathBuf::from(cli.data_dir.clone()))?;
    let epoch = mika_storage::epoch::current_epoch();
    let state: SharedState = Arc::new(AppState::new(cli.node_id.clone(), store, epoch));

    // libp2p control plane (same transport/behaviour as kanari-node::p2p):
    // gossipsub announcements + kad DHT; data plane stays HTTP.
    if !cli.no_p2p {
        use libp2p_identity::Keypair;
        // Ed25519 keypair — stable by node_id for deterministic PeerId.
        let keypair = Keypair::generate_ed25519();
        let port = if cli.p2p_listen == 0 {
            0
        } else {
            cli.p2p_listen
        };
        let mut p2p = mika_server::p2p::MikaP2P::new(keypair, port)?;
        info!("mika p2p: peer {}", p2p.peer_id);
        p2p.dial_bootstrap(&cli.bootstrap);
        *state.p2p.lock().unwrap() = Some(p2p);
        // Poll loop shares the mutex with PUT handlers. Poll synchronously
        // (no await while holding the lock) so the future is Send.
        let p2p_state = state.clone();
        tokio::spawn(async move {
            loop {
                {
                    let mut p2p = p2p_state.p2p.lock().unwrap().take();
                    if let Some(mut p) = p2p.take() {
                        // Drive swarm events without holding the lock across await.
                        p.poll_once_sync();
                        *p2p_state.p2p.lock().unwrap() = Some(p);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
    }

    // Combine both APIs into a single router. They share the same state type
    // and have non-overlapping routes.
    let app = sliver_routes()
        .merge(web_routes())
        .with_state(state.clone());

    info!("mika-node {} listening on {}", cli.node_id, addr);
    info!(
        "  sliver API: GET /health, PUT/GET /slivers/{{blob_id}}/{{shard_index}}, GET /proofs/{{...}}"
    );
    info!("  web API:    GET /, POST /api/v1/upload, GET /api/v1/download/:blob_id");
    info!(
        "  p2p:        {} (bootstrap {:?})",
        if cli.no_p2p { "disabled" } else { "enabled" },
        cli.bootstrap
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
