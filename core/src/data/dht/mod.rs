//! DHT data layer
//!
//! Handles persistence for peer information and DHT routing table.

pub mod peer;
pub mod routing;

// Re-export commonly used items
pub use peer::{
    cleanup_stale_peers, current_timestamp, delete_peer, ensure_peer_exists, get_peer,
    get_peer_relay_info, get_peers_with_fresh_addresses, get_stale_peers,
    store_peer_relay_url_unverified, touch_peer, update_peer_address, update_peer_latency,
    update_peer_relay_url, upsert_peer, PeerInfo, ADDRESS_FRESHNESS_THRESHOLD_SECS,
    PEER_RETENTION_SECS,
};
pub use routing::{
    clear_all_entries, count_all_entries, count_bucket_entries, get_all_entries,
    get_bucket_entries, insert_dht_entry, load_routing_table, load_routing_table_relay_urls,
    remove_dht_entry, DhtEntry,
};
