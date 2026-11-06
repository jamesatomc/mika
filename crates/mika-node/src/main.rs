//! Mika storage node binary.
//!
//! Runs an HTTP server that accepts and serves erasure-coded slivers and can
//! produce storage proofs.

use mika_node::server::{NodeState, router};
use mika_node::store::SliverStore;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| anyhow::anyhow!("setting tracer: {}", e))?;

    let node_id = std::env::var("MIKA_NODE_ID").unwrap_or_else(|_| "node-1".into());
    let data_dir = std::env::var("MIKA_DATA_DIR").unwrap_or_else(|_| "./mika-data".into());
    let addr: SocketAddr = std::env::var("MIKA_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;

    let store = SliverStore::new(std::path::PathBuf::from(data_dir))?;
    let state = Arc::new(NodeState::new(
        node_id.clone(),
        store,
        mika_storage::epoch::current_epoch(),
    ));

    let app = router(state.clone());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("mika-node {} listening on {}", node_id, addr);
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
