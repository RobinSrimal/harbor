//! Outgoing connection handlers
//!
//! Contains methods for initiating connections to other nodes:
//! - Send protocol: message delivery to peers
//! - Harbor protocol: store/pull/ack with Harbor Nodes, DHT lookups
//! - DHT tests: tests for DHT types

mod dht; // Tests only
mod harbor;
mod send;

// Note: send and harbor add impl blocks to Protocol, no re-exports needed
