//! Data layer for Harbor Protocol DB
//!
//! Provides storage and retrieval for:
//! - Local node identity (key pair)
//! - Peer information (EndpointID, addresses, latency)
//! - DHT routing table entries
//! - Topics and their members
//! - Outgoing packets and read receipts
//! - Harbor cache
//! - Packet deduplication

pub mod dedup;
pub mod dht;
pub mod harbor;
pub mod identity;
pub mod outgoing;
pub mod peer;
pub mod schema;
pub mod start;
pub mod topic;

// Re-export commonly used items
pub use harbor::{
    cache_packet, cleanup_old_pulled_packets, cleanup_pulled_for_topic, get_cached_packet,
    get_packets_for_recipient, get_pulled_packet_ids, mark_delivered, mark_pulled, was_pulled,
    CachedPacket, PACKET_LIFETIME_SECS, WILDCARD_RECIPIENT,
};
pub use identity::{get_or_create_identity, LocalIdentity};
pub use outgoing::{
    acknowledge_receipt, get_packets_needing_replication, mark_replicated_to_harbor,
    store_outgoing_packet,
};
pub use peer::{cleanup_stale_peers, current_timestamp, PeerInfo, PEER_RETENTION_SECS};
pub use schema::create_all_tables;
pub use start::{start_db, start_memory_db, StartError};
pub use topic::{
    add_topic_member, add_topic_member_with_relay, get_all_topics, get_joined_at, get_topic,
    get_topic_members, get_topic_members_with_info, get_topics_for_member, remove_topic_member,
    subscribe_topic, unsubscribe_topic, TopicMember, TopicMemberInfo, TopicSubscription,
};
pub use dedup::{
    check_and_mark as dedup_check_and_mark, get_total_tracked_count, get_tracked_count,
    DedupError, DedupResult,
};
