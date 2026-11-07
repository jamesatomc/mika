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
use hex::encode as hex_encode;
use kanari_common::{get_active_address, load_kanari_config};
use kanari_core::BlockchainEngine;
use kanari_crypto::keystore::Keystore;
use kanari_crypto::{
    encryption::decrypt_data,
    keys::{CurveType, keypair_from_private_key},
};
use std::sync::Arc;
use tokio::sync::RwLock;

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
        })
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
}

#[async_trait::async_trait]
impl StoragePayment for KanariStorage {
    async fn pay_storage_rent(&self, payment: &StorageFeePayment) -> Result<()> {
        // Submit a native KANARI transfer transaction via BlockchainEngine
        // to pay storage rent from the payer's account.
        let _engine = self.engine.read().await;
        let _payment = payment;
        // TODO: Implement using BlockchainEngine's transaction submission API
        Err(MikaStorageError::OnChain(
            "pay_storage_rent not yet implemented against BlockchainEngine".into(),
        ))
    }

    async fn anchor_blob(&self, anchor: &BlobAnchor) -> Result<()> {
        // Anchor blob metadata on-chain via BlockchainEngine.
        let _engine = self.engine.read().await;
        let _anchor = anchor;
        Err(MikaStorageError::OnChain(
            "anchor_blob not yet implemented against BlockchainEngine".into(),
        ))
    }

    async fn transfer_blob_ownership(
        &self,
        blob_id: &BlobID,
        from: &kanari_types::address::Address,
        to: &kanari_types::address::Address,
        rent_transfer_amount: u64,
    ) -> Result<()> {
        let _engine = self.engine.read().await;
        let _blob_id = blob_id;
        let _from = from;
        let _to = to;
        let _rent_transfer_amount = rent_transfer_amount;
        Err(MikaStorageError::OnChain(
            "transfer_blob_ownership not yet implemented against BlockchainEngine".into(),
        ))
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
}
