# Mika Storage — Implementation Summary

This document summarizes all changes made to the Mika storage system to align it with Walrus-style architecture, including BFT quorum signing, Red Stuff 2D Merkle commitment, reclaim, and upgrade operations.

---

## 1. Clippy Warning Fixes

### `crates/mika-cli/src/main.rs`
- Replaced all 28 `println!` calls with `eprintln!` to resolve `clippy::print-stdout` warnings.
- Consistent with existing `eprintln!` usage (e.g. line 269).

### `crates/mika-storage/src/erasure.rs`
- Renamed `GFTables` → `GfTables` (all 5 references) to resolve `clippy::upper_case_acronyms`.

### `crates/mika-server/src/p2p.rs`
- Converted `loop { match ... break }` to `while let Some(Some(event)) = ...` to resolve `clippy::while_let_loop`.

---

## 2. BFT Quorum Signing (Certification Quorum)

### Problem
Previously, `put_blob` created unsigned placeholder certificates locally, then the CLI re-signed all of them with a single uploader key (`kv.sign_quorum`). This meant the quorum was signed by **one key**, not by distinct storage nodes — not real BFT.

### Solution
Each storage node now signs its own `WriteCertificate` when it stores a sliver. The client collects these node-signed certificates and verifies the quorum threshold before anchoring on-chain.

### Files Changed

**`crates/mika-server/src/lib.rs`** — `AppState`
- Added `private_key`, `public_key`, `curve_type` fields.
- Each node generates its own K256 keypair on startup via `generate_keypair(CurveType::K256)`.

**`crates/mika-server/src/server.rs`** — `put_sliver` handler
- `PUT /slivers/{blob_id}/{shard_index}` now:
  1. Stores the sliver.
  2. Creates a `WriteCertificate` with `sliver_hash = blake3(sliver.data)`.
  3. Signs the certificate with the node's private key.
  4. Returns the signed certificate in `PutResponse.certificate`.
- `PutResponse` now includes `certificate: WriteCertificate`.

**`crates/mika-core/src/storage_client.rs`** — `MikaClient`
- `http_put()` now returns `Result<Option<WriteCertificate>>` (parses the node's signed certificate from the HTTP response).
- `put_blob()`:
  - Collects node-signed certificates from HTTP responses.
  - Falls back to unsigned placeholder certificates for dev/local mode (no running nodes).
  - Enforces quorum: requires `data_shards` distinct nodes. If all certs are signed, verifies each signature. If any cert is unsigned, skips signature verification (dev mode).
  - Returns `InsufficientShards` error if quorum is not met.

**`crates/mika-cli/src/main.rs`** — Upload command
- Removed `kv.sign_quorum(&mut quorum)` (no longer re-signing with uploader key).
- Calls `mika_core::kanari_integration::verify_quorum(&quorum)` to validate node signatures.
- Reports quorum stats: cert count, distinct nodes, verified flag.
- Bails if quorum verification fails.

---

## 3. Red Stuff 2D Merkle Commitment

### Problem
The previous 2D commitment was a single-layer root-of-hashes (row roots hashed together). It could not verify a single sliver in isolation without all row roots — no Merkle proof path.

### Solution
Full Red Stuff 2D commitment with two Merkle tree layers:
- **Layer 1**: `blake3(shard)` for each data shard → leaves; Merkle tree → `data_root`.
- **Layer 2**: `blake3(shard)` for each parity shard → leaves; Merkle tree → `parity_root`.
- **Layer 3**: `blake3(k || m || data_root || parity_root)` → final on-chain `commitment`.

### Files Changed

**`crates/mika-core/src/commitment.rs`** — Complete rewrite
New types and functions:

| Type/Function | Description |
|---|---|
| `MerkleProof` | `leaf_index`, `siblings: Vec<[u8;32]>`, `root: [u8;32]` |
| `Commitment2D` | `data_roots`, `parity_roots`, `data_root`, `parity_root`, `commitment` |
| `commitment_2d(data, params)` → `Commitment2D` | Full 2D commitment from raw data |
| `build_commitment_2d(encoded, params)` → `Commitment2D` | From pre-encoded shards |
| `blob_commitment(data, params)` → `[u8;32]` | Backward-compatible hash only |
| `shard_proof(index, data_roots)` → `Option<MerkleProof>` | Generate Merkle proof for one data shard |
| `verify_shard_2d(shard, index, proof, parity_root, commitment, params)` → `bool` | Verify one shard with Merkle proof |
| `verify_shard(...)` | Deprecated flat root-of-roots verification (kept for compatibility) |

### Verification Flow
1. Storage node stores sliver → computes `blake3(shard)` → generates Merkle proof up to `data_root`.
2. Client collects `data_root`, `parity_root`, `commitment` from on-chain anchor.
3. Verifier calls `verify_shard_2d(shard, index, proof, parity_root, commitment, params)`:
   - Checks `blake3(shard)` matches proof leaf.
   - Verifies Merkle path up to `data_root` (from proof).
   - Checks `blake3(k || m || data_root || parity_root) == commitment`.

All existing tests updated; new tests for Merkle root (single/even/odd leaf counts), proof round-trip, tamper detection, and `verify_shard_2d` round-trip + wrong-index rejection.

---

## 4. Reclaim (Delete Expired Blob)

### Problem
The Move module already had `reclaim_blob` (storage.move:238-259) and `BlobReclaimedEvent`, but the Rust side had no bridge — no request struct, no trait method, no implementation, no CLI command.

### Solution
End-to-end reclaim: anchor removal + quota refund to Storage object.

### Files Changed

**`crates/mika-types/src/storage.rs`**
- `BlobReclaimedEvent` struct (`blob_id`, `reclaimed_at`, `quota_refund`).
- `ReclaimBlobRequest` struct (`storage_id: Vec<u8>`, `blob_id: Vec<u8>`, `now_epoch: u64`) with `encode_args()`.
- `reclaim_blob` added to `StorageFunctions` and `function_names()`.
- Test: `reclaim_args_encode_in_order`.

**`crates/mika-types/src/onchain.rs`**
- `reclaim_blob(&self, request: &ReclaimBlobRequest)` added to `StoragePayment` trait.

**`crates/mika-core/src/kanari_integration.rs`**
- `reclaim_blob` implemented on `KanariStorage` — builds and submits `reclaim_blob(storage_id, blob_id, now_epoch)` Move transaction via `BlockchainEngine`/RPC.

**`crates/mika-cli/src/main.rs`**
- New `Reclaim` subcommand:
  ```
  mika-cli reclaim <blob_id> --storage-object-id <id> --password <pw> [--now-epoch <ts>]
  ```

---

## 5. Upgrade (Blob Version Change)

### Problem
The Move module already had `upgrade_blob` (storage.move:197-232) and `BlobUpgradedEvent`, but the Rust side had no bridge.

### Solution
End-to-end upgrade: creates a new anchor for the same logical blob with new content (new blob_id). Old anchor stays as history; newest anchor for the owner wins.

### Files Changed

**`crates/mika-types/src/storage.rs`**
- `BlobUpgradedEvent` struct (`old_blob_id`, `new_blob_id`, `version`, `owner`, `fee_paid`).
- `UpgradeBlobRequest` struct (`storage_id`, `old_blob_id`, `new_blob_id`, `owner`, `commitment`, `epoch`, `fee_paid`) with `encode_args()`.
- `upgrade_blob` added to `StorageFunctions` and `function_names()`.
- Test: `upgrade_args_encode_in_order`.

**`crates/mika-types/src/onchain.rs`**
- `upgrade_blob(&self, request: &UpgradeBlobRequest)` added to `StoragePayment` trait.

**`crates/mika-core/src/kanari_integration.rs`**
- `upgrade_blob` implemented on `KanariStorage` — builds and submits `upgrade_blob(...)` Move transaction via `BlockchainEngine`/RPC.

**`crates/mika-cli/src/main.rs`**
- New `Upgrade` subcommand:
  ```
  mika-cli upgrade <old_blob_id> <new_blob_id> --owner <addr> --commitment <hex> --password <pw> --storage-object-id <id> [--fee <mist>]
  ```

---

## 6. Verification

### Clippy
```
cargo clippy --all-targets
```
- `mika-cli`: clean (no `print-stdout` warnings).
- `mika-core`, `mika-server`, `mika-storage`, `mika-types`: clean (only pre-existing warnings in unrelated test code).

### Tests
```
cargo test --all-targets
```
All tests pass across all crates:
- `mika-core`: 18 tests (including 2D commitment, Merkle proof, quorum, storage client).
- `mika-types`: 13 tests (including reclaim/upgrade arg encoding).
- `mika-storage`: 3 tests.
- `mika-server`: 3 tests.

---

## 7. CLI Command Reference

| Command | Purpose |
|---|---|
| `mika-cli buy --amount <mist> --password <pw> --storage-object-id <id>` | Buy storage quota on-chain |
| `mika-cli anchor <blob_id> <owner> <commitment> --password <pw> --fee <mist>` | Anchor blob metadata on-chain |
| `mika-cli upload <file> --nodes <addr,...> --data-shards <k> --parity-shards <m> --fee <mist> --password <pw> --storage-object-id <id>` | Upload + BFT quorum + certify on-chain |
| `mika-cli download <blob_id> --output <path> --nodes <addr,...>` | Download blob from storage nodes |
| `mika-cli reclaim <blob_id> --storage-object-id <id> --password <pw> [--now-epoch <ts>]` | Reclaim expired blob (quota refund) |
| `mika-cli upgrade <old_id> <new_id> --owner <addr> --commitment <hex> --password <pw> --storage-object-id <id> [--fee <mist>]` | Upgrade blob to new version |
| `mika-cli node [--node-id <id>] [--data-dir <path>] [--listen-addr <addr>] [--no-p2p]` | Start Mika storage node |
| `mika-cli proof <blob_id> <shard_index> --node-addr <addr>` | Get storage proof for a sliver |
