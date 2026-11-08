//! Kanari SDK integration for Mika storage.
//!
//! Bridges Mika's erasure-coded storage with the kanari-sdk blockchain layer:
//! signing storage certificates with kanari-crypto, anchoring blob metadata
//! on-chain via kanari-core's BlockchainEngine, and paying storage rent with
//! native KANARI coin transfers.
//!
//! The signing keypair is loaded from the kanari config file via
//! [`kanari_common`], which reads `~/.kanari/kanari_config/kanari.yaml` and
//! decrypts the keystore at the configured `keystore_path`.

use crate::blob::BlobID;
use crate::certificate::{WriteCertificate, WriteCertificateQuorum};
use crate::error::{MikaStorageError, Result};
use crate::onchain::{BlobAnchor, StorageFeePayment, StoragePayment};
use bcs::to_bytes as bcs_to_bytes;
use kanari_common::{get_active_address, load_kanari_config};
use kanari_core::BlockchainEngine;
use kanari_crypto::keys::{CurveType, keypair_from_private_key};
use kanari_rpc_client::RpcClient;
use kanari_types::address::Address as KanariAddress;
use kanari_types::transaction::{
    GasPayment, ObjectInput, ObjectOwnerKind, ObjectRef, SignedTransaction, Transaction,
};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Mika Move module address (where the storage contract is deployed).
const MIKA_MODULE_ADDRESS: &str =
    "0x3ba63b92aac5f2bff87e580e820b61faf1c5fe9ae12f0bc8addd931a340b3146";
/// Mika Move module name (the module *inside* package `mika`).
const MIKA_MODULE_NAME: &str = "storage";

/// A Mika storage node integrated with the kanari-sdk blockchain.
///
/// Wraps a `BlockchainEngine` for on-chain operations (fee payments, blob
/// anchoring, ownership transfers) and a signing key loaded from the kanari
/// config file / keystore for storage certificates.
pub struct KanariStorage {
    engine: Arc<RwLock<BlockchainEngine>>,
    /// RPC URL of the running Kanari node (`get_active_rpc`), for submitting
    /// transactions to the real network (visible in explorer) instead of the
    /// in-memory engine.
    rpc: Option<RpcClient>,
    rpc_url: Option<String>,
    private_key: String,
    #[allow(dead_code)]
    public_key: String,
    address: String,
    curve_type: CurveType,
    node_id: String,
    /// Storage object ID created during genesis init of the Mika Move module.
    /// Set via [`KanariStorage::with_storage_object_id`] after module deploy.
    storage_object_id: Option<String>,
}

impl KanariStorage {
    /// Load keypair from the kanari config file via [`kanari_common`] and
    /// create a new `KanariStorage` with an in-memory BlockchainEngine.
    ///
    /// Reads `~/.kanari/kanari_config/kanari.yaml` for configuration,
    /// extracts the `keystore_path` and `active_address`, loads the keystore,
    /// and decrypts the active address's private key with the given password.
    pub fn from_config(password: &str, node_id: impl Into<String>) -> Result<Self> {
        // Load config via kanari-common
        let _config = load_kanari_config().map_err(|e| MikaStorageError::Io(e.to_string()))?;

        // Get active address from config
        let active_address = get_active_address().ok_or_else(|| {
            MikaStorageError::Crypto("no active address found in kanari config".into())
        })?;

        // The keystore stores an encrypted *Wallet* (TOML, compressed) — not a
        // raw private key. `decrypt_data` alone returns the compressed TOML,
        // which hex-encoding + keypair_from_private_key cannot parse
        // ("Invalid private key format"). `load_wallet` does the full
        // decrypt → decompress → TOML parse.
        let wallet = kanari_crypto::load_wallet(&active_address, password)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;

        let private_key_hex = wallet.private_key.to_string();
        let curve_type = wallet.curve_type;

        // Derive public key from private key (validates format)
        let keypair = keypair_from_private_key(&private_key_hex, curve_type)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;

        let engine = BlockchainEngine::new_in_memory()
            .map_err(|e| MikaStorageError::OnChain(e.to_string()))?;

        Ok(Self {
            engine: Arc::new(RwLock::new(engine)),
            rpc: kanari_common::get_active_rpc().map(|u| {
                eprintln!("mika: submitting on-chain via RPC {u}");
                RpcClient::new(u)
            }),
            rpc_url: kanari_common::get_active_rpc(),
            private_key: private_key_hex.to_string(),
            public_key: keypair.public_key,
            address: active_address,
            curve_type,
            node_id: node_id.into(),
            storage_object_id: None,
        })
    }

    /// Create a KanariStorage from an existing private key hex string.
    pub fn from_private_key(
        private_key_hex: &str,
        curve_type: CurveType,
        node_id: impl Into<String>,
    ) -> Result<Self> {
        let engine = BlockchainEngine::new_in_memory()
            .map_err(|e| MikaStorageError::OnChain(e.to_string()))?;
        let keypair = keypair_from_private_key(private_key_hex, curve_type)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;
        Ok(Self {
            engine: Arc::new(RwLock::new(engine)),
            rpc: None,
            rpc_url: None,
            private_key: private_key_hex.to_string(),
            public_key: keypair.public_key,
            address: keypair.address,
            curve_type,
            node_id: node_id.into(),
            storage_object_id: None,
        })
    }

    /// Set the Storage object ID (returned from genesis `StorageInitializedEvent`).
    /// Required before calling `anchor_blob`, `pay_storage_rent`, or
    /// `transfer_blob_ownership`.
    pub fn with_storage_object_id(mut self, storage_object_id: impl Into<String>) -> Self {
        self.storage_object_id = Some(storage_object_id.into());
        self
    }

    /// Return the Storage object ID, if configured.
    pub fn storage_object_id(&self) -> Option<&str> {
        self.storage_object_id.as_deref()
    }

    /// Return the node's kanari address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Return the node's node ID.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Return the node's kanari public key.
    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// Return the curve type used for signing.
    pub fn curve_type(&self) -> CurveType {
        self.curve_type
    }

    /// Sign a write certificate with this node's private key.
    pub fn sign_certificate(&self, cert: &mut WriteCertificate) -> Result<()> {
        cert.sign(&self.private_key, self.curve_type)
    }

    /// Sign all certificates in a quorum with this node's private key.
    pub fn sign_quorum(&self, quorum: &mut WriteCertificateQuorum) -> Result<()> {
        quorum.sign_all(&self.private_key, self.curve_type)
    }

    /// Get a reference to the underlying BlockchainEngine.
    pub fn engine(&self) -> Arc<RwLock<BlockchainEngine>> {
        self.engine.clone()
    }

    /// Build a `SignedTransaction` that calls a Mika Move module function.
    fn build_execute_function_tx(
        &self,
        function: &str,
        args: Vec<Vec<u8>>,
        object_inputs: Vec<ObjectInput>,
    ) -> Result<SignedTransaction> {
        // RPC verification requires tagged sender: `CurveType:public_key`
        // (ko `kanari move` uses `get_sender_for_tx` for the same reason).
        let sender = format!("{}:{}", self.curve_type, self.public_key);
        let module = format!("{}::{}", MIKA_MODULE_ADDRESS, MIKA_MODULE_NAME);

        let gas_payment = Some(GasPayment {
            payment_objects: vec![],
            owner: self.address.clone(),
            budget: 10_000_000,
            price: 1000,
            coin_type: None,
            price_version: None,
        });

        let tx = Transaction::ExecuteFunction {
            sender,
            module,
            function: function.to_string(),
            type_args: vec![],
            args,
            object_inputs,
            gas_payment,
            gas_limit: 10_000_000,
            gas_price: 1000,
            nonce: 0,
        };

        let mut signed_tx = SignedTransaction::new(tx);
        signed_tx
            .sign(&self.private_key, self.curve_type)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;

        Ok(signed_tx)
    }

    /// Submit a signed transaction. When an RPC URL is configured
    /// (`get_active_rpc`), submit to the **running node** via
    /// `call_function` (visible in explorer); otherwise fall back to the
    /// in-memory engine (local/dev only, invisible on explorer).
    async fn submit_transaction(&self, signed_tx: SignedTransaction) -> Result<String> {
        if let Some(rpc) = &self.rpc {
            // Build the request on the node first (nonce/gas chosen by node).
            // Submitting a nonce-0 request fails: "Prepared request nonce
            // must be non-zero" — the node rejects zeros, so we must build
            // then sign the *built* request (same as `kanari move` CLI).
            let Transaction::ExecuteFunction {
                sender,
                module,
                function,
                type_args,
                args,
                mut object_inputs,
                gas_payment,
                gas_limit,
                gas_price,
                nonce: _,
            } = signed_tx.transaction.clone()
            else {
                return Err(MikaStorageError::OnChain(
                    "expected ExecuteFunction transaction for RPC submit".into(),
                ));
            };
            // Node requires (object_id, version, digest) for object inputs.
            for input in &mut object_inputs {
                if input.object_ref.version.is_none() || input.object_ref.digest.is_none() {
                    let info = rpc
                        .get_object(&input.object_ref.object_id)
                        .await
                        .map_err(|e| {
                            MikaStorageError::OnChain(format!(
                                "get_object {}: {e}",
                                input.object_ref.object_id
                            ))
                        })?;
                    input.object_ref.version = Some(info.version);
                    input.object_ref.digest = info.digest;
                }
            }
            let (package, module_name) = module.split_once("::").unwrap_or((&module, ""));
            let build_req = kanari_rpc_api::requests::BuildCallFunctionRequest {
                sender: sender.clone(),
                package: package.to_string(),
                module: module_name.to_string(),
                function: function.clone(),
                type_args: type_args.clone(),
                args: args.clone(),
                object_inputs: if object_inputs.is_empty() {
                    None
                } else {
                    Some(object_inputs.clone())
                },
                gas_limit,
                gas_price,
                nonce: None,
                execute_immediate: None,
            };
            let mut built = rpc
                .build_call_function(build_req)
                .await
                .map_err(|e| MikaStorageError::OnChain(format!("RPC build_call_function: {e}")))?;
            // Sign the built request (same Transaction shape as `kanari move`).
            let tx_for_sign = Transaction::ExecuteFunction {
                sender: built.sender.clone(),
                module: format!("{}::{}", built.package, built.module),
                function: built.function.clone(),
                type_args: built.type_args.clone(),
                args: built.args.clone(),
                object_inputs: built.object_inputs.clone().unwrap_or_default(),
                gas_payment: built.gas_payment.clone(),
                gas_limit: built.gas_limit,
                gas_price: built.gas_price,
                nonce: built
                    .canonical_nonce()
                    .map_err(|e| MikaStorageError::OnChain(format!("invalid built nonce: {e}")))?,
            };
            let mut sig_tx = SignedTransaction::new(tx_for_sign);
            sig_tx
                .sign(&self.private_key, self.curve_type)
                .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;
            built.signature = Some(sig_tx.signature.clone());
            // Keep the locally-built gas payment if node left it unset.
            if built.gas_payment.is_none() {
                let _ = gas_payment; // built already carries it
            }
            let _ = signed_tx; // original nonce-0 signatures discarded (mismatched)
            let status = rpc
                .call_function(built)
                .await
                .map_err(|e| MikaStorageError::OnChain(format!("RPC call_function: {e}")))?;
            if !status.success {
                return Err(MikaStorageError::OnChain(format!(
                    "tx {} failed: {}",
                    status.hash, status.status
                )));
            }
            eprintln!(
                "mika: tx {} submitted via RPC {} (status {})",
                status.hash,
                self.rpc_url.as_deref().unwrap_or("?"),
                status.status
            );
            return Ok(status.hash);
        }

        let engine = self.engine.read().await;
        engine
            .submit_transactions_batch(vec![signed_tx])
            .map_err(|e| MikaStorageError::OnChain(e.to_string()))?;
        engine
            .produce_owned_fast_checkpoint()
            .map_err(|e| MikaStorageError::OnChain(e.to_string()))?;
        // In-memory engine has no hash to report.
        Ok(String::new())
    }
}

#[async_trait::async_trait]
impl StoragePayment for KanariStorage {
    async fn pay_storage_rent(&self, payment: &StorageFeePayment) -> Result<()> {
        let storage_id = self.storage_object_id.as_deref().ok_or_else(|| {
            MikaStorageError::OnChain(
                "storage_object_id not configured. Call with_storage_object_id() first".into(),
            )
        })?;

        // Move: pay_storage_rent(storage_id: address, blob_id: address,
        //                         amount: u64, payer: address)
        // `address` args are raw 32 bytes (not BCS Vec<u8> with length prefix).
        // `storage_id` must be the FIRST arg (plus object input for borrow).
        let storage_addr = KanariAddress::from_hex_literal(storage_id)
            .map_err(|e| MikaStorageError::OnChain(format!("bad storage id {storage_id}: {e}")))?;
        let args = vec![
            storage_addr.to_bytes().to_vec(),
            payment.blob_id.as_bytes().to_vec(),
            bcs_to_bytes(&payment.amount)
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
            payment.payer.to_bytes().to_vec(),
        ];

        // Object input: Storage object (mutable)
        let object_inputs = vec![ObjectInput {
            object_ref: ObjectRef {
                object_id: storage_id.to_string(),
                version: None,
                digest: None,
            },
            owner: Some(ObjectOwnerKind::Shared),
            mutable: true,
        }];

        let signed_tx = self.build_execute_function_tx("pay_storage_rent", args, object_inputs)?;

        self.submit_transaction(signed_tx).await?;
        Ok(())
    }

    async fn anchor_blob(&self, anchor: &BlobAnchor) -> Result<()> {
        let storage_id = self.storage_object_id.as_deref().ok_or_else(|| {
            MikaStorageError::OnChain(
                "storage_object_id not configured. Call with_storage_object_id() first".into(),
            )
        })?;

        // Move: anchor_blob(storage_id: address, blob_id: address, owner: address,
        //                         commitment: vector<u8>, epoch: u64, fee_paid: u64,
        //                         ctx: &mut TxContext /* injected by node */)
        let storage_addr = KanariAddress::from_hex_literal(storage_id)
            .map_err(|e| MikaStorageError::OnChain(format!("bad storage id {storage_id}: {e}")))?;
        let args = vec![
            storage_addr.to_bytes().to_vec(),
            anchor.blob_id.as_bytes().to_vec(),
            anchor.owner.to_bytes().to_vec(),
            bcs_to_bytes(&anchor.commitment.to_vec())
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
            bcs_to_bytes(&anchor.epoch)
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
            bcs_to_bytes(&anchor.fee_paid)
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
        ];

        // Object input: Storage object (mutable)
        let object_inputs = vec![ObjectInput {
            object_ref: ObjectRef {
                object_id: storage_id.to_string(),
                version: None,
                digest: None,
            },
            owner: Some(ObjectOwnerKind::Shared),
            mutable: true,
        }];

        let signed_tx = self.build_execute_function_tx("anchor_blob", args, object_inputs)?;

        self.submit_transaction(signed_tx).await?;
        Ok(())
    }

    async fn transfer_blob_ownership(
        &self,
        blob_id: &BlobID,
        from: &KanariAddress,
        to: &KanariAddress,
        rent_transfer_amount: u64,
    ) -> Result<()> {
        let storage_id = self.storage_object_id.as_deref().ok_or_else(|| {
            MikaStorageError::OnChain(
                "storage_object_id not configured. Call with_storage_object_id() first".into(),
            )
        })?;

        // Move: transfer_blob_ownership(storage_id: address, blob_id: address,
        // from: address, to: address, rent_transfer_amount: u64)
        let storage_addr = KanariAddress::from_hex_literal(storage_id)
            .map_err(|e| MikaStorageError::OnChain(format!("bad storage id {storage_id}: {e}")))?;
        let args = vec![
            storage_addr.to_bytes().to_vec(),
            blob_id.as_bytes().to_vec(),
            from.to_bytes().to_vec(),
            to.to_bytes().to_vec(),
            bcs_to_bytes(&rent_transfer_amount)
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
        ];

        // Object input: Storage object (mutable)
        let object_inputs = vec![ObjectInput {
            object_ref: ObjectRef {
                object_id: storage_id.to_string(),
                version: None,
                digest: None,
            },
            owner: Some(ObjectOwnerKind::Shared),
            mutable: true,
        }];

        let signed_tx =
            self.build_execute_function_tx("transfer_blob_ownership", args, object_inputs)?;

        self.submit_transaction(signed_tx).await?;
        Ok(())
    }
}

impl KanariStorage {
    /// Walrus-style: buy storage quota on-chain (top-up `Storage.quota`).
    /// The quota persists on the shared Storage object; per-blob anchors
    /// record their own `fee_paid` against it.
    pub async fn buy_storage(&self, amount: u64) -> Result<String> {
        let storage_id = self.storage_object_id.as_deref().ok_or_else(|| {
            MikaStorageError::OnChain(
                "storage_object_id not configured. Call with_storage_object_id() first".into(),
            )
        })?;
        // Move: buy_storage(storage_id: address, amount: u64)
        let storage_addr = KanariAddress::from_hex_literal(storage_id)
            .map_err(|e| MikaStorageError::OnChain(format!("bad storage id {storage_id}: {e}")))?;
        let args = vec![
            storage_addr.to_bytes().to_vec(),
            bcs_to_bytes(&amount).map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
        ];
        let object_inputs = vec![ObjectInput {
            object_ref: ObjectRef {
                object_id: storage_id.to_string(),
                version: None,
                digest: None,
            },
            owner: Some(ObjectOwnerKind::Shared),
            mutable: true,
        }];
        let signed_tx = self.build_execute_function_tx("buy_storage", args, object_inputs)?;
        self.submit_transaction(signed_tx).await
    }
}

/// Verify all certificates in a WriteCertificateQuorum.
pub fn verify_quorum(quorum: &WriteCertificateQuorum) -> Result<bool> {
    quorum.verify_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kanari_storage_from_private_key() {
        let storage = KanariStorage::from_private_key(
            "0000000000000000000000000000000000000000000000000000000000000001",
            CurveType::K256,
            "node-1",
        )
        .unwrap();
        assert!(!storage.public_key().is_empty());
        assert_eq!(storage.node_id(), "node-1");
        assert!(storage.storage_object_id().is_none());
    }

    #[test]
    fn kanari_storage_sign_and_verify_quorum() {
        let storage = KanariStorage::from_private_key(
            "0000000000000000000000000000000000000000000000000000000000000001",
            CurveType::K256,
            "node-1",
        )
        .unwrap();
        let blob_id = BlobID::from_content(b"integration test");
        let mut quorum = WriteCertificateQuorum::new(blob_id, 3, 2, 1, 100);
        quorum.add(crate::certificate::WriteCertificate {
            blob_id,
            shard_index: 0,
            node_id: storage.node_id().to_string(),
            epoch: 1,
            sliver_hash: [1u8; 32],
            signature: vec![],
            public_key: storage.public_key().to_string(),
            curve_type: "K256".to_string(),
        });
        storage.sign_quorum(&mut quorum).unwrap();
        assert!(verify_quorum(&quorum).unwrap());
    }

    #[test]
    fn kanari_storage_with_storage_object_id() {
        let storage = KanariStorage::from_private_key(
            "0000000000000000000000000000000000000000000000000000000000000001",
            CurveType::K256,
            "node-1",
        )
        .unwrap();
        let storage_with_id = storage.with_storage_object_id("0xabc123");
        assert_eq!(storage_with_id.storage_object_id(), Some("0xabc123"));
    }
}
