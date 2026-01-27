//! Send Service - Configuration and shared types
//!
//! This module contains shared configuration for the Send service.
//! The actual implementation is split into:
//! - `outgoing.rs` - SendService (single entry point, sending packets)
//! - `incoming.rs` - Processing incoming packets

use std::time::Duration;

/// Configuration for the Send service
#[derive(Debug, Clone)]
pub struct SendConfig {
    /// Timeout for sending to a single recipient (default: 5 seconds)
    pub send_timeout: Duration,
    /// How long to wait for receipts before replicating to Harbor (default: 30 seconds)
    pub receipt_timeout: Duration,
    /// Maximum concurrent sends (default: 10)
    pub max_concurrent_sends: usize,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            send_timeout: Duration::from_secs(5),
            receipt_timeout: Duration::from_secs(30),
            max_concurrent_sends: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_config_default() {
        let config = SendConfig::default();
        assert_eq!(config.send_timeout, Duration::from_secs(5));
        assert_eq!(config.receipt_timeout, Duration::from_secs(30));
        assert_eq!(config.max_concurrent_sends, 10);
    }
}
