//! Walrus Red-Stuff-style 2D commitment for a blob.
//!
//! A 1D commitment (`blob_id` = blake3 of the whole blob) proves the blob
//! as a whole but cannot verify a single sliver in isolation. The 2D
//! commitment hashes each data shard (row) into a leaf, builds a Merkle
//! tree over data-shard leaves and another over parity-shard leaves, then
//! binds both Merkle roots plus k/m into the final on-chain commitment.
//! Any single sliver can be verified against the on-chain commitment with
//! just its Merkle proof — no need for the whole blob.

use serde::{Deserialize, Serialize};

use crate::erasure::{ErasureParams, encode, split_into_shards};
use kanari_crypto::hash_data_blake3_array;

/// A Merkle proof: the sibling hashes on the path from a leaf up to the
/// root, plus the leaf's index and the root hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerkleProof {
    pub leaf_index: usize,
    pub siblings: Vec<[u8; 32]>,
    pub root: [u8; 32],
}

/// Full Red Stuff 2D commitment for `k` data shards and `m` parity shards.
///
/// - `data_roots`: blake3 hash of each data shard (the Merkle leaves).
/// - `parity_roots`: blake3 hash of each parity shard (the Merkle leaves).
/// - `data_root`: Merkle root over `data_roots`.
/// - `parity_root`: Merkle root over `parity_roots`.
/// - `commitment`: `blake3(k || m || data_root || parity_root)` — recorded
///   on-chain by `anchor_blob`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commitment2D {
    pub data_roots: Vec<[u8; 32]>,
    pub parity_roots: Vec<[u8; 32]>,
    pub data_root: [u8; 32],
    pub parity_root: [u8; 32],
    pub commitment: [u8; 32],
}

/// Hash a pair of 32-byte values: `blake3(left || right)`.
fn hash_pair(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&left);
    buf.extend_from_slice(&right);
    hash_data_blake3_array(&buf)
}

/// Build a Merkle tree over leaf hashes and return the root.
/// Odd leaf counts are handled by duplicating the last leaf at each level.
fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return hash_data_blake3_array(b"");
    }
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            if pair.len() == 2 {
                next.push(hash_pair(pair[0], pair[1]));
            } else {
                // Odd one out: hash with itself.
                next.push(hash_pair(pair[0], pair[0]));
            }
        }
        level = next;
    }
    level[0]
}

/// Generate a Merkle proof for the leaf at `index`.
/// Returns the sibling hashes from leaf to root, plus the root.
fn merkle_proof(leaves: &[[u8; 32]], index: usize) -> Option<MerkleProof> {
    if index >= leaves.len() {
        return None;
    }
    let root = merkle_root(leaves);
    let mut siblings = Vec::new();
    let mut level = leaves.to_vec();
    let mut idx = index;
    while level.len() > 1 {
        let pair_start = (idx / 2) * 2;
        let sibling = if pair_start + 1 < level.len() {
            if idx.is_multiple_of(2) {
                level[pair_start + 1] // right sibling
            } else {
                level[pair_start] // left sibling
            }
        } else {
            level[pair_start] // odd one out: duplicate itself
        };
        siblings.push(sibling);
        idx /= 2;
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            if pair.len() == 2 {
                next.push(hash_pair(pair[0], pair[1]));
            } else {
                next.push(hash_pair(pair[0], pair[0]));
            }
        }
        level = next;
    }
    Some(MerkleProof {
        leaf_index: index,
        siblings,
        root,
    })
}

/// Verify a Merkle proof: recompute the path from `leaf` up to `proof.root`
/// using the sibling hashes.
fn verify_merkle_proof(leaf: &[u8; 32], proof: &MerkleProof) -> bool {
    if proof.siblings.is_empty() {
        // Single-leaf tree: root == leaf.
        return *leaf == proof.root;
    }
    let mut current = *leaf;
    let mut idx = proof.leaf_index;
    for sibling in &proof.siblings {
        current = if idx.is_multiple_of(2) {
            hash_pair(current, *sibling) // left node
        } else {
            hash_pair(*sibling, current) // right node
        };
        idx /= 2;
    }
    current == proof.root
}

/// Build a full 2D commitment from pre-encoded shards.
pub fn build_commitment_2d(encoded: &[Vec<u8>], params: &ErasureParams) -> Commitment2D {
    let k = params.data_shards;
    let _m = params.parity_shards;
    let data_shards = &encoded[..k];
    let parity_shards = &encoded[k..];

    let data_roots: Vec<[u8; 32]> = data_shards
        .iter()
        .map(|s| hash_data_blake3_array(s))
        .collect();
    let parity_roots: Vec<[u8; 32]> = parity_shards
        .iter()
        .map(|s| hash_data_blake3_array(s))
        .collect();

    let data_root = merkle_root(&data_roots);
    let parity_root = merkle_root(&parity_roots);

    let mut buf = Vec::with_capacity(66);
    buf.push(params.data_shards as u8);
    buf.push(params.parity_shards as u8);
    buf.extend_from_slice(&data_root);
    buf.extend_from_slice(&parity_root);
    let commitment = hash_data_blake3_array(&buf);

    Commitment2D {
        data_roots,
        parity_roots,
        data_root,
        parity_root,
        commitment,
    }
}

/// Build a 2D commitment from raw data: split into shards, encode, then
/// commit. Deterministic for the same (data, params).
pub fn commitment_2d(data: &[u8], params: &ErasureParams) -> Commitment2D {
    let shards = split_into_shards(data, params.data_shards)
        .expect("split_into_shards requires k > 0 (validated by ErasureParams)");
    let encoded = encode(&shards, params).expect("encode requires valid params");
    build_commitment_2d(&encoded, params)
}

/// Generate a Merkle proof for a single data shard at `shard_index`.
/// The proof lets a verifier confirm the shard is part of `data_root`.
pub fn shard_proof(shard_index: usize, data_roots: &[[u8; 32]]) -> Option<MerkleProof> {
    merkle_proof(data_roots, shard_index)
}

/// Verify a single data shard against the 2D commitment using its Merkle proof.
///
/// The caller supplies the shard bytes, its index, the Merkle proof from the
/// storage node, the on-chain `parity_root` and `commitment`, and the erasure
/// params. Returns true iff:
///   1. `blake3(shard)` hashes to the proof's leaf,
///   2. the Merkle proof verifies up to `data_root`,
///   3. `blake3(k || m || data_root || parity_root) == commitment`.
pub fn verify_shard_2d(
    shard: &[u8],
    shard_index: usize,
    proof: &MerkleProof,
    parity_root: [u8; 32],
    commitment: [u8; 32],
    params: &ErasureParams,
) -> bool {
    if proof.leaf_index != shard_index {
        return false;
    }
    let leaf = hash_data_blake3_array(shard);
    if !verify_merkle_proof(&leaf, proof) {
        return false;
    }
    let mut buf = Vec::with_capacity(66);
    buf.push(params.data_shards as u8);
    buf.push(params.parity_shards as u8);
    buf.extend_from_slice(&proof.root); // data_root from the proof
    buf.extend_from_slice(&parity_root);
    hash_data_blake3_array(&buf) == commitment
}

/// The on-chain commitment hash: `blake3(k || m || data_root || parity_root)`.
/// Backward-compatible wrapper returning just the 32-byte commitment.
pub fn blob_commitment(data: &[u8], params: &ErasureParams) -> [u8; 32] {
    commitment_2d(data, params).commitment
}

/// Verify a single shard against the legacy flat commitment (root-of-roots).
/// Kept for compatibility; prefer [`verify_shard_2d`] with Merkle proofs.
#[deprecated(note = "use verify_shard_2d with a Merkle proof instead")]
pub fn verify_shard(
    shard: &[u8],
    shard_index: usize,
    row_roots: &[[u8; 32]],
    params: &ErasureParams,
    commitment: &[u8; 32],
) -> bool {
    if shard_index >= row_roots.len() {
        return false;
    }
    if hash_data_blake3_array(shard) != row_roots[shard_index] {
        return false;
    }
    let mut buf = vec![params.data_shards as u8, params.parity_shards as u8];
    for r in row_roots {
        buf.extend_from_slice(r);
    }
    hash_data_blake3_array(&buf) == *commitment
}

/// BlobID ↔ commitment helper for tests: commitment changes if data changes.
pub fn commitment_for_blob(blob: &mika_types::blob::Blob) -> [u8; 32] {
    mika_types::blob::BlobID::from_content(&blob.data).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::erasure::ErasureParams;

    #[test]
    fn merkle_root_single_leaf() {
        let leaf = [42u8; 32];
        assert_eq!(merkle_root(&[leaf]), leaf);
    }

    #[test]
    fn merkle_root_even_count() {
        let leaves: Vec<[u8; 32]> = (0..4).map(|i| [i as u8; 32]).collect();
        let root = merkle_root(&leaves);
        // root = hash(hash(l0,l1), hash(l2,l3))
        let expected = hash_pair(
            hash_pair(leaves[0], leaves[1]),
            hash_pair(leaves[2], leaves[3]),
        );
        assert_eq!(root, expected);
    }

    #[test]
    fn merkle_root_odd_count() {
        let leaves: Vec<[u8; 32]> = (0..3).map(|i| [i as u8; 32]).collect();
        let root = merkle_root(&leaves);
        // level 1: hash(l0,l1), hash(l2,l2)
        // root: hash(hash(l0,l1), hash(l2,l2))
        let expected = hash_pair(
            hash_pair(leaves[0], leaves[1]),
            hash_pair(leaves[2], leaves[2]),
        );
        assert_eq!(root, expected);
    }

    #[test]
    fn merkle_proof_round_trip() {
        let leaves: Vec<[u8; 32]> = (0..8).map(|i| [i as u8; 32]).collect();
        let root = merkle_root(&leaves);
        for i in 0..leaves.len() {
            let proof = merkle_proof(&leaves, i).unwrap();
            assert_eq!(proof.root, root);
            assert!(verify_merkle_proof(&leaves[i], &proof));
        }
    }

    #[test]
    fn merkle_proof_tamper_fails() {
        let leaves: Vec<[u8; 32]> = (0..8).map(|i| [i as u8; 32]).collect();
        let proof = merkle_proof(&leaves, 0).unwrap();
        let mut bad_leaf = leaves[0];
        bad_leaf[0] ^= 0xFF;
        assert!(!verify_merkle_proof(&bad_leaf, &proof));
    }

    #[test]
    fn commitment_is_deterministic() {
        let p = ErasureParams::new(3, 2).unwrap();
        let a = commitment_2d(b"hello walrus", &p);
        let b = commitment_2d(b"hello walrus", &p);
        assert_eq!(a, b);
    }

    #[test]
    fn commitment_changes_with_data() {
        let p = ErasureParams::new(3, 2).unwrap();
        assert_ne!(commitment_2d(b"a", &p), commitment_2d(b"b", &p));
    }

    #[test]
    fn commitment_binds_params() {
        let p1 = ErasureParams::new(3, 2).unwrap();
        let p2 = ErasureParams::new(4, 2).unwrap();
        assert_ne!(commitment_2d(b"hello", &p1), commitment_2d(b"hello", &p2));
    }

    #[test]
    fn verify_shard_2d_round_trip() {
        let p = ErasureParams::new(3, 2).unwrap();
        let data = b"verify me with 2d merkle";
        let commit = commitment_2d(data, &p);
        // Re-encode to get shards
        let shards = split_into_shards(data, 3).unwrap();
        let encoded = encode(&shards, &p).unwrap();
        for (i, shard) in encoded.iter().enumerate() {
            if i >= 3 {
                // parity shards aren't covered by data Merkle tree
                continue;
            }
            let proof = shard_proof(i, &commit.data_roots).unwrap();
            assert!(
                verify_shard_2d(shard, i, &proof, commit.parity_root, commit.commitment, &p),
                "shard {i}"
            );
        }
        // Tamper one byte → must fail.
        let mut bad = encoded[0].clone();
        bad[0] ^= 0xFF;
        let proof = shard_proof(0, &commit.data_roots).unwrap();
        assert!(!verify_shard_2d(
            &bad,
            0,
            &proof,
            commit.parity_root,
            commit.commitment,
            &p
        ));
    }

    #[test]
    fn verify_shard_2d_wrong_index_fails() {
        let p = ErasureParams::new(3, 2).unwrap();
        let data = b"wrong index test";
        let commit = commitment_2d(data, &p);
        let shards = split_into_shards(data, 3).unwrap();
        let encoded = encode(&shards, &p).unwrap();
        // Use shard 0's proof but claim it's shard 1.
        let proof = shard_proof(0, &commit.data_roots).unwrap();
        assert!(!verify_shard_2d(
            &encoded[1],
            1,
            &proof,
            commit.parity_root,
            commit.commitment,
            &p
        ));
    }

    #[test]
    fn blob_commitment_returns_hash() {
        let p = ErasureParams::new(3, 2).unwrap();
        let commit = commitment_2d(b"test", &p);
        assert_eq!(blob_commitment(b"test", &p), commit.commitment);
    }
}
