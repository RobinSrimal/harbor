//! DHT internal modules
//!
//! Helper modules for DHT implementation:
//! - `api`: High-level API client for DHT operations
//! - `bootstrap`: Bootstrap node configuration
//! - `config`: DHT configuration and factory functions
//! - `distance`: XOR distance metric and Id type
//! - `lookup`: Kademlia iterative lookup algorithm
//! - `pool`: Connection pooling for DHT queries
//! - `routing`: K-bucket routing table

pub mod api;
pub mod bootstrap;
pub mod config;
pub mod distance;
pub mod lookup;
pub mod pool;
pub mod routing;
