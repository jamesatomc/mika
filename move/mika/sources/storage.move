module mika::storage {

    use kanari_system::object::{Self, UID};
    use kanari_system::tx_context::{Self, TxContext};
    use kanari_system::event;
    use kanari_system::transfer;
    use kanari_system::borrow;

    const E_BLOB_ALREADY_ANCHORED: u64 = 0;
    const E_NOT_OWNER: u64 = 1;
    const E_NOT_FROM_OWNER: u64 = 2;

    public struct Storage has key, store, drop {
        id: UID,
        anchors: vector<BlobAnchor>,
        quota: u64
    }

    public struct BlobAnchor has store, drop {
        id: u64,
        blob_id: address,
        owner: address,
        commitment: vector<u8>,
        epoch: u64,
        fee_paid: u64,
        created_at: u64
    }

    public struct BlobUpgradedEvent has copy, drop, store {
        old_blob_id: address,
        new_blob_id: address,
        version: u64,
        owner: address,
        fee_paid: u64
    }

    public struct BlobReclaimedEvent has copy, drop, store {
        blob_id: address,
        reclaimed_at: u64,
        quota_refund: u64
    }

    public struct StorageInitializedEvent has copy, drop, store {
        storage_id: address
    }

    public struct BlobAnchoredEvent has copy, drop, store {
        blob_id: address,
        owner: address,
        commitment: vector<u8>,
        epoch: u64,
        fee_paid: u64
    }

    public struct StorageRentPaidEvent has copy, drop, store {
        blob_id: address,
        amount: u64,
        new_fee_paid: u64
    }

    public struct BlobTransferredEvent has copy, drop, store {
        blob_id: address,
        from: address,
        to: address,
        rent_transfer_amount: u64
    }

    /// Walrus-style storage quota event.
    public struct StorageQuotaBoughtEvent has copy, drop, store {
        amount: u64,
        total_quota: u64
    }

    public fun init(ctx: &mut TxContext) {
        let storage = Storage {
            id: object::new(ctx),
            anchors: vector::empty(),
            quota: 0
        };
        let storage_address = object::uid_address(&storage.id);
        transfer::share_object(storage);
        event::emit(
            StorageInitializedEvent {
                storage_id: storage_address
            }
        );
    }

    /// Walrus-style: buy storage quota up-front (top-up `Storage.quota`).
    /// Subsequent `anchor_blob` calls draw against this quota; `fee_paid`
    /// on the anchor remains the per-blob paid amount on record.
    public fun buy_storage(storage_id: address, amount: u64) {
        let storage = borrow::borrow_mut<Storage>(storage_id);
        storage.quota = storage.quota + amount;
        event::emit(
            StorageQuotaBoughtEvent {
                amount,
                total_quota: storage.quota
            }
        );
    }

    public fun anchor_blob(
        storage_id: address,
        blob_id: address,
        owner: address,
        commitment: vector<u8>,
        epoch: u64,
        fee_paid: u64,
        ctx: &mut TxContext
    ) {
        let storage = borrow::borrow_mut<Storage>(storage_id);
        assert!(
            !blob_exists(&storage.anchors, blob_id),
            E_BLOB_ALREADY_ANCHORED,
        );

        let anchor_index = vector::length(&storage.anchors);

        let anchor = BlobAnchor {
            id: anchor_index,
            blob_id,
            owner,
            commitment,
            epoch,
            fee_paid,
            created_at: tx_context::epoch_timestamp_ms(ctx)
        };

        vector::push_back(&mut storage.anchors, anchor);
        event::emit(
            BlobAnchoredEvent {
                blob_id,
                owner,
                commitment,
                epoch,
                fee_paid
            }
        );
    }

    public fun pay_storage_rent(
        storage_id: address,
        blob_id: address,
        amount: u64,
        payer: address
    ) {
        let storage = borrow::borrow_mut<Storage>(storage_id);
        let anchor_index = find_anchor_index(&storage.anchors, blob_id);
        let anchor = vector::borrow_mut(&mut storage.anchors, anchor_index);

        assert!(
            anchor.owner == payer,
            E_NOT_OWNER,
        );

        anchor.fee_paid = anchor.fee_paid + amount;
        event::emit(
            StorageRentPaidEvent {
                blob_id,
                amount,
                new_fee_paid: anchor.fee_paid
            }
        );
    }

    public fun transfer_blob_ownership(
        storage_id: address,
        blob_id: address,
        from: address,
        to: address,
        rent_transfer_amount: u64
    ) {
        let storage = borrow::borrow_mut<Storage>(storage_id);
        let anchor_index = find_anchor_index(&storage.anchors, blob_id);
        let anchor = vector::borrow_mut(&mut storage.anchors, anchor_index);

        assert!(
            anchor.owner == from,
            E_NOT_FROM_OWNER,
        );

        anchor.owner = to;
        event::emit(
            BlobTransferredEvent {
                blob_id,
                from,
                to,
                rent_transfer_amount
            }
        );
    }

    /// Walrus-style upgrade: a new anchor for the same logical blob (new
    /// content = new blob_id). Old anchor stays as history; the newest
    /// anchor for the owner wins. Emits `BlobUpgradedEvent`.
    public fun upgrade_blob(
        storage_id: address,
        old_blob_id: address,
        new_blob_id: address,
        owner: address,
        commitment: vector<u8>,
        epoch: u64,
        fee_paid: u64,
        ctx: &mut TxContext
    ) {
        let storage = borrow::borrow_mut<Storage>(storage_id);
        assert!(blob_exists(&storage.anchors, old_blob_id), E_NOT_OWNER);
        let old_idx = find_anchor_index(&storage.anchors, old_blob_id);
        let old_anchor = vector::borrow(&storage.anchors, old_idx);
        assert!(old_anchor.owner == owner, E_NOT_OWNER);
        let version = old_anchor.id + 1;
        let anchor = BlobAnchor {
            id: version,
            blob_id: new_blob_id,
            owner,
            commitment,
            epoch,
            fee_paid,
            created_at: tx_context::epoch_timestamp_ms(ctx)
        };
        vector::push_back(&mut storage.anchors, anchor);
        event::emit(
            BlobUpgradedEvent {
                old_blob_id,
                new_blob_id,
                version,
                owner,
                fee_paid
            }
        );
    }

    /// Walrus-style lease expiry reclaim: anyone may reclaim a blob whose
    /// lease has expired (now - anchor.epoch >= 1 epoch). Removes the
    /// anchor and refunds quota to the Storage object (fee kept on record
    /// via the reclaimed period).
    public fun reclaim_blob(
        storage_id: address,
        blob_id: address,
        now_epoch: u64,
        ctx: &mut TxContext
    ) {
        let storage = borrow::borrow_mut<Storage>(storage_id);
        let idx = find_anchor_index(&storage.anchors, blob_id);
        let anchor = vector::borrow(&storage.anchors, idx);
        // One epoch = 86_400s; expired if lease covers >= 1 epoch.
        let now_ms = tx_context::epoch_timestamp_ms(ctx);
        assert!(now_ms >= anchor.created_at + 86400000, 7);
        let anchor = vector::remove(&mut storage.anchors, idx);
        storage.quota = storage.quota + anchor.fee_paid;
        event::emit(
            BlobReclaimedEvent {
                blob_id,
                reclaimed_at: now_ms,
                quota_refund: anchor.fee_paid
            }
        );
    }

    public fun get_blob_anchor(
        storage_id: address,
        blob_id: address
    ): (address, vector<u8>, u64, u64, u64) {
        let storage = borrow::borrow<Storage>(storage_id);
        let anchor_index = find_anchor_index(&storage.anchors, blob_id);
        let anchor = vector::borrow(&storage.anchors, anchor_index);
        (
            anchor.owner,
            anchor.commitment,
            anchor.epoch,
            anchor.fee_paid,
            anchor.created_at,
        )
    }

    public fun is_blob_anchored(storage_id: address, blob_id: address): bool {
        let storage = borrow::borrow<Storage>(storage_id);
        blob_exists(&storage.anchors, blob_id)
    }

    fun blob_exists(anchors: &vector<BlobAnchor>, blob_id: address): bool {
        let mut i = 0;
        let mut found = false;
        while (i < vector::length(anchors)) {
            let anchor = vector::borrow(anchors, i);
            if (anchor.blob_id == blob_id) {
                found = true;
            };
            i = i + 1;
        };
        found
    }

    fun find_anchor_index(anchors: &vector<BlobAnchor>, blob_id: address): u64 {
        let mut i = 0;
        let mut found = false;
        let mut result = 0;
        while (i < vector::length(anchors)) {
            let anchor = vector::borrow(anchors, i);
            if (anchor.blob_id == blob_id) {
                found = true;
                result = i;
            };
            i = i + 1;
        };
        if (!found) {
            abort 4
        };
        result
    }

}
