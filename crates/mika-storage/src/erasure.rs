//! Erasure coding over GF(2^8).
//!
//! Implements a systematic Reed-Solomon-style erasure code: split a blob into
//! `k` data shards plus `m` parity shards, tolerating the loss of up to `m`
//! shards while still being able to reconstruct the original data from any `k`
//! surviving shards.

use crate::error::{MikaStorageError, Result};

/// Pre-computed GF(256) log and exp tables.
struct GfTables {
    exp: [u8; 512],
    log: [u8; 256],
}

impl GfTables {
    fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x = 1u8;
        for i in 0..255 {
            exp[i as usize] = x;
            log[x as usize] = i as u8;
            let high_bit_set = x & 0x80 != 0;
            x <<= 1;
            if high_bit_set {
                x ^= 0x1d; // GF(2^8) reduction with polynomial 0x11d (generator 2)
            }
        }
        for i in 255..512 {
            exp[i as usize] = exp[(i - 255) as usize];
        }
        log[0] = 0; // undefined but guarded
        Self { exp, log }
    }
}

static TABLES: std::sync::OnceLock<GfTables> = std::sync::OnceLock::new();

fn tables() -> &'static GfTables {
    TABLES.get_or_init(GfTables::new)
}

fn gf_mul_raw(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let t = tables();
    t.exp[(t.log[a as usize] as usize + t.log[b as usize] as usize) % 255]
}

fn gf_div_raw(a: u8, b: u8) -> Result<u8> {
    if b == 0 {
        return Err(MikaStorageError::Erasure(
            "division by zero in GF(256)".into(),
        ));
    }
    if a == 0 {
        return Ok(0);
    }
    let t = tables();
    Ok(t.exp[((t.log[a as usize] as i32 - t.log[b as usize] as i32).rem_euclid(255)) as usize])
}

fn gf_inv_raw(a: u8) -> Result<u8> {
    if a == 0 {
        return Err(MikaStorageError::Erasure(
            "inverse of zero in GF(256)".into(),
        ));
    }
    let t = tables();
    Ok(t.exp[(255 - t.log[a as usize] as i32).rem_euclid(255) as usize])
}

/// Multiply two elements of GF(256).
pub fn gf_mul(a: u8, b: u8) -> u8 {
    gf_mul_raw(a, b)
}

/// Divide `a` by `b` in GF(256).
pub fn gf_div(a: u8, b: u8) -> Result<u8> {
    gf_div_raw(a, b)
}

/// Compute the multiplicative inverse of `a` in GF(256).
pub fn gf_inv(a: u8) -> Result<u8> {
    gf_inv_raw(a)
}

/// Raise `a` to the power `n` in GF(256).
pub fn gf_pow(a: u8, n: u32) -> u8 {
    if n == 0 {
        return 1;
    }
    if a == 0 {
        return 0;
    }
    let t = tables();
    let log_n = (t.log[a as usize] as u32 * n) % 255;
    t.exp[log_n as usize]
}

/// Erasure-coding parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErasureParams {
    /// Number of data shards (k).
    pub data_shards: usize,
    /// Number of parity shards (m). Up to `m` shards can be lost.
    pub parity_shards: usize,
}

impl ErasureParams {
    pub fn new(data_shards: usize, parity_shards: usize) -> Result<Self> {
        if data_shards == 0 {
            return Err(MikaStorageError::Erasure("data_shards must be > 0".into()));
        }
        if parity_shards == 0 {
            return Err(MikaStorageError::Erasure(
                "parity_shards must be > 0".into(),
            ));
        }
        if data_shards + parity_shards > 256 {
            return Err(MikaStorageError::Erasure(
                "data_shards + parity_shards must be <= 256".into(),
            ));
        }
        Ok(Self {
            data_shards,
            parity_shards,
        })
    }

    pub fn total_shards(&self) -> usize {
        self.data_shards + self.parity_shards
    }
}

/// Coefficient for parity shard `p` (0-based) and data shard `d` (0-based).
/// Systematic RS Vandermonde coefficient: alpha^(p+1 * d).
fn rs_coeff(parity_index: usize, data_index: usize) -> u8 {
    gf_pow(2u8, ((parity_index + 1) * data_index) as u32)
}

/// Encode `data_shards` shards into `total_shards` shards (systematic).
/// The first `data_shards` shards are the input data; the remaining are parity.
pub fn encode(shards: &[Vec<u8>], params: &ErasureParams) -> Result<Vec<Vec<u8>>> {
    if shards.len() != params.data_shards {
        return Err(MikaStorageError::Erasure(format!(
            "expected {} data shards, got {}",
            params.data_shards,
            shards.len()
        )));
    }
    let shard_len = shards[0].len();
    for s in shards.iter() {
        if s.len() != shard_len {
            return Err(MikaStorageError::Erasure(
                "all data shards must have equal length".into(),
            ));
        }
    }

    let mut out = shards.to_vec();
    for _ in 0..params.parity_shards {
        out.push(vec![0u8; shard_len]);
    }

    for p in 0..params.parity_shards {
        let parity_idx = params.data_shards + p;
        for byte in 0..shard_len {
            let mut acc: u8 = 0;
            for (d, shard) in shards.iter().enumerate() {
                acc ^= gf_mul(rs_coeff(p, d), shard[byte]);
            }
            out[parity_idx][byte] = acc;
        }
    }

    Ok(out)
}

/// Split a blob into `k` equal-size data shards, padding the last one if needed.
pub fn split_into_shards(data: &[u8], k: usize) -> Result<Vec<Vec<u8>>> {
    if k == 0 {
        return Err(MikaStorageError::Erasure("k must be > 0".into()));
    }
    let total = data.len();
    let shard_len = (total + k - 1) / k.max(1);
    let mut out = Vec::with_capacity(k);
    for i in 0..k {
        let start = i * shard_len;
        let end = (start + shard_len).min(total);
        if start >= total {
            out.push(vec![0u8; shard_len]);
        } else {
            let mut shard = data[start..end].to_vec();
            shard.resize(shard_len, 0);
            out.push(shard);
        }
    }
    Ok(out)
}

/// Rejoin `k` data shards into the original blob, trimming padding.
pub fn rejoin_shards(shards: &[Vec<u8>], original_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(original_len);
    for s in shards {
        out.extend_from_slice(s);
    }
    out.truncate(original_len);
    out
}

/// Decode: given at least `k` available shards (by index), recover all `k` data shards.
/// `shards[i]` is `Some(data)` if shard `i` is available, `None` if lost.
pub fn decode(shards: &[Option<Vec<u8>>], params: &ErasureParams) -> Result<Vec<Vec<u8>>> {
    let k = params.data_shards;
    let n = params.total_shards();
    if shards.len() != n {
        return Err(MikaStorageError::Erasure(format!(
            "expected {} shards, got {}",
            n,
            shards.len()
        )));
    }

    let available: Vec<usize> = shards
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.as_ref().map(|_| i))
        .collect();
    if available.len() < k {
        return Err(MikaStorageError::InsufficientShards {
            have: available.len(),
            need: k,
        });
    }

    // Select the first k available shards.
    let chosen = &available[..k];
    let shard_len = shards[chosen[0]].as_ref().unwrap().len();

    // Build the k x k matrix M where row r corresponds to chosen[r].
    // For systematic data shards (index < k): unit vector.
    // For parity shards (index >= k, parity p = index - k): [alpha^(p+1*d)].
    let mut matrix: Vec<Vec<u8>> = vec![vec![0u8; k]; k];
    for (r, &idx) in chosen.iter().enumerate() {
        if idx < k {
            matrix[r][idx] = 1;
        } else {
            let p = idx - k;
            for (d, cell) in matrix[r].iter_mut().enumerate() {
                *cell = rs_coeff(p, d);
            }
        }
    }

    // Augment with the RHS (the actual shard bytes).
    // We solve M * x = b for each byte position independently.
    let mut data_shards = vec![vec![0u8; shard_len]; k];

    for (byte, _) in shards[chosen[0]].as_ref().unwrap().iter().enumerate() {
        // Build augmented matrix [M | b]
        let mut aug: Vec<Vec<u8>> = vec![vec![0u8; k + 1]; k];
        for r in 0..k {
            for c in 0..k {
                aug[r][c] = matrix[r][c];
            }
            aug[r][k] = shards[chosen[r]].as_ref().unwrap()[byte];
        }

        // Gaussian elimination over GF(256).
        for col in 0..k {
            // Find pivot.
            let mut pivot = None;
            for (row, aug_row) in aug.iter().enumerate().skip(col) {
                if aug_row[col] != 0 {
                    pivot = Some(row);
                    break;
                }
            }
            let pivot = pivot
                .ok_or_else(|| MikaStorageError::Erasure("singular matrix during decode".into()))?;
            aug.swap(col, pivot);

            // Normalize pivot row.
            let inv = gf_inv(aug[col][col])?;
            for cell in aug[col][col..=k].iter_mut() {
                *cell = gf_mul(*cell, inv);
            }

            // Eliminate other rows.
            let pivot_row = aug[col].clone();
            for (row, aug_row) in aug.iter_mut().enumerate() {
                if row != col && aug_row[col] != 0 {
                    let factor = aug_row[col];
                    for (cell, &pivot_cell) in
                        aug_row[col..=k].iter_mut().zip(pivot_row[col..=k].iter())
                    {
                        *cell ^= gf_mul(factor, pivot_cell);
                    }
                }
            }
        }

        for d in 0..k {
            data_shards[d][byte] = aug[d][k];
        }
    }

    Ok(data_shards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_data(k: usize, len: usize) -> Vec<Vec<u8>> {
        (0..k).map(|i| vec![(i * 7 + 13) as u8; len]).collect()
    }

    #[test]
    fn encode_round_trip_no_loss() {
        let params = ErasureParams::new(4, 2).unwrap();
        let data = make_data(4, 64);
        let all = encode(&data, &params).unwrap();
        assert_eq!(all.len(), 6);
        // First k are the data themselves.
        for d in 0..4 {
            assert_eq!(all[d], data[d]);
        }
        // Rejoin.
        let joined = rejoin_shards(&all[..4], 64 * 4);
        assert_eq!(joined.len(), 256);
    }

    #[test]
    fn decode_tolerates_loss() {
        let params = ErasureParams::new(4, 2).unwrap();
        let data = make_data(4, 64);
        let all = encode(&data, &params).unwrap();
        let original_len = 64 * 4;

        // Lose shards 0 and 3 (one data, one parity), keep 1,2,4,5.
        let mut present: Vec<Option<Vec<u8>>> = (0..6).map(|i| Some(all[i].clone())).collect();
        present[0] = None;
        present[3] = None;

        let recovered = decode(&present, &params).unwrap();
        assert_eq!(recovered.len(), 4);
        let joined = rejoin_shards(&recovered, original_len);
        // Rebuild expected data.
        let mut expected = Vec::new();
        for d in 0..4 {
            expected.extend_from_slice(&data[d]);
        }
        expected.truncate(original_len);
        assert_eq!(joined, expected);
    }

    #[test]
    fn decode_insufficient_shards_fails() {
        let params = ErasureParams::new(4, 2).unwrap();
        let data = make_data(4, 64);
        let all = encode(&data, &params).unwrap();
        let mut present: Vec<Option<Vec<u8>>> = (0..6).map(|i| Some(all[i].clone())).collect();
        // Only keep 2 shards (< k=4).
        for i in 0..6 {
            if i != 1 && i != 4 {
                present[i] = None;
            }
        }
        let err = decode(&present, &params).unwrap_err();
        assert!(matches!(err, MikaStorageError::InsufficientShards { .. }));
    }
}
