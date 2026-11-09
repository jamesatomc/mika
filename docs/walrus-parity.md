# Mika × Walrus Parity Guide

Status of Mika against [Walrus](https://docs.wal.app) (Mysten Labs' decentralized storage on Sui).

## Implemented and verified

| Area | Status | Details |
|---|---|---|
| Store → certify → PoA flow | ✅ | Client encodes → pushes to entry peer → quorum verify → `anchor_blob` (records `fee_paid`) → aggregated PoA submitted on-chain |
| Off-chain slivers, on-chain metadata | ✅ | Sliver bytes live on nodes' fjall stores; chain holds only anchors/commitments |
| **Red Stuff 2D** | ✅ | `mika_core::erasure::redstuff_{encode,decode,recover_primary}` — primary + secondary grids, RS over GF(2⁸), any k of either grid decodes, self-healing recovery from O(k) secondaries. CLI index space `0..2n` (primary `0..n`, secondary `n..2n`) |
| 2D Merkle commitment | ✅ | `mika_core::commitment` — per-sliver verifiable 2D commitment, used for `anchor_blob` |
| PQ (Dilithium) | ✅ | `--blob-type pq` signs payload with Dilithium3 |
| Quota / buy storage | ✅ | `buy_storage` tops up the shared `Storage.quota` |
| Reclaim / prune | ✅ | `reclaim_blob` refunds quota; node reclaim loop sweeps expired leases |
| Epoch leases | ✅ | Fee buys whole epochs (`EPOCH_SECONDS`); lease expiry tracked per blob |
| P2P dissemination | ✅ | libp2p gossipsub announce + request-response fetch/store — uploader talks to one entry peer, mesh spreads the rest |
| **Committee registry** | ✅ | `mika::committee` Move module: shared `Committee` object + `join_committee`/`leave_committee`/`committee_size`/`is_member`; Rust mirror in `mika_types::committee` (`assign_shards` deterministic round-robin, snapshot file via `--committee-signers`) |
| **PoA on-chain (committee gate)** | ✅ | `PoaCert::aggregate` (≥ `MIN_POA_SIGNERS`=2 distinct verified keys) → `record_availability(committee_id, storage_id, blob_id, signers)` — Move counts **distinct committee members** from the live registry and enforces the 2/3 threshold `(2n+2)/3`. Crypto signature verification stays off-chain (Kanari Move has no sig-verify native); the event carries the batch for watchers |
| **BFT availability votes** | ✅ | Signed committee votes on `mika/votes/v1` (`AvailabilityVote`: blob, state root, epoch, voter, pubkey, sig) — sig + committee identity verified, 2/3 → `mika bft: availability committed` (application-level BFT, same arithmetic model) |
| **Shard-assignment enforcement** | ✅ | With a committee configured, each store request is routed to its assigned node (`peers_by_key` learnt from announcements' `node_key`); unmapped peers fall back to local store with a warning |
| Move bindings | ✅ | `mika_types::{storage, committee}` follow the `kanari_types` mirror pattern; no copied `object` code |

## Divergences (by design or pending platform support)

| Area | Walrus | Mika | Notes |
|---|---|---|---|
| Crypto verify on-chain | Full PoA signature verification on Sui | Off-chain sig verify; on-chain committee-membership + quorum gate | **Blocked by platform**: Kanari Move exposes no equivalent native. Swap when the native lands (wire format unchanged) |
| Consensus | BFT committee consensus (3f+1), shard assignment on-chain | Vote-based BFT attestation over libp2p (2/3 commit); no Byzantine agreement/tokenomics engine | Same arithmetic model, not Walrus-strength consensus |
| On-chain event read | Contract/state read by storage nodes | Nodes read committee snapshot file; registry changes require rewriting the snapshot | `committee_size`/`is_blob_certified` views exist in Move; need chain object reads (`getObject`) once exposed |
| Fen | GF(2¹⁶) `reed-solomon-simd`, 1000 shards, ~4.5× billed + fixed metadata term | GF(2⁸) custom, default 3+2; billed = 2n slivers (primary+secondary); no fixed metadata term | Shard cap 1MB per shard in the local cache layer |
| Client transport | HTTP aggregator/publisher + direct client | P2P multiaddr `--nodes /ip4/.../p2p/<PeerId>`; no HTTP surface by decision | Entry peer stores then announces; peers pull |

## Running it (P2P-only node mesh)

```bash
# 1. Nodes (log prints the /ip4/.../p2p/<PeerId> multiaddr — use it as --nodes)
mika-node --node-id n1 --data-dir ./d1 --p2p-listen 34001
mika-node --node-id n2 --data-dir ./d2 --p2p-listen 34002 \
  --bootstrap /ip4/127.0.0.1/tcp/34001/p2p/<PEERID_n1>

# 2. Upload to ONE entry peer (P2P spreads; --no-anchor skips on-chain)
mika-cli upload --file myfile.txt \
  --nodes /ip4/127.0.0.1/tcp/34001/p2p/<PEERID_n1> --no-anchor
mika-cli download <BLOB_ID> --output out.txt \
  --nodes /ip4/127.0.0.1/tcp/34001/p2p/<PEERID_n1>

# 3. On-chain (after `kanari move publish` of mika::storage + mika::committee):
mika-cli committee join --committee-id <ID> --node-key <0x...> \
  --password ...                                          # register a node key
mika-cli buy --amount 100000 --password ... --storage-object-id <STORAGE_ID>
mika-cli upload --file f --nodes <MULTIADDR> --fee 5000 \
  --password ... --storage-object-id <ID> --committee-id <COMMITTEE_ID>
#   → anchor_blob then record_availability (PoA) automatically
```

## Next verification TODO

1. Republish Move modules (`mika::storage` with PoA committee gate, new `mika::committee`) — requires wallet password.
2. On-chain read paths (`committee_size`, `is_blob_certified`, `get_blob_anchor`) via RPC once the chain supports them; nodes can then sync the committee snapshot from chain instead of a file.
3. Node-side shard-assignment fetch routing (currently entry-node forwarding only; fetch path is still direct from announcements).
