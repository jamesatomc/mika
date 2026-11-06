# Mika Storage — User Manual (English)

Mika is a minimal decentralized storage prototype. A blob is split into `k` data shards plus `m` parity shards (systematic Reed–Solomon over GF(2^8)), distributed across storage nodes, and can be reconstructed from any `k` surviving shards.

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
- `<shard_index>` — shard index (positional, not a flag)
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
# 1. Upload
cargo run -p mika-cli -- upload --file myfile.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2 --fee 1000

# 2. Proof a shard (uses the stored sliver)
cargo run -p mika-cli -- proof <BLOB_ID> 0 --node-addr 127.0.0.1:8080

# 3. Download and verify
cargo run -p mika-cli -- download <BLOB_ID> --output recovered.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2
fc myfile.txt recovered.txt
```

## How It Works

1. **Split** — `split_into_shards` divides the blob into `k` equal-size data shards, zero-padding the last shard if needed.
2. **Encode** — `encode` computes `m` parity shards using GF(256) Reed–Solomon coefficients `α^((p+1)·d)`. The first `k` output shards are the data; the rest are parity.
3. **Distribute** — `put_blob` stores each encoded shard on a node (round-robin) and collects write certificates.
4. **Retrieve** — `get_blob` gathers at least `k` available shards, builds the `k×k` system matrix, solves it via Gaussian elimination over GF(256), and rejoins the data shards, trimming padding to the original length.
5. **Persist** — `MikaClient::with_store_path` saves the in-memory sliver store to `./mika-store.json` so uploads survive across separate CLI invocations (upload in one process, proof/download in another).

## Requirements

- `data_shards + parity_shards <= 256` (GF(256) field size limit)
- `data_shards > 0` and `parity_shards > 0`
- At least `k` shards must be available to reconstruct

## Known Limitations

- The demo `MikaClient` is an in-memory HTTP-simulated store; real node-to-node HTTP is not yet implemented.
- Cross-process persistence relies on `./mika-store.json` written to the current working directory.
