//! Harbor Sync - CRDT Synchronization
//!
//! This crate provides CRDT (Conflict-free Replicated Data Types) synchronization
//! for Harbor topics. It enables collaborative data structures that automatically
//! merge changes from multiple peers without conflicts.
//!
//! # Status
//!
//! This crate is currently a placeholder. Implementation is planned.
//!
//! # Planned Features
//!
//! - Per-topic CRDT documents
//! - Automatic sync over Harbor Core's Send protocol
//! - Support for common CRDT types (text, lists, maps)
//! - Built on [Loro](https://github.com/loro-dev/loro) CRDT library

#![allow(unused)]

/// Re-export core types for convenience
pub use harbor_core;

/// Placeholder for future Sync protocol
pub struct SyncProtocol {
    _private: (),
}

impl SyncProtocol {
    /// Create a new SyncProtocol (not yet implemented)
    pub fn new() -> Self {
        todo!("harbor-sync is not yet implemented")
    }
}

