# Mika Storage — User Manual (English)

Mika is a minimal decentralized storage prototype. A blob is split into `k` data shards plus `m` parity shards (systematic Reed–Solomon over GF(2^8)), distributed across storage nodes, and can be reconstructed from any `k` surviving shards.

## Storage Engine: Fjall

Mika uses [fjall](https://fjall-rs.github.io/) (v3), an embeddable LSM-tree key-value store written in Rust, for on-disk sliver persistence. Fjall replaces the previous JSON file-based approach and provides:

- **ACID transactions** with crash safety
- **LSM-tree** storage for high write throughput
- **Key-value separation** for efficient large value storage
- **Automatic compaction** and garbage collection
- **Concurrent reads** with snapshot isolation

## Build

```powershell
cargo build --release
```

Binaries:

- `target/release/mika-cli` — CLI tool (upload, download, proof, node)
- `target/release/mika-node` — storage node server

## Running the Storage Node

```powershell
cargo run -p mika-node -- --node-id node-1 --data-dir ./mika-data --listen-addr 127.0.0.1:8080
```

The node serves both APIs on the same port:

| API            | Endpoints                                                                                                                                                          |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Sliver API** | `GET /health`, `PUT/GET /slivers/{blob_id}/{shard_index}`, `GET /proofs/{blob_id}/{shard_index}`                                                                   |
| **Web API**    | `GET /`, `POST /api/v1/upload`, `GET /api/v1/download/{blob_id}`, `GET /api/v1/proof/{blob_id}/{shard_index}`, `GET /api/v1/blobs/{blob_id}`, `GET /api/v1/health` |

Options:

- `--node-id` — node identifier (default `node-1`)
- `--data-dir` — on-disk data directory (default `./mika-data`)
- `--listen-addr` — listen address (default `127.0.0.1:8080`)

## Web API (Browser-Ready)

The web API includes CORS headers so browsers can call it directly. A single-page HTML frontend is served at `GET /`.

### 1. Upload a File

```powershell
curl -X POST -F "file=@myfile.txt" http://127.0.0.1:8080/api/v1/upload
```

Response:

```json
{
  "ok": true,
  "blob_id": "27b8f6f78d134903727959b38de290371b6793a760055d669e11802b2bb896f7",
  "filename": "myfile.txt",
  "size": 32,
  "shards": "3 data + 2 parity",
  "nodes": 1
}
```

### 2. Download a File

```powershell
curl http://127.0.0.1:8080/api/v1/download/27b8f6f7... > recovered.txt
```

### 3. Storage Proof

```powershell
curl http://127.0.0.1:8080/api/v1/proof/27b8f6f7.../0
```

Response:

```json
{
  "blob_id": "27b8f6f7...",
  "shard_index": 0,
  "node_id": "node-1",
  "epoch": 1794030586,
  "sliver_digest": "7d8081c24869765dcb79490332667118681c3908ac719bee3a8dc54c5c4cb4fd",
  "sliver_size": 11,
  "original_len": 32,
  "data_shards": 3,
  "parity_shards": 2
}
```

### 4. Blob Metadata

```powershell
curl http://127.0.0.1:8080/api/v1/blobs/27b8f6f7...
```

## CLI Usage

```powershell
cargo run -p mika-cli -- <command> [options]
```

### Upload

```powershell
cargo run -p mika-cli -- upload --file myfile.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2 --fee 1000
```

### Download

```powershell
cargo run -p mika-cli -- download <blob_id> --output out.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2
```

### Proof

```powershell
cargo run -p mika-cli -- proof <blob_id> <shard_index> --node-addr 127.0.0.1:8080
```

### Run Node

```powershell
cargo run -p mika-cli -- node --node-id node-1 --data-dir ./mika-data --listen-addr 127.0.0.1:8080
```

## Persistence

Slivers are stored in a fjall database at `./mika-data/` (or `--data-dir`). Data persists across separate CLI invocations and server restarts.

## Storage Layout

Each sliver is stored as a key-value pair in fjall:

- **Key**: `blob_id (32 bytes) || shard_index (4 bytes little-endian)` = 36 bytes
- **Value**: bcs-serialized `Sliver` struct

Prefix scans on `blob_id` bytes list all shard indices for a blob.

## Requirements

- `data_shards + parity_shards <= 256` (GF(256) field size limit)
- `data_shards > 0` and `parity_shards > 0`
- At least `k` shards must be available to reconstruct

## Known Limitations

- Real node-to-node HTTP replication is not yet implemented (single node stores all shards locally).
- The web API uses fixed erasure parameters (3 data + 2 parity shards).
