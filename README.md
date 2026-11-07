# Mika Storage — User Manual (English)

Mika is a minimal decentralized storage prototype. A blob is split into `k` data shards plus `m` parity shards (systematic Reed–Solomon over GF(2^8)), distributed across storage nodes, and can be reconstructed from any `k` surviving shards.

## Storage Engine: Fjall

Mika uses [fjall](https://fjall-rs.github.io/) (v3), an embeddable LSM-tree key-value store written in Rust, for on-disk sliver persistence. Fjall replaces the previous JSON file-based approach and provides:

- **ACID transactions** with crash safety
- **LSM-tree** storage for high write throughput
- **Key-value separation** for efficient large value storage
- **Automatic compaction** and garbage collection
- **Concurrent reads** with snapshot isolation

## Kanari SDK Integration (v0.3.9-alpha)

Mika is built as part of the [kanari-sdk](https://github.com/kanari-network/kanari-sdk) workspace. The SDK crates are pinned to tag `v0.3.9-alpha`:

| Crate | Purpose |
|-------|---------|
| `kanari-crypto` | Key generation (K256, P256, Ed25519, Dilithium, SPHINCS+), signing, verification, encryption |
| `kanari-types` | Rust bindings for Kanari Move modules (Address, Coin, Transfer, CurrencyMetadata) |
| `kanari-core` | `BlockchainEngine` for on-chain transaction execution, checkpoints, state roots |
| `kanari-rpc-client` | RPC client for interacting with kanari nodes |
| `kanari-common` | Config management, address handling, environment configuration |

### Storage Certificates Signed with kanari-crypto

Write certificates are signed using `kanari_crypto::signatures::sign_message` and verified with `verify_signature_with_curve`. Each certificate includes the signer's public key and curve type.

### Keypair Loading via kanari-common

The signing keypair is loaded from the kanari config at `~/.kanari/kanari_config/`:

- `kanari.yaml` — config with `keystore_path`, `active_address`, and `envs`
- `kanari.keystore` — encrypted keystore with private keys (password-protected)

`KanariStorage::from_config(password, node_id)`:
1. Loads config via `kanari_common::load_kanari_config()`
2. Gets `active_address` via `kanari_common::get_active_address()`
3. Loads keystore from `keystore_path` in config
4. Decrypts the active address's private key with `kanari_crypto::encryption::decrypt_data()` using your password
5. Derives the public key for verification

The `kanari.yaml` on this system has:
- `active_address: 0x3ba63b92aac5f2bff87e580e820b61faf1c5fe9ae12f0bc8addd931a340b3146`
- `keystore_path: C:\Users\TaroPukpui\.kanari\kanari_config\kanari.keystore`

## Build

The `StoragePayment` trait (`mika-storage::onchain`) defines the interface for:
- `pay_storage_rent` — pay storage rent via native KANARI coin transfer
- `anchor_blob` — anchor blob metadata (owner, commitment, lease) on-chain
- `transfer_blob_ownership` — transfer blob ownership with rent transfer

`KanariStorage` (`mika-storage::kanari_integration`) implements `StoragePayment` against `kanari_core::BlockchainEngine`. It wraps an in-memory or persistent `BlockchainEngine` and a signing keypair.

## Build

```powershell
cargo build --release
```

Binaries:

- `target/release/mika-cli` — CLI tool (upload, download, proof, node, anchor)
- `target/release/mika-node` — storage node server

Build with on-chain support:

```powershell
cargo build --release -p mika-cli --features onchain
```

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

### KeyConfig — Load keypair from kanari config

Mika loads the signing keypair from the kanari config file managed by `kanari-common`:

```
C:\Users\TaroPukpui\.kanari\kanari_config\kanari.yaml
```

The config file should contain the user's private key hex string. This is used by
`KanariStorage::from_config()` for signing storage certificates and on-chain anchoring.

The config path is resolved via `kanari_common::get_kanari_config_path()` and
loaded via `kanari_common::load_kanari_config()`.

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

### Anchor — Anchor blob metadata on-chain

```powershell
cargo run -p mika-cli -- features onchain -- anchor <blob_id> <owner_address> <commitment_hex> --password <keystore_password> --fee 1000
```

The `--password` is used to decrypt your private key from `kanari.keystore`.

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
- `KanariStorage::anchor_blob`, `pay_storage_rent`, and `transfer_blob_ownership` are stubs awaiting `BlockchainEngine` transaction submission API.
