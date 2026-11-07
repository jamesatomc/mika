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

The binary is `target/release/mika-cli`.

## CLI Usage

```powershell
cargo run -p mika-cli -- <command> [options]
```

### 1. Upload a file

```powershell
cargo run -p mika-cli -- upload --file myfile.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2 --fee 1000
```

Options:

- `--file` — path to the file to upload (required)
- `--nodes` — storage node address(es), comma-separated (default `127.0.0.1:8080`)
- `--data-shards` — number of data shards `k` (default `3`)
- `--parity-shards` — number of parity shards `m` (default `2`)
- `--fee` — storage fee in mist (default `1000`)

Example output:

```
Upload complete
  Blob ID: 27b8f6f7...
  Size: 32 bytes
  Shards: 3 data + 2 parity
  Nodes: 1
  Certificates collected: 5
```

### 2. Download a file

```powershell
cargo run -p mika-cli -- download <blob_id> --output out.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2
```

- `<blob_id>` — hex-encoded blob ID returned by upload
- `--output` — destination file path

### 3. Start a storage node

```powershell
cargo run -p mika-cli -- node --node-id node-1 --data-dir ./mika-data --listen-addr 127.0.0.1:8080
```

Options:

- `--node-id` — node identifier (default `node-1`)
- `--data-dir` — on-disk data directory (default `./mika-data`)
- `--listen-addr` — listen address (default `127.0.0.1:8080`)

### 4. Get a storage proof for a sliver

```powershell
cargo run -p mika-cli -- proof <blob_id> <shard_index> --node-addr 127.0.0.1:8080
```

- `<blob_id>` — hex-encoded blob ID
- `<shard_index>` — shard index (positional argument, not a flag)
- `--node-addr` — node address (default `127.0.0.1:8080`)

Example output:

```json
{
  "blob_id": "...",
  "epoch": 1793987101,
  "node_id": "127.0.0.1:8080",
  "shard_index": 0,
  "sliver_digest": "7d8081c2...",
  "sliver_size": 11
}
```

## End-to-End Example

```powershell
# 1. Upload (creates ./mika-store/ fjall database)
cargo run -p mika-cli -- upload --file myfile.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2 --fee 1000

# 2. Proof a shard (reads from ./mika-store/)
cargo run -p mika-cli -- proof <BLOB_ID> 0 --node-addr 127.0.0.1:8080

# 3. Download and verify (reads from ./mika-store/)
cargo run -p mika-cli -- download <BLOB_ID> --output recovered.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2
fc myfile.txt recovered.txt
```

## Persistence

Slivers are stored in a fjall database at `./mika-store/` (relative to the working directory). The directory is created automatically on first upload. Because fjall is an embedded KV store, data persists across separate CLI invocations — you can upload in one process and then proof/download in another.

## Storage Layout

Each sliver is stored as a key-value pair in fjall:

- **Key**: `blob_id (32 bytes) || shard_index (4 bytes little-endian)` = 36 bytes
- **Value**: bcs-serialized `Sliver` struct

Prefix scans on `blob_id` bytes are used to list all shard indices for a given blob.

## Requirements

- `data_shards + parity_shards <= 256` (GF(256) field size limit)
- `data_shards > 0` and `parity_shards > 0`
- At least `k` shards must be available to reconstruct

## Known Limitations

- The demo `MikaClient` uses fjall for local persistence; real node-to-node HTTP replication is not yet implemented.
- The fjall database directory (`./mika-store/`) is not automatically cleaned up between runs.
- Cross-process persistence relies on the shared `./mika-store/` directory.
