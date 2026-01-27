//! Harbor Node service
//!
//! Provides the core service struct and configuration for Harbor operations.
//!
//! The implementation is split across:
//! - `service.rs` (this file): Core struct, constructors, error types
//! - `incoming.rs`: Incoming request handlers (store, pull, ack, sync)
//! - `outgoing.rs`: Outgoing operations (create requests, process responses, network sends)
//!
//! See root README for `RUST_LOG` configuration.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::Endpoint;
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

use crate::data::LocalIdentity;
use crate::network::dht::DhtService;
use crate::protocol::ProtocolEvent;
use crate::resilience::{PoWConfig, PoWVerifyResult, RateLimitConfig, RateLimiter};
use crate::resilience::{StorageConfig, StorageManager};

/// Harbor Node service
///
/// Handles both server-side (acting as Harbor Node) and client-side
/// (storing to / pulling from Harbor Nodes) operations.
///
/// Owns its resources (endpoint, identity, db, event channel) following
/// the same pattern as SendService, DhtService, etc.
///
/// Includes abuse protection:
/// - Rate limiting (per-connection and per-HarborID limits)
/// - Proof of Work (requires computational work before storing)
/// - Storage limits (total and per-HarborID quotas)
pub struct HarborService {
    // === Primary resources (shared with other services) ===
    /// Iroh QUIC endpoint for outgoing connections
    pub(super) endpoint: Option<Endpoint>,
    /// Local identity (keypair)
    pub(super) identity: Option<Arc<LocalIdentity>>,
    /// Database connection
    pub(super) db: Option<Arc<TokioMutex<DbConnection>>>,
    /// Event sender (for app notifications)
    pub(super) event_tx: Option<mpsc::Sender<ProtocolEvent>>,
    /// DHT service for finding Harbor Nodes
    pub(super) dht_service: Option<Arc<DhtService>>,

    // === Harbor-specific config ===
    /// Local node's EndpointID
    pub(super) endpoint_id: [u8; 32],
    /// Rate limiter for abuse prevention (thread-safe via Mutex)
    pub(super) rate_limiter: Mutex<RateLimiter>,
    /// Proof of Work configuration
    pub(super) pow_config: PoWConfig,
    /// Storage quota manager
    pub(super) storage_manager: StorageManager,

    // === Timeout configuration ===
    /// Timeout for connecting to Harbor Nodes
    pub(super) connect_timeout: Duration,
    /// Timeout for Harbor Node responses
    pub(super) response_timeout: Duration,
}

impl HarborService {
    /// Create a fully-wired Harbor service (for production use)
    ///
    /// This is the primary constructor, matching the pattern of SendService::new().
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<TokioMutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        dht_service: Option<Arc<DhtService>>,
        pow_config: PoWConfig,
        storage_config: StorageConfig,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) -> Self {
        let endpoint_id = identity.public_key;
        Self {
            endpoint: Some(endpoint),
            identity: Some(identity),
            db: Some(db),
            event_tx: Some(event_tx),
            dht_service,
            endpoint_id,
            rate_limiter: Mutex::new(RateLimiter::with_config(RateLimitConfig::default())),
            pow_config,
            storage_manager: StorageManager::new(storage_config),
            connect_timeout,
            response_timeout,
        }
    }

    /// Create a standalone Harbor service with custom configs (for tests and incoming-only use)
    pub fn with_full_config(
        endpoint_id: [u8; 32],
        rate_config: RateLimitConfig,
        pow_config: PoWConfig,
        storage_config: StorageConfig,
    ) -> Self {
        Self {
            endpoint: None,
            identity: None,
            db: None,
            event_tx: None,
            dht_service: None,
            endpoint_id,
            rate_limiter: Mutex::new(RateLimiter::with_config(rate_config)),
            pow_config,
            storage_manager: StorageManager::new(storage_config),
            connect_timeout: Duration::from_secs(5),
            response_timeout: Duration::from_secs(30),
        }
    }

    /// Create a new Harbor service with custom rate limit and PoW configs
    pub fn with_config(
        endpoint_id: [u8; 32],
        rate_config: RateLimitConfig,
        pow_config: PoWConfig,
    ) -> Self {
        Self::with_full_config(endpoint_id, rate_config, pow_config, StorageConfig::default())
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

    // === Accessors ===

    /// Get our EndpointID
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.endpoint_id
    }

    /// Get the identity
    pub fn identity(&self) -> &LocalIdentity {
        self.identity.as_ref().expect("HarborService: identity not set (test-only instance?)")
    }

    /// Get the database
    pub fn db(&self) -> &Arc<TokioMutex<DbConnection>> {
        self.db.as_ref().expect("HarborService: db not set (test-only instance?)")
    }

    /// Get the event sender
    pub fn event_tx(&self) -> &mpsc::Sender<ProtocolEvent> {
        self.event_tx.as_ref().expect("HarborService: event_tx not set (test-only instance?)")
    }

    /// Get the endpoint
    pub fn endpoint(&self) -> &Endpoint {
        self.endpoint.as_ref().expect("HarborService: endpoint not set (test-only instance?)")
    }

    /// Get the DHT service
    pub fn dht_service(&self) -> &Option<Arc<DhtService>> {
        &self.dht_service
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

    /// Get connect timeout
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// Get response timeout
    pub fn response_timeout(&self) -> Duration {
        self.response_timeout
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

    // Tests moved from handlers/outgoing/harbor.rs

    #[test]
    fn test_harbor_connect_timeout_default_is_reasonable() {
        let config = crate::protocol::ProtocolConfig::default();
        assert!(config.harbor_connect_timeout_secs >= 3);
        assert!(config.harbor_connect_timeout_secs <= 30);
    }

    #[test]
    fn test_harbor_response_timeout_default_is_reasonable() {
        let config = crate::protocol::ProtocolConfig::default();
        assert!(config.harbor_response_timeout_secs >= 10);
        assert!(config.harbor_response_timeout_secs <= 120);
    }

    #[test]
    fn test_response_timeout_longer_than_connect() {
        let config = crate::protocol::ProtocolConfig::default();
        assert!(config.harbor_response_timeout_secs > config.harbor_connect_timeout_secs);
    }

    #[test]
    fn test_store_response_max_size() {
        assert!(HarborService::STORE_RESPONSE_MAX_SIZE <= 4096);
        assert!(HarborService::STORE_RESPONSE_MAX_SIZE >= 256);
    }

    #[test]
    fn test_pull_response_max_size() {
        assert!(HarborService::PULL_RESPONSE_MAX_SIZE >= 1024 * 1024);
        assert!(HarborService::PULL_RESPONSE_MAX_SIZE <= 100 * 1024 * 1024);
    }

    #[test]
    fn test_pull_response_larger_than_store() {
        assert!(HarborService::PULL_RESPONSE_MAX_SIZE > HarborService::STORE_RESPONSE_MAX_SIZE);
    }

    #[test]
    fn test_node_id_from_harbor_node() {
        let harbor_node = [42u8; 32];
        let result = iroh::EndpointId::from_bytes(&harbor_node);
        assert!(result.is_ok());
    }

    #[test]
    fn test_dht_id_from_harbor_id() {
        use crate::network::dht::Id as DhtId;
        let harbor_id = [123u8; 32];
        let target = DhtId::new(harbor_id);
        assert_eq!(*target.as_bytes(), harbor_id);
    }
}
