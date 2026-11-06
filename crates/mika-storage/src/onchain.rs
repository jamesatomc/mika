//! On-chain integration: paying Mika storage fees with KANARI native coin
//! transfers via `kanari-core`.
//!
//! This module is gated behind the `onchain` feature and depends on
//! `kanari-core`. It defines the interface for charging storage rent and
//! transferring blob ownership using the existing native transfer execution
//! path (`execute_backend_native_transfer` in kanari-core).

use crate::blob::BlobID;
use crate::error::{MikaStorageError, Result};
use kanari_types::address::Address as KanariAddress;
use kanari_types::gas_coin::GAS_COIN;
use serde::{Deserialize, Serialize};

/// A storage fee payment recorded on-chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageFeePayment {
    pub blob_id: BlobID,
    pub payer: KanariAddress,
    pub amount: u64,
    pub epoch: crate::epoch::Epoch,
}

/// Result of anchoring a blob's metadata (owner, commitment, lease) on-chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobAnchor {
    pub blob_id: BlobID,
    pub owner: KanariAddress,
    pub commitment: [u8; 32],
    pub epoch: crate::epoch::Epoch,
    pub fee_paid: u64,
}

/// Trait for paying storage fees and anchoring blobs on-chain.
///
/// Implementations live in the consumer crate (e.g. kanari-core or an RPC
/// service) that has access to a `BlockchainEngine`. Mika-storage itself only
/// defines the trait so the storage library stays decoupled from the engine.
#[async_trait::async_trait]
pub trait StoragePayment: Send + Sync {
    /// Pay storage rent for a blob by debiting `payer`'s native coin object.
    async fn pay_storage_rent(&self, payment: &StorageFeePayment) -> Result<()>;
    /// Anchor blob metadata (owner, commitment, lease) on-chain.
    async fn anchor_blob(&self, anchor: &BlobAnchor) -> Result<()>;
    /// Transfer blob ownership from `from` to `to`, moving the remaining
    /// prepaid rent via a native KANARI transfer.
    async fn transfer_blob_ownership(
        &self,
        blob_id: &BlobID,
        from: &KanariAddress,
        to: &KanariAddress,
        rent_transfer_amount: u64,
    ) -> Result<()>;
}

/// Compute the native coin type string for KANARI.
pub fn native_coin_type() -> String {
    GAS_COIN.to_string()
}

/// Encode a KANARI balance into canonical coin object bytes (40 bytes: 32-byte
/// id + 8-byte little-endian balance).
pub fn encode_coin_balance(balance: u64) -> Vec<u8> {
    let mut data = vec![0u8; 40];
    data[32..40].copy_from_slice(&balance.to_le_bytes());
    data
}

/// Decode a KANARI balance from canonical coin object bytes.
pub fn decode_coin_balance(data: &[u8]) -> Result<u64> {
    if data.len() < 40 {
        return Err(MikaStorageError::Serialization(
            "coin object data too short".into(),
        ));
    }
    Ok(u64::from_le_bytes(data[32..40].try_into().map_err(
        |_| MikaStorageError::Serialization("invalid balance bytes".into()),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coin_balance_round_trip() {
        let data = encode_coin_balance(1_234_567);
        assert_eq!(decode_coin_balance(&data).unwrap(), 1_234_567);
    }

    #[test]
    fn anchor_serialization() {
        let blob_id = BlobID::from_content(b"anchor");
        let anchor = BlobAnchor {
            blob_id,
            owner: KanariAddress::from_hex_literal("0xaaaa").unwrap(),
            commitment: [9u8; 32],
            epoch: 12345,
            fee_paid: 5000,
        };
        let json = serde_json::to_string(&anchor).unwrap();
        let back: BlobAnchor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, anchor);
    }
}
