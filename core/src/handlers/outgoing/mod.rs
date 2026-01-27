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
mod share;
mod sync;

// Re-export share handler functions for use in spawned tasks
pub use share::push_section_to_peer_standalone;

// Note: send, harbor, share, and sync add impl blocks to Protocol, no re-exports needed
