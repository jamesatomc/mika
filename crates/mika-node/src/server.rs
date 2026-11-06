//! HTTP API for a Mika storage node.
//!
//! Endpoints:
//!   PUT   /slivers/{blob_id}/{shard_index}   store a sliver
//!   GET   /slivers/{blob_id}/{shard_index}   retrieve a sliver
//!   GET   /proofs/{blob_id}/{shard_index}    get a storage proof
//!   GET   /health                            liveness check

use axum::body::Bytes;
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, put},
};
use mika_storage::blob::BlobID;
use mika_storage::sliver::Sliver;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::proof::StorageProof;
use crate::store::SliverStore;

pub type AppState = Arc<NodeState>;

#[derive(Clone)]
pub struct NodeState {
    pub node_id: String,
    pub store: SliverStore,
    pub epoch: u64,
}

impl NodeState {
    pub fn new(node_id: String, store: SliverStore, epoch: u64) -> Self {
        Self {
            node_id,
            store,
            epoch,
        }
    }
}

#[derive(Deserialize)]
pub struct SliverPathParams {
    blob_id: String,
    shard_index: u32,
}

#[derive(Serialize)]
pub struct PutResponse {
    pub ok: bool,
    pub blob_id: String,
    pub shard_index: u32,
    pub node_id: String,
}

#[derive(Serialize)]
pub struct ProofResponse {
    pub proof: StorageProof,
}

/// `PUT /slivers/{blob_id}/{shard_index}` — store a sliver on this node.
async fn put_sliver(
    State(state): State<AppState>,
    Path(params): Path<SliverPathParams>,
    body: Bytes,
) -> Result<Json<PutResponse>, StatusCode> {
    let blob_id = BlobID::try_from(params.blob_id.as_str()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let sliver = Sliver::from_bytes(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if sliver.blob_id != blob_id || sliver.shard_index != params.shard_index {
        return Err(StatusCode::BAD_REQUEST);
    }
    state
        .store
        .put(&sliver)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    info!(node = %state.node_id, blob = %blob_id, shard = sliver.shard_index, "stored sliver");
    Ok(Json(PutResponse {
        ok: true,
        blob_id: blob_id.to_hex(),
        shard_index: sliver.shard_index,
        node_id: state.node_id.clone(),
    }))
}

/// `GET /slivers/{blob_id}/{shard_index}` — retrieve a sliver.
async fn get_sliver(
    State(state): State<AppState>,
    Path(params): Path<SliverPathParams>,
) -> Result<axum::body::Bytes, StatusCode> {
    let blob_id = BlobID::try_from(params.blob_id.as_str()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let sliver = state
        .store
        .get(&blob_id, params.shard_index)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::body::Bytes::from(
        sliver
            .to_bytes()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

/// `GET /proofs/{blob_id}/{shard_index}` — storage proof.
async fn get_proof(
    State(state): State<AppState>,
    Path(params): Path<SliverPathParams>,
) -> Result<Json<ProofResponse>, StatusCode> {
    let blob_id = BlobID::try_from(params.blob_id.as_str()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let sliver = state
        .store
        .get(&blob_id, params.shard_index)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let proof = StorageProof::new(
        blob_id,
        params.shard_index,
        &state.node_id,
        state.epoch,
        &sliver.data,
    );
    Ok(Json(ProofResponse { proof }))
}

/// `GET /health`
async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "node_id": state.node_id,
        "epoch": state.epoch,
    }))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route(
            "/slivers/{blob_id}/{shard_index}",
            put(put_sliver).get(get_sliver),
        )
        .route("/proofs/{blob_id}/{shard_index}", get(get_proof))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_storage::erasure::{ErasureParams, encode};
    use mika_storage::sliver::Sliver;
    use tempfile::tempdir;

    #[tokio::test]
    async fn node_api_round_trip() {
        let dir = tempdir().unwrap();
        let store = SliverStore::new(dir.path().to_path_buf()).unwrap();
        let state = Arc::new(NodeState::new("node-1".into(), store, 1));
        let _app = router(state.clone());

        let blob_id = BlobID::from_content(b"api test");
        let params = ErasureParams::new(3, 2).unwrap();
        let shards = encode(&vec![vec![9u8; 64]; 3], &params).unwrap();
        let sliver = Sliver::new(blob_id, 0, 5, 3, 2, 64, shards[0].clone());

        // Verify store state after a simulated put.
        state.store.put(&sliver).await.unwrap();
        assert!(state.store.has(&blob_id, 0).await);
        let back = state.store.get(&blob_id, 0).await.unwrap().unwrap();
        assert_eq!(back.data, shards[0]);
    }
}
