//! Storage epochs and lease management.
//!
//! Storage is rented in epochs. When an epoch expires, the blob's slivers may be
//! reclaimed by storage nodes unless the lease is renewed.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// An opaque epoch identifier (Unix seconds at which the epoch started).
pub type Epoch = u64;

/// Current epoch in seconds since the Unix epoch.
pub fn current_epoch() -> Epoch {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

    pub fn is_expired(&self, now: Epoch) -> bool {
        now >= self.expires_at
    }

    pub fn renew(&mut self, additional_seconds: u64, additional_fee: u64) {
        self.expires_at += additional_seconds;
        self.fee_paid += additional_fee;
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
}
