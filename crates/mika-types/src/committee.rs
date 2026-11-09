// Copyright (c) KanariNetwork, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Rust mirrors of the on-chain `mika::committee` Move module.
//!
//! The committee is the shared registry of storage-node keys whose
//! stored-sliver receipts count toward Proof-of-Availability (Walrus-style:
//! a shared system object, committee membership update + shard assignment).
//! Off-chain nodes load the same membership from a snapshot file; the Move
//! object is canonical for join/leave.

use anyhow::{Context, Result};
use kanari_types::object::UIDRecord;
use move_core_types::account_address::AccountAddress;
use move_core_types::{identifier::Identifier, language_storage::ModuleId};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Structs (mirror Move `public struct ... has key, store, drop`)
// ---------------------------------------------------------------------------

/// `Committee { id: UID, members: vector<CommitteeMember> }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Committee {
    pub id: UIDRecord,
    pub members: Vec<CommitteeMember>,
}

/// `CommitteeMember { node_key: address, shard_count: u64, joined_at: u64 }`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommitteeMember {
    pub node_key: AccountAddress,
    pub shard_count: u64,
    pub joined_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommitteeInitializedEvent {
    pub committee_id: AccountAddress,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommitteeJoinedEvent {
    pub node_key: AccountAddress,
    pub shard_count: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommitteeLeftEvent {
    pub node_key: AccountAddress,
}

// ---------------------------------------------------------------------------
// CallFunction arguments (BCS-ready, in Move parameter order)
// ---------------------------------------------------------------------------

/// Args for `join_committee(committee_id: address, node_key: address,
/// shard_count: u64, ctx)` (`ctx` injected by the node).
#[derive(Debug, Clone)]
pub struct JoinCommitteeRequest {
    pub committee_id: AccountAddress,
    pub node_key: AccountAddress,
    pub shard_count: u64,
}

impl JoinCommitteeRequest {
    pub fn encode_args(&self) -> Result<Vec<Vec<u8>>> {
        Ok(vec![
            self.committee_id.to_vec(),
            self.node_key.to_vec(),
            bcs::to_bytes(&self.shard_count)?,
        ])
    }
}

/// Args for `leave_committee(committee_id: address, node_key: address)`.
#[derive(Debug, Clone)]
pub struct LeaveCommitteeRequest {
    pub committee_id: AccountAddress,
    pub node_key: AccountAddress,
}

impl LeaveCommitteeRequest {
    pub fn encode_args(&self) -> Result<Vec<Vec<u8>>> {
        Ok(vec![self.committee_id.to_vec(), self.node_key.to_vec()])
    }
}

// ---------------------------------------------------------------------------
// Shard assignment (Walrus-style: the committee's ordered member list
// maps shard index → node; deterministic and identical on every peer)
// ---------------------------------------------------------------------------

/// Map shard `j` to the committee member at `j % committee_size` (the
/// committee is kept in a canonical sorted order so every peer derives
/// the same assignment).
pub fn assign_shards(committee: &[AccountAddress], total_shards: usize) -> Vec<AccountAddress> {
    if committee.is_empty() || total_shards == 0 {
        return Vec::new();
    }
    let mut sorted = committee.to_vec();
    sorted.sort_by_key(|a| a.to_string());
    (0..total_shards)
        .map(|j| sorted[j % sorted.len()])
        .collect()
}

/// Load and parse a committee snapshot file: one hex address per line
/// (`0x...`), blank lines and `#` comments ignored. Nodes pass this via
/// `--committee-signers <path>`; it mirrors the on-chain registry.
pub fn load_committee_file(path: &str) -> Result<Vec<AccountAddress>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading committee file {path}"))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.push(
            AccountAddress::from_hex_literal(line)
                .with_context(|| format!("bad committee address: {line}"))?,
        );
    }
    Ok(out)
}

/// 2/3 BFT honest threshold of a committee of `n`: the minimum number of
/// distinct members whose attestations constitute a quorum.
pub fn bft_quorum_threshold(committee_size: usize) -> usize {
    if committee_size == 0 {
        return 0;
    }
    (committee_size * 2).div_ceil(3)
}

/// Canonical committee `node_key` for a node's signing key: the BLAKE3
/// digest of its K256 public key, as an on-chain address. The same
/// derivation is used by nodes (assignment checks), the CLI (PoA signer
/// listing) and the Move committee registry.
pub fn node_key_for_public_key(public_key_hex: &str) -> Result<AccountAddress> {
    let pk =
        hex::decode(public_key_hex.trim_start_matches("0x")).context("public_key must be hex")?;
    let bytes = kanari_crypto::hash_data_blake3_array(&pk);
    Ok(AccountAddress::new(bytes))
}

// ---------------------------------------------------------------------------
// Module helpers
// ---------------------------------------------------------------------------

/// Constants and utilities for `mika::committee`.
pub struct CommitteeModule;

impl CommitteeModule {
    pub const PACKAGE_NAME: &'static str = "mika";
    pub const MODULE_NAME: &'static str = "committee";
    pub const MODULE_ADDRESS: &'static str =
        "0x3ba63b92aac5f2bff87e580e820b61faf1c5fe9ae12f0bc8addd931a340b3146";

    /// Canonical full `address::module` string for RPC `call_function`.
    pub fn module_full_id() -> String {
        format!("{}::{}", Self::MODULE_ADDRESS, Self::MODULE_NAME)
    }

    /// Canonical `ModuleId` for `mika::committee` (deployed package address).
    pub fn get_module_id() -> Result<ModuleId> {
        Self::get_module_id_for(Self::MODULE_ADDRESS)
    }

    /// `ModuleId::new(package_addr, "committee")`
    pub fn get_module_id_for(package_address: &str) -> Result<ModuleId> {
        let address =
            AccountAddress::from_hex_literal(package_address).context("Invalid package address")?;
        let name = Identifier::new(Self::MODULE_NAME).context("Invalid committee module name")?;
        Ok(ModuleId::new(address, name))
    }

    /// Function names in `mika::committee`.
    pub fn function_names() -> CommitteeFunctions {
        CommitteeFunctions {
            init: "init",
            join_committee: "join_committee",
            leave_committee: "leave_committee",
            committee_size: "committee_size",
            is_member: "is_member",
        }
    }
}

/// Committee module function names.
pub struct CommitteeFunctions {
    pub init: &'static str,
    pub join_committee: &'static str,
    pub leave_committee: &'static str,
    pub committee_size: &'static str,
    pub is_member: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_id_round_trip() {
        let m = CommitteeModule::get_module_id_for("0x2").unwrap();
        assert_eq!(m.address().to_hex_literal(), "0x2");
        assert_eq!(m.name().to_string(), "committee");
    }

    #[test]
    fn join_args_encode_in_order() {
        let req = JoinCommitteeRequest {
            committee_id: AccountAddress::from_hex_literal("0x1").unwrap(),
            node_key: AccountAddress::from_hex_literal("0x2").unwrap(),
            shard_count: 16,
        };
        let args = req.encode_args().unwrap();
        assert_eq!(args.len(), 3);
        assert_eq!(args[0].len(), 32);
        assert_eq!(args[1].len(), 32);
        assert_eq!(args[2].len(), 8);
    }

    #[test]
    fn leave_args_encode_in_order() {
        let req = LeaveCommitteeRequest {
            committee_id: AccountAddress::from_hex_literal("0x1").unwrap(),
            node_key: AccountAddress::from_hex_literal("0x2").unwrap(),
        };
        let args = req.encode_args().unwrap();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].len(), 32);
        assert_eq!(args[1].len(), 32);
    }

    #[test]
    fn assign_shards_round_robin_sorted() {
        let a =
            AccountAddress::from_hex_literal("0x3000000000000000000000000000000000000003").unwrap();
        let b =
            AccountAddress::from_hex_literal("0x1000000000000000000000000000000000000001").unwrap();
        let c =
            AccountAddress::from_hex_literal("0x2000000000000000000000000000000000000002").unwrap();
        let shards = assign_shards(&[a, b, c], 5);
        assert_eq!(shards.len(), 5);
        // Sorted order b, c, a, b, c — deterministic for every peer.
        assert_eq!(shards[0], b);
        assert_eq!(shards[1], c);
        assert_eq!(shards[2], a);
        assert_eq!(shards[3], b);
        assert_eq!(shards[4], c);
    }

    #[test]
    fn assign_shards_empty_committee() {
        assert!(assign_shards(&[], 3).is_empty());
    }

    #[test]
    fn bft_threshold_table() {
        assert_eq!(bft_quorum_threshold(0), 0);
        assert_eq!(bft_quorum_threshold(1), 1);
        assert_eq!(bft_quorum_threshold(2), 2);
        assert_eq!(bft_quorum_threshold(3), 2);
        assert_eq!(bft_quorum_threshold(4), 3);
        assert_eq!(bft_quorum_threshold(10), 7);
    }

    #[test]
    fn node_key_derivation_is_deterministic() {
        let k = node_key_for_public_key(format!("02{}", "ab".repeat(32)).as_str()).unwrap();
        let k2 = node_key_for_public_key(format!("02{}", "ab".repeat(32)).as_str()).unwrap();
        assert_eq!(k, k2);
    }
}
