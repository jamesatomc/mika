//! Web-facing API for Mika storage.
//!
//! Provides CORS-enabled HTTP endpoints so browsers can upload, download, and
//! verify erasure-coded blobs.

use axum::{
    Router,
    body::Body,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
};
use kanari_crypto::hash_data_blake3_array;
use mika_types::blob::BlobID;
use mika_core::erasure::{ErasureParams, decode, encode, rejoin_shards, split_into_shards};
use mika_types::sliver::Sliver;
use serde::Serialize;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use crate::SharedState;

#[derive(Serialize)]
pub struct UploadResponse {
    pub ok: bool,
    pub blob_id: String,
    pub filename: String,
    pub size: usize,
    pub shards: String,
    pub nodes: usize,
}

/// Build the web API routes with CORS enabled for browser access.
pub fn routes() -> Router<SharedState> {
    Router::new()
        .route("/", get(serve_index))
        .route("/api/v1/health", get(health))
        .route("/api/v1/upload", post(upload_file))
        .route("/api/v1/download/{blob_id}", get(download_file))
        .route("/api/v1/proof/{blob_id}/{shard_index}", get(get_proof))
        .route("/api/v1/blobs/{blob_id}", get(get_blob_meta))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
}

/// Serve the single-page HTML frontend.
pub async fn serve_index() -> Html<&'static str> {
    Html(include_str!("../web_ui.html"))
}

/// `GET /api/v1/health` — liveness check.
pub async fn health(State(_state): State<SharedState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "mika-web",
    }))
}

/// `POST /api/v1/upload` — upload a file (multipart/form-data).
pub async fn upload_file(
    State(state): State<SharedState>,
    mut multipart: Multipart,
) -> Result<Response, StatusCode> {
    let mut filename = String::new();
    let mut data: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name == "file" {
            filename = field
                .file_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "upload".to_string());
            data = Some(
                field
                    .bytes()
                    .await
                    .map_err(|_| StatusCode::BAD_REQUEST)?
                    .to_vec(),
            );
        }
    }

    let data = data.ok_or(StatusCode::BAD_REQUEST)?;
    if data.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let params = ErasureParams::new(3, 2).map_err(|_| StatusCode::BAD_REQUEST)?;

    let blob = mika_types::blob::Blob::new(data.clone());
    let shards = split_into_shards(&blob.data, params.data_shards)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let encoded = encode(&shards, &params).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Store each sliver locally.
    for (i, shard) in encoded.iter().enumerate() {
        let sliver = Sliver::new(
            blob.id,
            i as u32,
            params.total_shards() as u32,
            params.data_shards as u32,
            params.parity_shards as u32,
            blob.data.len() as u64,
            shard.clone(),
        );
        state
            .store
            .put(&sliver)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    info!(node = %state.node_id, blob = %blob.id, size = blob.data.len(), "web upload stored");

    Ok(Json(UploadResponse {
        ok: true,
        blob_id: blob.id.to_hex(),
        filename,
        size: blob.data.len(),
        shards: format!("{} data + {} parity", 3, 2),
        nodes: 1,
    })
    .into_response())
}

/// `GET /api/v1/download/:blob_id` — reconstruct and download a file.
pub async fn download_file(
    State(state): State<SharedState>,
    Path(blob_id_hex): Path<String>,
) -> Result<Response, StatusCode> {
    let blob_id = BlobID::try_from(blob_id_hex.as_str()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let params = ErasureParams::new(3, 2).map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut present: Vec<Option<Vec<u8>>> = vec![None; params.total_shards()];
    let mut original_len: Option<usize> = None;

    for (i, present_slot) in present.iter_mut().enumerate() {
        if let Some(sliver) = state
            .store
            .get(&blob_id, i as u32)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        {
            if original_len.is_none() {
                original_len = Some(sliver.original_len as usize);
            }
            *present_slot = Some(sliver.data);
        }
    }

    let available = present.iter().filter(|o| o.is_some()).count();
    if available < params.data_shards {
        return Err(StatusCode::NOT_FOUND);
    }

    let data_shards = decode(&present, &params).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let original_len = original_len.unwrap_or_else(|| data_shards.iter().map(|s| s.len()).sum());
    let data = rejoin_shards(&data_shards, original_len);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .header("content-length", data.len().to_string())
        .body(Body::from(data))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// `GET /api/v1/proof/:blob_id/:shard_index` — storage proof for a sliver.
pub async fn get_proof(
    State(state): State<SharedState>,
    Path((blob_id_hex, shard_index)): Path<(String, u32)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let blob_id = BlobID::try_from(blob_id_hex.as_str()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let sliver = state
        .store
        .get(&blob_id, shard_index)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let digest = hash_data_blake3_array(&sliver.data);
    let digest_hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();

    Ok(Json(serde_json::json!({
        "blob_id": blob_id.to_hex(),
        "shard_index": shard_index,
        "node_id": state.node_id,
        "epoch": mika_types::epoch::current_epoch(),
        "sliver_digest": digest_hex,
        "sliver_size": sliver.data.len(),
        "original_len": sliver.original_len,
        "data_shards": sliver.data_shards,
        "parity_shards": sliver.parity_shards,
    })))
}

/// `GET /api/v1/blobs/:blob_id` — blob metadata.
pub async fn get_blob_meta(
    State(state): State<SharedState>,
    Path(blob_id_hex): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let blob_id = BlobID::try_from(blob_id_hex.as_str()).map_err(|_| StatusCode::BAD_REQUEST)?;

    let sliver = state
        .store
        .get(&blob_id, 0)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(serde_json::json!({
        "blob_id": blob_id.to_hex(),
        "original_len": sliver.original_len,
        "data_shards": sliver.data_shards,
        "parity_shards": sliver.parity_shards,
        "total_shards": sliver.total_shards,
        "shard_size": sliver.data.len(),
    })))
}
