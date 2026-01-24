//! Send Service - Configuration and shared types
//!
//! This module contains shared configuration for the Send service.
//! The actual implementation is split into:
//! - `outgoing.rs` - Sending packets to recipients
//! - `incoming.rs` - Processing incoming packets

use std::time::Duration;

use crate::resilience::PoWConfig;

/// Configuration for the Send service
#[derive(Debug, Clone)]
pub struct SendConfig {
    /// Timeout for sending to a single recipient (default: 5 seconds)
    pub send_timeout: Duration,
    /// How long to wait for receipts before replicating to Harbor (default: 30 seconds)
    pub receipt_timeout: Duration,
    /// Maximum concurrent sends (default: 10)
    pub max_concurrent_sends: usize,
    /// Proof of Work configuration for receiving packets
    pub pow_config: PoWConfig,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            send_timeout: Duration::from_secs(5),
            receipt_timeout: Duration::from_secs(30),
            max_concurrent_sends: 10,
            pow_config: PoWConfig::default(),
        }
    }
}

impl SendConfig {
    /// Create config with PoW disabled (for testing)
    pub fn without_pow() -> Self {
        Self {
            pow_config: PoWConfig::disabled(),
            ..Default::default()
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
        assert!(config.pow_config.enabled); // PoW enabled by default
    }

    #[test]
    fn test_send_config_without_pow() {
        let config = SendConfig::without_pow();
        assert!(!config.pow_config.enabled);
    }
}
