//! Mika JSON-RPC client (kanari-sdk style: `{jsonrpc, method, params, id}`).
//!
//! Mirrors `kanari_rpc_client::RpcClient`: `new(url)` → typed methods that
//! POST to `POST /rpc` and map `error.code/message` to `MikaStorageError`.

use mika_types::error::{MikaStorageError, Result};
use reqwest::Client;
use serde_json::{Value, json};

pub struct MikaRpcClient {
    client: Client,
    url: String,
    request_id: std::sync::Mutex<u64>,
}

impl MikaRpcClient {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            url: url.into(),
            request_id: std::sync::Mutex::new(1),
        }
    }

    fn next_id(&self) -> u64 {
        let mut g = self.request_id.lock().unwrap();
        let id = *g;
        *g += 1;
        id
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id();
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });
        let resp = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| MikaStorageError::NodeUnavailable(format!("RPC: {e}")))?;
        let v: Value = resp
            .json()
            .await
            .map_err(|e| MikaStorageError::Serialization(e.to_string()))?;
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("error");
            return Err(MikaStorageError::NodeUnavailable(format!(
                "RPC error {code}: {msg}"
            )));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn get_health(&self) -> Result<Value> {
        self.request("getHealth", json!({})).await
    }

    pub async fn get_blob_shards(&self, blob_id: &mika_types::blob::BlobID) -> Result<Vec<u32>> {
        let v = self
            .request("getBlob", json!({ "blob_id": blob_id.to_hex() }))
            .await?;
        Ok(v.get("shards")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_u64().map(|n| n as u32))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn list_blobs(&self, limit: usize, offset: usize) -> Result<Value> {
        self.request("listBlobs", json!({ "limit": limit, "offset": offset }))
            .await
    }
}
