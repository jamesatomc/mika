//! Walrus Red-Stuff-style 2D commitment for a blob.
//!
//! A 1D commitment (`blob_id` = blake3 of the whole blob) proves the blob
//! as a whole but cannot verify a single sliver in isolation. The 2D
//! commitment hashes each data shard (row) into a row root, then the row
//! roots + parity roots into a blob commitment, so any single sliver can
//! be verified against the on-chain commitment without the whole blob.

use crate::blob::BlobID;
use crate::erasure::{ErasureParams, encode, split_into_shards};
use kanari_crypto::hash_data_blake3_array;

/// 2D commitment for `data` encoded with `params`: row roots over the
/// `k` data shards, then a root over row+parity roots, mixed with the
/// blob's 1D id. Deterministic for the same (data, params).
pub fn commitment_2d(data: &[u8], params: &ErasureParams) -> [u8; 32] {
    let shards = split_into_shards(data, params.data_shards)
        .expect("split_into_shards requires k > 0 (validated by ErasureParams)");
    let encoded = encode(&shards, params).expect("encode requires valid params");
    commitment_from_encoded(&encoded, params)
}

/// Same as [`commitment_2d`] but from pre-encoded shards.
pub fn commitment_from_encoded(encoded: &[Vec<u8>], params: &ErasureParams) -> [u8; 32] {
    // Row roots: hash each encoded shard.
    let mut roots = Vec::with_capacity(encoded.len());
    for shard in encoded {
        roots.push(hash_data_blake3_array(shard));
    }
    // Root of roots, bound to k/m so different erasure params cannot collide.
    let mut buf = vec![params.data_shards as u8, params.parity_shards as u8];
    for r in &roots {
        buf.extend_from_slice(r);
    }
    hash_data_blake3_array(&buf)
}

/// The on-chain commitment is the 2D root (verifiable per-sliver) — *not*
/// the 1D blob_id. `blob_id` remains the content address; `commitment`
/// is what `anchor_blob` records.
pub fn blob_commitment(data: &[u8], params: &ErasureParams) -> [u8; 32] {
    commitment_2d(data, params)
}

/// Verify a single shard against the 2D commitment given its row roots
/// (hashes of all shards, data+parity). The node proof API supplies the
/// roots; recomputing the root-of-roots must equal the on-chain value.
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
pub fn commitment_for_blob(blob: &crate::blob::Blob) -> [u8; 32] {
    BlobID::from_content(&blob.data).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::erasure::ErasureParams;

    #[test]
    fn commitment_is_deterministic() {
        let p = ErasureParams::new(3, 2).unwrap();
        let a = commitment_2d(b"hello walrus", &p);
        let b = commitment_2d(b"hello walrus", &p);
        assert_eq!(a, b);
        assert_ne!(a, BlobID::from_content(b"hello walrus").0);
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
    fn verify_shard_round_trip() {
        let p = ErasureParams::new(3, 2).unwrap();
        let data = b"verify me with 2d";
        let encoded = encode(&split_into_shards(data, 3).unwrap(), &p).unwrap();
        let commit = commitment_from_encoded(&encoded, &p);
        let roots: Vec<[u8; 32]> = encoded.iter().map(|s| hash_data_blake3_array(s)).collect();
        for (i, shard) in encoded.iter().enumerate() {
            assert!(verify_shard(shard, i, &roots, &p, &commit), "shard {i}");
        }
        // Tamper one byte → must fail.
        let mut bad = encoded[0].clone();
        bad[0] ^= 0xFF;
        assert!(!verify_shard(&bad, 0, &roots, &p, &commit));
    }
}
