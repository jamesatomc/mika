//! Storage epochs and lease management.
//!
//! Storage is rented in epochs. When an epoch expires, the blob's slivers may be
//! reclaimed by storage nodes unless the lease is renewed.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// An opaque epoch identifier (Unix seconds at which the epoch started).
pub type Epoch = u64;

/// Duration of one Walrus-style storage epoch (seconds).
pub const EPOCH_SECONDS: u64 = 86_400;

/// Price charged per storage epoch (in mist) — Walrus-style per-epoch rent.
/// `fee = epochs * PRICE_PER_EPOCH` buys `epochs` epochs.
pub const PRICE_PER_EPOCH: u64 = 1_000;

/// Current epoch in seconds since the Unix epoch.
pub fn current_epoch() -> Epoch {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How many whole epochs a `fee` buys (at least 1 epoch is always bought,
/// even for a dust fee, so short uploads are not instantly expired).
pub fn epochs_for_fee(fee: u64) -> u64 {
    (fee / PRICE_PER_EPOCH).max(1)
}

/// A storage lease for a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub blob_id: crate::blob::BlobID,
    /// Epoch at which this lease expires.
    pub expires_at: Epoch,
    /// Storage fee paid (in smallest coin unit, Mist).
    pub fee_paid: u64,
}

impl Lease {
    pub fn new(blob_id: crate::blob::BlobID, ttl_seconds: u64, fee_paid: u64) -> Self {
        let expires_at = current_epoch() + ttl_seconds;
        Self {
            blob_id,
            expires_at,
            fee_paid,
        }
    }

    /// Walrus-style lease: `fee` buys whole epochs at `PRICE_PER_EPOCH`.
    pub fn new_for_epochs(blob_id: crate::blob::BlobID, epochs: u64, fee_paid: u64) -> Self {
        let epochs = epochs.max(1);
        let expires_at = current_epoch() + epochs * EPOCH_SECONDS;
        Self {
            blob_id,
            expires_at,
            fee_paid,
        }
    }

    pub fn is_expired(&self, now: Epoch) -> bool {
        now >= self.expires_at
    }

    pub fn renew(&mut self, additional_seconds: u64, additional_fee: u64) {
        self.expires_at += additional_seconds;
        self.fee_paid += additional_fee;
    }

    /// Walrus-style renewal: buy `epochs` more epochs.
    pub fn renew_epochs(&mut self, epochs: u64) {
        let epochs = epochs.max(1);
        self.expires_at += epochs * EPOCH_SECONDS;
        self.fee_paid += epochs * PRICE_PER_EPOCH;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_expiry() {
        let blob_id = crate::blob::BlobID::from_content(b"lease test");
        let mut lease = Lease::new(blob_id, 100, 1000);
        assert!(!lease.is_expired(current_epoch()));
        lease.renew(200, 500);
        assert_eq!(lease.fee_paid, 1500);
    }

    #[test]
    fn epochs_pricing() {
        assert_eq!(epochs_for_fee(0), 1, "dust fee still buys 1 epoch");
        assert_eq!(epochs_for_fee(999), 1);
        assert_eq!(epochs_for_fee(1000), 1);
        assert_eq!(epochs_for_fee(2500), 2);
        let blob_id = crate::blob::BlobID::from_content(b"walrus");
        let lease = Lease::new_for_epochs(blob_id, 2, 2000);
        assert_eq!(lease.fee_paid, 2000);
        assert!(!lease.is_expired(current_epoch()));
        assert!(lease.expires_at > current_epoch() + EPOCH_SECONDS);
        let mut l2 = lease;
        l2.renew_epochs(1);
        assert_eq!(l2.fee_paid, 3000);
    }
}
