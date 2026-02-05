//! Harbor protocol data layer
//!
//! Handles persistence for:
//! - Harbor cache (packets stored for offline recipients)
//! - Pulled packets tracking (deduplication)
//! - Harbor nodes cache (DHT lookup results)

pub mod cache;
pub mod dedup;
pub mod nodes;

// Re-export commonly used items
pub use cache::{
    all_recipients_delivered, cache_packet, cache_packet_with_ttl, cleanup_expired, delete_packet,
    get_active_harbor_ids, get_cached_packet, get_delivered_recipients,
    get_harbor_cache_stats, get_packets_for_recipient, get_packets_for_sync,
    get_undelivered_recipients, mark_delivered, mark_pulled, was_pulled,
    cleanup_old_pulled_packets, cleanup_pulled_for_topic, get_pulled_packet_ids,
    CachedPacket, PACKET_LIFETIME_SECS, WILDCARD_RECIPIENT,
};
pub use dedup::{
    check_and_mark as dedup_check_and_mark, get_total_tracked_count, get_tracked_count,
    DedupError, DedupResult,
};
pub use nodes::{get_all_active_harbor_ids, get_cached_harbor_nodes, replace_harbor_nodes};

