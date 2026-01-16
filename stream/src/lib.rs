//! Harbor Stream - Real-time Audio/Video
//!
//! This crate provides real-time streaming capabilities for Harbor topics,
//! enabling audio/video calls and other streaming use cases.
//!
//! # Status
//!
//! This crate is currently a placeholder. Implementation is planned.
//!
//! # Planned Features
//!
//! - Per-topic audio/video streams
//! - Low-latency peer-to-peer streaming
//! - Automatic quality adaptation
//! - Built on [iroh-live](https://github.com/n0-computer/iroh-live) or WebRTC

#![allow(unused)]

/// Re-export core types for convenience
pub use harbor_core;

/// Placeholder for future Stream protocol
pub struct StreamProtocol {
    _private: (),
}

impl StreamProtocol {
    /// Create a new StreamProtocol (not yet implemented)
    pub fn new() -> Self {
        todo!("harbor-stream is not yet implemented")
    }
}

