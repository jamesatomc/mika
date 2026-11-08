// Copyright (c) KanariNetwork, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Rust mirrors of the on-chain `mika::storage` Move module.
//!
//! Like `kanari-types` mirrors `coin`/`transfer`/`object`, this mirrors
//! `mika/move/mika/sources/storage.move`: structs, events, constants,
//! module id and function names.

use anyhow::{Context, Result};
use kanari_types::object::UIDRecord;
use move_core_types::account_address::AccountAddress;
use move_core_types::{identifier::Identifier, language_storage::ModuleId};
use serde::{Deserialize, Serialize};
// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

pub const E_BLOB_ALREADY_ANCHORED: u64 = 0;
pub const E_NOT_OWNER: u64 = 1;
pub const E_NOT_FROM_OWNER: u64 = 2;

// ---------------------------------------------------------------------------
// Structs (mirror Move `public struct ... has key, store, drop`)
// ---------------------------------------------------------------------------

/// `Storage { id: UID, anchors: vector<BlobAnchor>, quota: u64 }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Storage {
    pub id: UIDRecord,
    pub anchors: Vec<BlobAnchor>,
    pub quota: u64,
}

/// `BlobAnchor { id: u64, blob_id: address, owner: address, commitment: vector<u8>, epoch: u64, fee_paid: u64, created_at: u64 }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BlobAnchor {
    pub id: u64,
    pub blob_id: AccountAddress,
    pub owner: AccountAddress,
    pub commitment: Vec<u8>,
    pub epoch: u64,
    pub fee_paid: u64,
    pub created_at: u64,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// `StorageInitializedEvent { storage_id: address }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StorageInitializedEvent {
    pub storage_id: AccountAddress,
}

/// `BlobAnchoredEvent { blob_id: address, owner: address, commitment: vector<u8>, epoch: u64, fee_paid: u64 }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BlobAnchoredEvent {
    pub blob_id: AccountAddress,
    pub owner: AccountAddress,
    pub commitment: Vec<u8>,
    pub epoch: u64,
    pub fee_paid: u64,
}

/// `StorageRentPaidEvent { blob_id: address, amount: u64, new_fee_paid: u64 }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StorageRentPaidEvent {
    pub blob_id: AccountAddress,
    pub amount: u64,
    pub new_fee_paid: u64,
}

/// `BlobTransferredEvent { blob_id: address, from: address, to: address, rent_transfer_amount: u64 }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BlobTransferredEvent {
    pub blob_id: AccountAddress,
    pub from: AccountAddress,
    pub to: AccountAddress,
    pub rent_transfer_amount: u64,
}

/// `StorageQuotaBoughtEvent { amount: u64, total_quota: u64 }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StorageQuotaBoughtEvent {
    pub amount: u64,
    pub total_quota: u64,
}

// ---------------------------------------------------------------------------
// CallFunction arguments (BCS-ready, in Move parameter order)
// ---------------------------------------------------------------------------

/// Args for `buy_storage(storage_id: address, amount: u64)`
#[derive(Debug, Clone)]
pub struct BuyStorageRequest {
    pub storage_id: AccountAddress,
    pub amount: u64,
}

impl BuyStorageRequest {
    pub fn encode_args(&self) -> Result<Vec<Vec<u8>>> {
        Ok(vec![self.storage_id.to_vec(), bcs::to_bytes(&self.amount)?])
    }
}

/// Args for `anchor_blob(storage_id: address, blob_id: address, owner: address,
/// commitment: vector<u8>, epoch: u64, fee_paid: u64, ctx: &mut TxContext)`
/// (`ctx` is injected by the node — do not send.)
#[derive(Debug, Clone)]
pub struct AnchorBlobRequest {
    pub storage_id: AccountAddress,
    pub blob_id: AccountAddress,
    pub owner: AccountAddress,
    pub commitment: Vec<u8>,
    pub epoch: u64,
    pub fee_paid: u64,
}

impl AnchorBlobRequest {
    pub fn encode_args(&self) -> Result<Vec<Vec<u8>>> {
        Ok(vec![
            self.storage_id.to_vec(),
            self.blob_id.to_vec(),
            self.owner.to_vec(),
            bcs::to_bytes(&self.commitment)?,
            bcs::to_bytes(&self.epoch)?,
            bcs::to_bytes(&self.fee_paid)?,
        ])
    }
}

/// Args for `pay_storage_rent(storage_id: address, blob_id: address, amount: u64, payer: address)`
#[derive(Debug, Clone)]
pub struct PayStorageRentRequest {
    pub storage_id: AccountAddress,
    pub blob_id: AccountAddress,
    pub amount: u64,
    pub payer: AccountAddress,
}

impl PayStorageRentRequest {
    pub fn encode_args(&self) -> Result<Vec<Vec<u8>>> {
        Ok(vec![
            self.storage_id.to_vec(),
            self.blob_id.to_vec(),
            bcs::to_bytes(&self.amount)?,
            self.payer.to_vec(),
        ])
    }
}

/// Args for `transfer_blob_ownership(storage_id: address, blob_id: address,
/// from: address, to: address, rent_transfer_amount: u64)`
#[derive(Debug, Clone)]
pub struct TransferBlobOwnershipRequest {
    pub storage_id: AccountAddress,
    pub blob_id: AccountAddress,
    pub from: AccountAddress,
    pub to: AccountAddress,
    pub rent_transfer_amount: u64,
}

impl TransferBlobOwnershipRequest {
    pub fn encode_args(&self) -> Result<Vec<Vec<u8>>> {
        Ok(vec![
            self.storage_id.to_vec(),
            self.blob_id.to_vec(),
            self.from.to_vec(),
            self.to.to_vec(),
            bcs::to_bytes(&self.rent_transfer_amount)?,
        ])
    }
}

// ---------------------------------------------------------------------------
// Module helpers
// ---------------------------------------------------------------------------

/// Constants and utilities for `mika::storage`.
pub struct StorageModule;

impl StorageModule {
    pub const PACKAGE_NAME: &'static str = "mika";
    pub const MODULE_NAME: &'static str = "storage";
    /// Deployed package address (the `mika` named address from Move.toml).
    pub const MODULE_ADDRESS: &'static str =
        "0x3ba63b92aac5f2bff87e580e820b61faf1c5fe9ae12f0bc8addd931a340b3146";
    /// Full `address::module` string used by Node RPC `call_function`.
    pub const MODULE_FULL_ID: &'static str =
        "0x3ba63b92aac5f2bff87e580e820b61faf1c5fe9ae12f0bc8addd931a340b3146::storage";

    /// Canonical `ModuleId` for `mika::storage`.
    pub fn get_module_id() -> Result<ModuleId> {
        Self::get_module_id_for(Self::MODULE_ADDRESS)
    }

    /// `ModuleId::new(package_addr, "storage")`
    pub fn get_module_id_for(package_address: &str) -> Result<ModuleId> {
        let address =
            AccountAddress::from_hex_literal(package_address).context("Invalid package address")?;
        let name = Identifier::new(Self::MODULE_NAME).context("Invalid storage module name")?;
        Ok(ModuleId::new(address, name))
    }

    /// Function names in `mika::storage`.
    pub fn function_names() -> StorageFunctions {
        StorageFunctions {
            init: "init",
            buy_storage: "buy_storage",
            anchor_blob: "anchor_blob",
            pay_storage_rent: "pay_storage_rent",
            transfer_blob_ownership: "transfer_blob_ownership",
            get_blob_anchor: "get_blob_anchor",
            is_blob_anchored: "is_blob_anchored",
        }
    }
}

/// Storage module function names.
pub struct StorageFunctions {
    pub init: &'static str,
    pub buy_storage: &'static str,
    pub anchor_blob: &'static str,
    pub pay_storage_rent: &'static str,
    pub transfer_blob_ownership: &'static str,
    pub get_blob_anchor: &'static str,
    pub is_blob_anchored: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use move_core_types::account_address::AccountAddress;

    #[test]
    fn module_id_round_trip() {
        let m = StorageModule::get_module_id_for("0x2").unwrap();
        assert_eq!(m.address().to_hex_literal(), "0x2");
        assert_eq!(m.name().to_string(), "storage");
    }

    #[test]
    fn anchor_args_encode_in_order() {
        let req = AnchorBlobRequest {
            storage_id: AccountAddress::from_hex_literal("0x1").unwrap(),
            blob_id: AccountAddress::from_hex_literal("0x2").unwrap(),
            owner: AccountAddress::from_hex_literal("0x3").unwrap(),
            commitment: vec![9u8; 32],
            epoch: 42,
            fee_paid: 1000,
        };
        let args = req.encode_args().unwrap();
        assert_eq!(args.len(), 6);
        assert_eq!(args[0].len(), 32);
        // commitment is BCS Vec<u8> (length-prefixed)
        assert!(args[3].len() > 32);
    }

    #[test]
    fn error_codes() {
        assert_eq!(E_BLOB_ALREADY_ANCHORED, 0);
        assert_eq!(E_NOT_OWNER, 1);
        assert_eq!(E_NOT_FROM_OWNER, 2);
    }
}
