# Mika Storage — Deployment Guide (English)

## Overview

Mika is an erasure-coded decentralized storage system built on the Kanari SDK. Files are split into `k` data shards + `m` parity shards (Reed-Solomon over GF(2^8)), stored across nodes, and can be reconstructed from any `k` surviving shards. All metadata and ownership are anchored on-chain via the Kanari Move module.

## Prerequisites

- Rust toolchain (cargo, rustc)
- Kanari CLI (`kanari`) installed
- Kanari config at `C:\Users\TaroPukpui\.kanari\kanari_config\kanari.yaml`
- Kanari keystore at `C:\Users\TaroPukpui\.kanari\kanari_config\kanari.keystore`

## Step 1: Build the Rust Crates

```powershell
# Build all crates with on-chain support
cargo build --release --all-features

# Binaries:
#   target/release/mika-cli   - CLI tool
#   target/release/mika-node  - Storage node server
```

## Step 2: Deploy the Move Module

```powershell
# Navigate to the Move package
cd move\mika

# Build the Move module
kanari move build

# Publish the module to kanari blockchain
kanari move publish --package-path . --gas-budget 10000000
```

This publishes the `mika::storage` module to address `0x1`. Note the **Module ID** returned.

## Step 3: Initialize the Storage Object

After publishing, call `init` to create the Storage singleton:

```powershell
# Call init via CLI (requires kanari-sdk RPC client)
# The init function creates Storage object at sender's address
cargo run -p mika-cli -- features onchain -- init-storage --sender <YOUR_ADDRESS> --password <keystore_password>
```

The `StorageInitializedEvent` contains the Storage object ID. Save it — you'll need it for all subsequent operations.

## Step 4: Run the Storage Node

```powershell
cargo run -p mika-node -- --node-id node-1 --data-dir ./mika-data --listen-addr 127.0.0.1:8080
```

The node serves:
- **Sliver API**: `PUT/GET /slivers/{blob_id}/{shard_index}`, `GET /proofs/{blob_id}/{shard_index}`
- **Web API**: `POST /api/v1/upload`, `GET /api/v1/download/{blob_id}`, `GET /api/v1/proof/{blob_id}/{shard_index}`

## Step 5: Upload a File

```powershell
cargo run -p mika-cli -- upload --file myfile.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2 --fee 1000
```

Returns a `Blob ID`. Save it.

## Step 6: Anchor Blob On-Chain

```powershell
cargo run -p mika-cli -- features onchain -- anchor <BLOB_ID> <OWNER_ADDRESS> <COMMITMENT_HEX> --password <keystore_password> --fee 1000
```

This submits a signed transaction to call `mika::storage::anchor_blob` on the blockchain.

## Step 7: Verify / Download

```powershell
# Download the file
cargo run -p mika-cli -- download <BLOB_ID> --output recovered.txt --nodes 127.0.0.1:8080 --data-shards 3 --parity-shards 2

# Get storage proof
cargo run -p mika-cli -- proof <BLOB_ID> 0 --node-addr 127.0.0.1:8080
```

## Move Module API

| Function | Description |
|----------|-------------|
| `init(sender: address): Storage` | Genesis — creates Storage singleton |
| `anchor_blob(storage, blob_id, owner, commitment, epoch, fee_paid)` | Register blob metadata |
| `pay_storage_rent(storage, blob_id, amount, payer)` | Pay storage rent |
| `transfer_blob_ownership(storage, blob_id, from, to, amount)` | Transfer ownership |
| `get_blob_anchor(storage, blob_id)` | Query blob metadata |
| `is_blob_anchored(storage, blob_id)` | Check existence |

## On-Chain Storage Layout

Each blob is stored as a `BlobAnchor` object in the `Storage` singleton's vector:

```
Storage (singleton object)
├── id: address (set at genesis)
└── anchors: vector<BlobAnchor>
    └── BlobAnchor {
          id: u64 (auto-increment index),
          blob_id: vector<u8> (32 bytes),
          owner: address,
          commitment: vector<u8>,
          epoch: u64,
          fee_paid: u64,
          created_at: u64
        }
```

## Sliver Storage Layout (Fjall)

Each sliver is stored as key-value in fjall LSM-tree at `./mika-data/`:

- **Key**: `blob_id (32 bytes) || shard_index (4 bytes LE)` = 36 bytes
- **Value**: bcs-serialized `Sliver` struct

## Key Management

The signing keypair is loaded from the kanari config:

```
C:\Users\TaroPukpui\.kanari\kanari_config\
├── kanari.yaml        # config: active_address, keystore_path
└── kanari.keystore    # encrypted keystore (password-protected)
```

`KanariStorage::from_config(password, node_id)`:
1. Loads config via `kanari_common::load_kanari_config()`
2. Gets `active_address` via `kanari_common::get_active_address()`
3. Loads keystore from `keystore_path`
4. Decrypts the active address's private key using `kanari_crypto::encryption::decrypt_data()`
5. Derives public key for signature verification

## Known Limitations

- `Storage` object must be deployed via genesis `init` before `anchor_blob`/`pay_storage_rent`/`transfer_blob_ownership` can be used
- Storage object ID must be passed to `KanariStorage::with_storage_object_id()`
- Single-node storage (all shards on one node); multi-node replication not yet implemented
- Web API uses fixed erasure parameters (3 data + 2 parity shards)
