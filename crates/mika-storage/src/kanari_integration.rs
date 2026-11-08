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
use hex::encode as hex_encode;
use kanari_common::{get_active_address, load_kanari_config};
use kanari_core::BlockchainEngine;
use kanari_crypto::keystore::Keystore;
use kanari_crypto::{
    encryption::decrypt_data,
    keys::{CurveType, keypair_from_private_key},
};
use kanari_types::address::Address as KanariAddress;
use kanari_types::transaction::{
    GasPayment, ObjectInput, ObjectOwnerKind, ObjectRef, SignedTransaction, Transaction,
};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Mika Move module address (where the storage contract is deployed).
const MIKA_MODULE_ADDRESS: &str = "0x3ba63b92aac5f2bff87e580e820b61faf1c5fe9ae12f0bc8addd931a340b3146";
/// Mika Move module name.
const MIKA_MODULE_NAME: &str = "mika::storage";

/// A Mika storage node integrated with the kanari-sdk blockchain.
///
/// Wraps a `BlockchainEngine` for on-chain operations (fee payments, blob
/// anchoring, ownership transfers) and a signing key loaded from the kanari
/// config file / keystore for storage certificates.
pub struct KanariStorage {
    engine: Arc<RwLock<BlockchainEngine>>,
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

        // Resolve keystore path from config
        let keystore_path = kanari_common::get_kanari_config_path();

        // Load keystore
        let keystore = Keystore::load_from_path(&keystore_path)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;

        // Get encrypted data for the active address
        let encrypted = keystore.get_wallet(&active_address).ok_or_else(|| {
            MikaStorageError::Crypto(format!(
                "no keystore entry for active address {}",
                active_address
            ))
        })?;

        // Decrypt the private key
        let plaintext = decrypt_data(encrypted, password)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;

        let private_key_hex = hex_encode(&plaintext);

        // Derive public key from private key
        let keypair = keypair_from_private_key(&private_key_hex, CurveType::K256)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;

        let engine = BlockchainEngine::new_in_memory()
            .map_err(|e| MikaStorageError::OnChain(e.to_string()))?;

        Ok(Self {
            engine: Arc::new(RwLock::new(engine)),
            private_key: private_key_hex,
            public_key: keypair.public_key,
            address: active_address,
            curve_type: CurveType::K256,
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
        let sender = self.address.clone();
        let module = format!("{}::{}", MIKA_MODULE_ADDRESS, MIKA_MODULE_NAME);

        let gas_payment = Some(GasPayment {
            payment_objects: vec![],
            owner: self.address.clone(),
            budget: 1_000_000_000,
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
            gas_limit: 1_000_000_000,
            gas_price: 1000,
            nonce: 0,
        };

        let mut signed_tx = SignedTransaction::new(tx);
        signed_tx
            .sign(&self.private_key, self.curve_type)
            .map_err(|e| MikaStorageError::Crypto(e.to_string()))?;

        Ok(signed_tx)
    }

    /// Submit a signed transaction via BlockchainEngine.
    /// Submits to mempool, then produces an owned fast checkpoint.
    async fn submit_transaction(&self, signed_tx: SignedTransaction) -> Result<()> {
        let engine = self.engine.read().await;

        // Submit to mempool
        engine
            .submit_transactions_batch(vec![signed_tx])
            .map_err(|e| MikaStorageError::OnChain(e.to_string()))?;

        // Produce checkpoint to execute the transaction
        engine
            .produce_owned_fast_checkpoint()
            .map_err(|e| MikaStorageError::OnChain(e.to_string()))?;

        Ok(())
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

        // Args: blob_id (vector<u8>), amount (u64)
        let args = vec![
            bcs_to_bytes(&payment.blob_id.as_bytes().to_vec())
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
            bcs_to_bytes(&payment.amount)
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
        ];

        // Object input: Storage object (mutable)
        let object_inputs = vec![ObjectInput {
            object_ref: ObjectRef {
                object_id: storage_id.to_string(),
                version: None,
                digest: None,
            },
            owner: Some(ObjectOwnerKind::AddressOwner(self.address.clone())),
            mutable: true,
        }];

        let signed_tx = self.build_execute_function_tx("pay_storage_rent", args, object_inputs)?;

        self.submit_transaction(signed_tx).await
    }

    async fn anchor_blob(&self, anchor: &BlobAnchor) -> Result<()> {
        let storage_id = self.storage_object_id.as_deref().ok_or_else(|| {
            MikaStorageError::OnChain(
                "storage_object_id not configured. Call with_storage_object_id() first".into(),
            )
        })?;

        // Args: blob_id (vector<u8>), owner (address), commitment (vector<u8>),
        // epoch (u64), fee_paid (u64)
        let args = vec![
            bcs_to_bytes(&anchor.blob_id.as_bytes().to_vec())
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
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
            owner: Some(ObjectOwnerKind::AddressOwner(self.address.clone())),
            mutable: true,
        }];

        let signed_tx = self.build_execute_function_tx("anchor_blob", args, object_inputs)?;

        self.submit_transaction(signed_tx).await
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

        // Args: blob_id (vector<u8>), from (address), to (address),
        // rent_transfer_amount (u64)
        let args = vec![
            bcs_to_bytes(&blob_id.as_bytes().to_vec())
                .map_err(|e| MikaStorageError::Serialization(e.to_string()))?,
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
            owner: Some(ObjectOwnerKind::AddressOwner(self.address.clone())),
            mutable: true,
        }];

        let signed_tx =
            self.build_execute_function_tx("transfer_blob_ownership", args, object_inputs)?;

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
