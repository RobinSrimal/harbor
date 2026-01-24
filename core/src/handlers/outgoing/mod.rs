//! Outgoing connection handlers
//!
//! Contains methods for initiating connections to other nodes:
//! - Send protocol: message delivery to peers
//! - Harbor protocol: store/pull/ack with Harbor Nodes, DHT lookups
//! - Share protocol: file sharing with peers
//! - DHT protocol: FindNode RPC for peer discovery
//! - Sync protocol: CRDT state sync responses

mod dht;
mod harbor;
mod send;
mod share;
mod sync;

// Re-export DHT handler for use by the DHT actor
pub use dht::send_find_node;

// Note: send, harbor, share, and sync add impl blocks to Protocol, no re-exports needed
