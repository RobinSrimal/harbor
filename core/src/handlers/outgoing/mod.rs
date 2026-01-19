//! Outgoing connection handlers
//!
//! Contains methods for initiating connections to other nodes:
//! - Send protocol: message delivery to peers
//! - Harbor protocol: store/pull/ack with Harbor Nodes, DHT lookups
//! - Share protocol: file sharing with peers
//! - DHT tests: tests for DHT types

mod dht; // Tests only
mod harbor;
mod send;
mod share;

// Note: send, harbor, and share add impl blocks to Protocol, no re-exports needed
