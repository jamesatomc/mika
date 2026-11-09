module mika::committee {

    use kanari_system::object::{Self, UID};
    use kanari_system::tx_context::{Self, TxContext};
    use kanari_system::event;
    use kanari_system::transfer;
    use kanari_system::borrow;

    const E_ALREADY_MEMBER: u64 = 0;
    const E_NOT_MEMBER: u64 = 1;

    /// Walrus-style committee (mirrors the shared system object on Sui):
    /// the active set of storage-node keys whose sliver receipts count
    /// toward Proof-of-Availability.
    public struct Committee has key, store, drop {
        id: UID,
        members: vector<CommitteeMember>
    }

    public struct CommitteeMember has store, drop {
        node_key: address,
        shard_count: u64,
        joined_at: u64
    }

    public struct CommitteeInitializedEvent has copy, drop, store {
        committee_id: address
    }

    public struct CommitteeJoinedEvent has copy, drop, store {
        node_key: address,
        shard_count: u64
    }

    public struct CommitteeLeftEvent has copy, drop, store {
        node_key: address
    }

    public fun init(ctx: &mut TxContext) {
        let committee = Committee {
            id: object::new(ctx),
            members: vector::empty()
        };
        let committee_address = object::uid_address(&committee.id);
        transfer::share_object(committee);
        event::emit(
            CommitteeInitializedEvent {
                committee_id: committee_address
            }
        );
    }

    /// A storage node key registers as a committee member (Walrus: the node
    /// joins the active set for upcoming epochs).
    public fun join_committee(
        committee_id: address,
        node_key: address,
        shard_count: u64,
        ctx: &mut TxContext
    ) {
        let committee = borrow::borrow_mut<Committee>(committee_id);
        assert!(!member_exists(&committee.members, node_key), E_ALREADY_MEMBER);
        let member = CommitteeMember {
            node_key,
            shard_count,
            joined_at: tx_context::epoch_timestamp_ms(ctx)
        };
        vector::push_back(&mut committee.members, member);
        event::emit(
            CommitteeJoinedEvent {
                node_key,
                shard_count
            }
        );
    }

    /// A node key leaves the committee.
    public fun leave_committee(
        committee_id: address,
        node_key: address
    ) {
        let committee = borrow::borrow_mut<Committee>(committee_id);
        let mut i = 0;
        let mut found = false;
        while (i < vector::length(&committee.members)) {
            let member = vector::borrow(&committee.members, i);
            if (member.node_key == node_key) {
                found = true;
            };
            i = i + 1;
        };
        assert!(found, E_NOT_MEMBER);
        let idx = find_member_index(&committee.members, node_key);
        vector::remove(&mut committee.members, idx);
        event::emit(
            CommitteeLeftEvent {
                node_key
            }
        );
    }

    /// Number of active committee members.
    public fun committee_size(committee_id: address): u64 {
        let committee = borrow::borrow<Committee>(committee_id);
        vector::length(&committee.members)
    }

    /// Is `node_key` currently a committee member?
    public fun is_member(committee_id: address, node_key: address): bool {
        let committee = borrow::borrow<Committee>(committee_id);
        member_exists(&committee.members, node_key)
    }

    fun member_exists(members: &vector<CommitteeMember>, node_key: address): bool {
        let mut i = 0;
        let mut found = false;
        while (i < vector::length(members)) {
            let member = vector::borrow(members, i);
            if (member.node_key == node_key) {
                found = true;
            };
            i = i + 1;
        };
        found
    }

    fun find_member_index(members: &vector<CommitteeMember>, node_key: address): u64 {
        let mut i = 0;
        let mut found = false;
        let mut result = 0;
        while (i < vector::length(members)) {
            let member = vector::borrow(members, i);
            if (member.node_key == node_key) {
                found = true;
                result = i;
            };
            i = i + 1;
        };
        if (!found) {
            abort E_NOT_MEMBER
        };
        result
    }

}
