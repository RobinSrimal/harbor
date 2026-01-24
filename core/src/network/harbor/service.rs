//! Harbor Node service
//!
//! Provides the core service struct and configuration for Harbor operations.
//!
//! The implementation is split across:
//! - `service.rs` (this file): Core struct, constructors, error types
//! - `incoming.rs`: Incoming request handlers (store, pull, ack, sync)
//! - `outgoing.rs`: Outgoing operations (create requests, process responses)
//!
//! See root README for `RUST_LOG` configuration.

use std::sync::Mutex;
use std::time::Duration;

use crate::resilience::{PoWConfig, PoWVerifyResult, RateLimitConfig, RateLimiter};
use crate::resilience::{StorageConfig, StorageManager};

/// Harbor Node service
///
/// Handles both server-side (acting as Harbor Node) and client-side
/// (storing to / pulling from Harbor Nodes) operations.
///
/// Includes abuse protection:
/// - Rate limiting (per-connection and per-HarborID limits)
/// - Proof of Work (requires computational work before storing)
/// - Storage limits (total and per-HarborID quotas)
pub struct HarborService {
    /// Local node's EndpointID
    pub(super) endpoint_id: [u8; 32],
    /// Rate limiter for abuse prevention (thread-safe via Mutex)
    pub(super) rate_limiter: Mutex<RateLimiter>,
    /// Proof of Work configuration
    pub(super) pow_config: PoWConfig,
    /// Storage quota manager
    pub(super) storage_manager: StorageManager,
}

impl HarborService {
    /// Create a new Harbor service with default configs (platform-appropriate storage)
    pub fn new(endpoint_id: [u8; 32]) -> Self {
        Self::with_full_config(
            endpoint_id,
            RateLimitConfig::default(),
            PoWConfig::default(),
            StorageConfig::default(),
        )
    }

    /// Create a new Harbor service with custom rate limit and PoW configs
    pub fn with_config(
        endpoint_id: [u8; 32],
        rate_config: RateLimitConfig,
        pow_config: PoWConfig,
    ) -> Self {
        Self::with_full_config(endpoint_id, rate_config, pow_config, StorageConfig::default())
    }

    /// Create a new Harbor service with all custom configs
    pub fn with_full_config(
        endpoint_id: [u8; 32],
        rate_config: RateLimitConfig,
        pow_config: PoWConfig,
        storage_config: StorageConfig,
    ) -> Self {
        Self {
            endpoint_id,
            rate_limiter: Mutex::new(RateLimiter::with_config(rate_config)),
            pow_config,
            storage_manager: StorageManager::new(storage_config),
        }
    }

    /// Create a new Harbor service with custom rate limit config (default PoW and storage)
    pub fn with_rate_limit_config(endpoint_id: [u8; 32], config: RateLimitConfig) -> Self {
        Self::with_config(endpoint_id, config, PoWConfig::default())
    }

    /// Create a new Harbor service with all abuse protection disabled (for testing)
    pub fn without_rate_limiting(endpoint_id: [u8; 32]) -> Self {
        Self::with_full_config(
            endpoint_id,
            RateLimitConfig {
                enabled: false,
                ..Default::default()
            },
            PoWConfig::disabled(),
            StorageConfig::disabled(),
        )
    }

    /// Get the PoW difficulty bits (for clients to know what difficulty to use)
    pub fn pow_difficulty(&self) -> u8 {
        self.pow_config.difficulty_bits
    }

    /// Check if PoW is required
    pub fn pow_enabled(&self) -> bool {
        self.pow_config.enabled
    }

    /// Get the storage configuration
    pub fn storage_config(&self) -> &StorageConfig {
        self.storage_manager.config()
    }
}

/// Harbor service error
#[derive(Debug)]
pub enum HarborError {
    /// Database error
    Database(rusqlite::Error),
    /// Protocol error
    Protocol(String),
    /// Unauthorized - claimed identity doesn't match authenticated connection
    Unauthorized,
    /// Rate limited - too many requests
    RateLimited {
        /// Time to wait before retrying
        retry_after: Duration,
    },
    /// Proof of Work required but not provided
    PoWRequired,
    /// Invalid Proof of Work
    InvalidPoW(PoWVerifyResult),
    /// Total storage limit exceeded
    StorageFull {
        /// Current total bytes stored
        current: u64,
        /// Maximum allowed bytes
        limit: u64,
    },
    /// Per-HarborID storage quota exceeded
    HarborQuotaExceeded {
        /// The HarborID that exceeded quota
        harbor_id: [u8; 32],
        /// Current bytes stored for this HarborID
        current: u64,
        /// Maximum allowed bytes per HarborID
        limit: u64,
    },
}

impl std::fmt::Display for HarborError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarborError::Database(e) => write!(f, "database error: {}", e),
            HarborError::Protocol(e) => write!(f, "protocol error: {}", e),
            HarborError::Unauthorized => {
                write!(f, "unauthorized: claimed identity doesn't match connection")
            }
            HarborError::RateLimited { retry_after } => {
                write!(f, "rate limited: retry after {}ms", retry_after.as_millis())
            }
            HarborError::PoWRequired => write!(f, "proof of work required"),
            HarborError::InvalidPoW(result) => write!(f, "invalid proof of work: {}", result),
            HarborError::StorageFull { current, limit } => {
                write!(
                    f,
                    "storage full: {} bytes used, {} bytes limit",
                    current, limit
                )
            }
            HarborError::HarborQuotaExceeded {
                harbor_id,
                current,
                limit,
            } => {
                write!(
                    f,
                    "harbor quota exceeded for {}: {} bytes used, {} bytes limit",
                    hex::encode(&harbor_id[..8]),
                    current,
                    limit
                )
            }
        }
    }
}

impl std::error::Error for HarborError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_harbor_table;
    use crate::security::create_key_pair::generate_key_pair;
    use crate::security::send::packet::PacketBuilder;
    use crate::security::topic_keys::harbor_id_from_topic;
    use rusqlite::Connection;

    use super::super::protocol::{HarborPacketType, StoreRequest};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_harbor_table(&conn).unwrap();
        conn
    }

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_harbor_error_display() {
        let db_err = HarborError::Database(rusqlite::Error::InvalidQuery);
        assert!(db_err.to_string().contains("database error"));

        let proto_err = HarborError::Protocol("test error".to_string());
        assert_eq!(proto_err.to_string(), "protocol error: test error");

        let unauth_err = HarborError::Unauthorized;
        assert!(unauth_err.to_string().contains("unauthorized"));

        let rate_err = HarborError::RateLimited {
            retry_after: Duration::from_millis(1000),
        };
        assert!(rate_err.to_string().contains("rate limited"));
        assert!(rate_err.to_string().contains("1000ms"));

        let pow_required = HarborError::PoWRequired;
        assert!(pow_required.to_string().contains("proof of work required"));

        let pow_invalid = HarborError::InvalidPoW(PoWVerifyResult::Expired);
        assert!(pow_invalid.to_string().contains("invalid proof of work"));
        assert!(pow_invalid.to_string().contains("expired"));

        let storage_full = HarborError::StorageFull {
            current: 100,
            limit: 50,
        };
        assert!(storage_full.to_string().contains("storage full"));
        assert!(storage_full.to_string().contains("100"));
        assert!(storage_full.to_string().contains("50"));

        let quota_exceeded = HarborError::HarborQuotaExceeded {
            harbor_id: test_id(42),
            current: 200,
            limit: 100,
        };
        assert!(quota_exceeded.to_string().contains("harbor quota exceeded"));
        assert!(quota_exceeded.to_string().contains("2a2a2a2a")); // hex of [42; 32][..8]
        assert!(quota_exceeded.to_string().contains("200"));
    }

    #[test]
    fn test_rate_limiting_blocks_excessive_requests() {
        use crate::resilience::RateLimitConfig;

        let mut conn = setup_db();
        // Very strict rate limit: 2 requests per window
        let rate_config = RateLimitConfig {
            enabled: true,
            max_requests_per_connection: 2,
            max_stores_per_harbor_id: 100, // High so connection limit hits first
            window_duration: Duration::from_secs(60),
        };
        // Disable PoW so we can test rate limiting in isolation
        let pow_config = PoWConfig::disabled();
        let service = HarborService::with_config(test_id(1), rate_config, pow_config);

        let sender_keypair = generate_key_pair();
        let topic_id = test_id(20);

        // Create a valid packet for the store request
        let packet = PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();

        let packet_bytes = packet.to_bytes().unwrap();
        let harbor_id = harbor_id_from_topic(&topic_id);

        // First 2 requests should succeed
        for i in 0..2 {
            let request = StoreRequest {
                packet_id: test_id(100 + i),
                harbor_id,
                sender_id: sender_keypair.public_key,
                packet_data: packet_bytes.clone(),
                recipients: vec![test_id(40)],
                packet_type: HarborPacketType::Content,
                proof_of_work: None, // PoW disabled in this test
            };
            let result = service.handle_store(&mut conn, request);
            assert!(result.is_ok(), "Request {} should succeed", i);
        }

        // Third request should be rate limited
        let request = StoreRequest {
            packet_id: test_id(102),
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes,
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request);
        assert!(matches!(result, Err(HarborError::RateLimited { .. })));
    }

    #[test]
    fn test_rate_limiting_disabled() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        let sender_keypair = generate_key_pair();
        let topic_id = test_id(20);

        let packet = PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();

        let packet_bytes = packet.to_bytes().unwrap();
        let harbor_id = harbor_id_from_topic(&topic_id);

        // Many requests should all succeed when rate limiting is disabled
        for i in 0..100 {
            let request = StoreRequest {
                packet_id: test_id(100 + i),
                harbor_id,
                sender_id: sender_keypair.public_key,
                packet_data: packet_bytes.clone(),
                recipients: vec![test_id(40)],
                packet_type: HarborPacketType::Content,
                proof_of_work: None,
            };
            let result = service.handle_store(&mut conn, request);
            assert!(
                result.is_ok(),
                "Request {} should succeed with rate limiting disabled",
                i
            );
        }
    }

    #[test]
    fn test_storage_limits_block_when_full() {
        use crate::resilience::StorageConfig;

        let mut conn = setup_db();

        let sender_keypair = generate_key_pair();
        let topic_id = test_id(20);

        let packet = PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();

        let packet_bytes = packet.to_bytes().unwrap();
        let packet_size = packet_bytes.len() as u64;
        let harbor_id = harbor_id_from_topic(&topic_id);

        // Set limit to allow exactly 1 packet but not 2
        let storage_config = StorageConfig {
            max_total_bytes: packet_size + 10, // Just enough for 1 packet
            max_per_harbor_bytes: packet_size * 10, // High so total limit hits first
            enabled: true,
        };
        let service = HarborService::with_full_config(
            test_id(1),
            RateLimitConfig {
                enabled: false,
                ..Default::default()
            },
            PoWConfig::disabled(),
            storage_config,
        );

        // First request should succeed
        let request1 = StoreRequest {
            packet_id: packet.packet_id, // Use actual packet ID
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes.clone(),
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request1);
        let response = result.expect("First request should not error");
        assert!(
            response.success,
            "First store should succeed: {:?}",
            response.error
        );

        // Second request should fail - storage full
        // Need a new packet with different packet_id
        let packet2 = PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"second message")
        .unwrap();
        let packet_bytes2 = packet2.to_bytes().unwrap();

        let request2 = StoreRequest {
            packet_id: packet2.packet_id,
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes2,
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request2);
        assert!(
            matches!(result, Err(HarborError::StorageFull { .. })),
            "Second request should fail with StorageFull, got: {:?}",
            result
        );
    }

    #[test]
    fn test_storage_per_harbor_limit() {
        use crate::resilience::StorageConfig;

        let mut conn = setup_db();

        let sender_keypair = generate_key_pair();
        let topic_id = test_id(20);

        let packet = PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();

        let packet_bytes = packet.to_bytes().unwrap();
        let packet_size = packet_bytes.len() as u64;
        let harbor_id = harbor_id_from_topic(&topic_id);

        // Set per-harbor limit to allow exactly 1 packet but not 2
        let storage_config = StorageConfig {
            max_total_bytes: packet_size * 100, // High so per-harbor limit hits first
            max_per_harbor_bytes: packet_size + 10, // Just enough for 1 packet
            enabled: true,
        };
        let service = HarborService::with_full_config(
            test_id(1),
            RateLimitConfig {
                enabled: false,
                ..Default::default()
            },
            PoWConfig::disabled(),
            storage_config,
        );

        // First request to harbor_id should succeed
        let request1 = StoreRequest {
            packet_id: packet.packet_id, // Use actual packet ID
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes.clone(),
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request1);
        let response = result.expect("First request should not error");
        assert!(
            response.success,
            "First store should succeed: {:?}",
            response.error
        );

        // Second request to same harbor should fail - per-harbor quota exceeded
        let packet2 = PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"second message")
        .unwrap();
        let packet_bytes2 = packet2.to_bytes().unwrap();

        let request2 = StoreRequest {
            packet_id: packet2.packet_id,
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes2,
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request2);
        assert!(
            matches!(result, Err(HarborError::HarborQuotaExceeded { .. })),
            "Second request should fail with HarborQuotaExceeded, got: {:?}",
            result
        );

        // Request to DIFFERENT harbor should succeed (different quota)
        let topic_id2 = test_id(21);
        let packet3 = PacketBuilder::new(
            topic_id2,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"third message")
        .unwrap();
        let packet_bytes3 = packet3.to_bytes().unwrap();
        let harbor_id2 = harbor_id_from_topic(&topic_id2);

        let request3 = StoreRequest {
            packet_id: packet3.packet_id,
            harbor_id: harbor_id2,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes3,
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request3);
        let response = result.expect("Third request should not error");
        assert!(
            response.success,
            "Request to different harbor should succeed: {:?}",
            response.error
        );
    }

    #[test]
    fn test_storage_disabled() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // without_rate_limiting uses StorageConfig::disabled()
        assert!(!service.storage_config().enabled);

        let sender_keypair = generate_key_pair();
        let topic_id = test_id(20);

        let packet = PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();

        let packet_bytes = packet.to_bytes().unwrap();
        let harbor_id = harbor_id_from_topic(&topic_id);

        // Many requests should all succeed when storage limits are disabled
        for i in 0..10 {
            let request = StoreRequest {
                packet_id: test_id(100 + i),
                harbor_id,
                sender_id: sender_keypair.public_key,
                packet_data: packet_bytes.clone(),
                recipients: vec![test_id(40)],
                packet_type: HarborPacketType::Content,
                proof_of_work: None,
            };
            let result = service.handle_store(&mut conn, request);
            assert!(
                result.is_ok(),
                "Request {} should succeed with storage disabled",
                i
            );
        }
    }
}
