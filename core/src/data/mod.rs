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
//! - File sharing (blobs)
//! - CRDT sync documents
//! - Peer connections and invites (control)
//!
//! Organized by protocol domain (mirrors network/ structure):
//! - `dht/` - Peer information and DHT routing
//! - `send/` - Outgoing packet tracking
//! - `harbor/` - Harbor cache and deduplication
//! - `membership/` - Topic subscriptions and members
//! - `share/` - File sharing metadata and blob storage
//! - `control/` - Peer connections, tokens, pending invites

pub mod control;
pub mod dht;
pub mod harbor;
pub mod identity;
pub mod membership;
pub mod schema;
pub mod send;
pub mod share;
pub mod start;

// Re-export commonly used items from dht/
pub use dht::{
    cleanup_stale_peers, current_timestamp, ensure_peer_exists, get_peer_relay_info,
    update_peer_relay_url, PeerInfo, PEER_RETENTION_SECS,
};

// Re-export commonly used items from harbor/
pub use harbor::{
    cache_packet, cleanup_old_pulled_packets, cleanup_pulled_for_topic, get_cached_packet,
    get_packets_for_recipient, get_pulled_packet_ids, mark_delivered, mark_pulled, was_pulled,
    CachedPacket, PACKET_LIFETIME_SECS, WILDCARD_RECIPIENT,
};
pub use harbor::{
    dedup_check_and_mark, get_total_tracked_count, get_tracked_count, DedupError, DedupResult,
};

// Re-export commonly used items from identity
pub use identity::{ensure_self_in_peers, get_or_create_identity, LocalIdentity};

// Re-export commonly used items from send/
pub use send::{
    acknowledge_receipt, get_packets_needing_replication, mark_replicated_to_harbor,
    store_outgoing_packet,
};

// Re-export commonly used items from membership/
pub use membership::{
    add_topic_member, get_all_topics, get_joined_at, get_topic,
    get_topic_admin, get_topic_members, get_topics_for_member, is_topic_admin,
    remove_topic_member, subscribe_topic, subscribe_topic_with_admin,
    unsubscribe_topic, TopicMember, TopicSubscription,
};
// Re-export epoch key functions
pub use membership::{
    cleanup_expired_epoch_keys, delete_epoch_keys_for_topic, get_all_epoch_keys,
    get_current_epoch, get_current_epoch_key, get_epoch_key, store_epoch_key,
    EpochKey, EPOCH_KEY_LIFETIME_SECS,
};

// Re-export commonly used items from schema
pub use schema::create_all_tables;

// Re-export commonly used items from start
pub use start::{start_db, start_memory_db, StartError};

// Re-export commonly used items from share/
pub use share::{
    add_blob_recipient, delete_blob, get_blob, get_blobs_for_scope, get_section_peer_suggestion,
    get_section_traces, init_blob_sections, insert_blob, is_distribution_complete,
    mark_blob_complete, mark_recipient_complete, record_peer_can_seed, record_section_received,
    BlobMetadata, BlobState, SectionTrace, CHUNK_SIZE,
};
pub use share::{default_blob_path, BlobStore};

// Re-export commonly used items from control/
pub use control::{
    consume_token, create_connect_token, delete_connection, delete_pending_invite,
    get_connection, get_pending_invite, is_peer_blocked, is_peer_connected,
    list_all_connections, list_connections_by_state, list_pending_invites,
    store_pending_invite, token_exists, update_connection_state, upsert_connection,
    ConnectionInfo, ConnectionState, PendingTopicInvite,
};
