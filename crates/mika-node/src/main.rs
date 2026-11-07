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
use mika_node::server::routes as sliver_routes;
use mika_node::store::SliverStore;
use mika_node::web::routes as web_routes;
use mika_node::{AppState, SharedState};
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

    // Combine both APIs into a single router. They share the same state type
    // and have non-overlapping routes.
    let app = sliver_routes().merge(web_routes()).with_state(state);

    info!("mika-node {} listening on {}", cli.node_id, addr);
    info!(
        "  sliver API: GET /health, PUT/GET /slivers/{{blob_id}}/{{shard_index}}, GET /proofs/{{...}}"
    );
    info!("  web API:    GET /, POST /api/v1/upload, GET /api/v1/download/:blob_id");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
