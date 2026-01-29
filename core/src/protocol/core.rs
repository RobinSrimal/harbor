//! Main Protocol implementation
//!
//! This is the core Protocol struct and initialization logic.
//! Implementation is split across:
//! - `protocol/` (this crate): Core struct, start/stop, public methods
//! - `handlers/`: Connection handling (incoming/outgoing)
//! - `tasks/`: Background automation

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use iroh::protocol::Router;
use rusqlite::Connection;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::info;

use crate::data::{get_or_create_identity, start_db, LocalIdentity};
use crate::network::dht::DhtService;
use crate::data::BlobStore;
use crate::network::harbor::HarborService;
use crate::network::stream::StreamService;
use crate::network::send::SendService;
use crate::network::share::{ShareConfig, ShareService};
use crate::network::sync::SyncService;
use crate::network::topic::TopicService;

use super::config::ProtocolConfig;
use super::error::ProtocolError;
use super::events::ProtocolEvent;
use super::stats::StatsService;

/// Maximum message size (512 KB)
pub const MAX_MESSAGE_SIZE: usize = 512 * 1024;

/// Retention period for Harbor packets (90 days)
///
/// How long packets are stored on Harbor Nodes before expiration.
#[allow(dead_code)]
pub const RETENTION_PERIOD: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// Retention period in seconds (for database queries)
#[allow(dead_code)]
pub const RETENTION_PERIOD_SECS: i64 = 90 * 24 * 60 * 60;

/// The Harbor Protocol
///
/// This is the main entry point for using the protocol.
pub struct Protocol {
    /// Configuration
    pub(crate) config: ProtocolConfig,
    /// Iroh endpoint
    pub(crate) endpoint: Endpoint,
    /// Local identity (shared with services)
    pub(crate) identity: Arc<LocalIdentity>,
    /// Database connection (wrapped for thread safety)
    pub(crate) db: Arc<Mutex<Connection>>,
    /// DHT service - single entry point for all DHT operations
    pub(crate) dht_service: Option<Arc<DhtService>>,
    /// Send service for message delivery
    pub(crate) send_service: Arc<SendService>,
    /// Harbor service for store-and-forward
    pub(crate) harbor_service: Arc<HarborService>,
    /// Share service for file distribution
    pub(crate) share_service: Arc<ShareService>,
    /// Sync service for CRDT synchronization
    pub(crate) sync_service: Arc<SyncService>,
    /// Stream service
    pub(crate) stream_service: Arc<StreamService>,
    /// Topic service for topic lifecycle
    pub(crate) topic_service: Arc<TopicService>,
    /// Stats service for monitoring
    pub(crate) stats_service: Arc<StatsService>,
    /// Event sender (for app notifications: messages, file events, etc.)
    pub(crate) event_tx: mpsc::Sender<ProtocolEvent>,
    /// Event receiver (cloneable via Arc)
    event_rx: Arc<RwLock<Option<mpsc::Receiver<ProtocolEvent>>>>,
    /// Running flag
    pub(crate) running: Arc<RwLock<bool>>,
    /// Background tasks
    pub(crate) tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
    /// Shutdown signal sender
    shutdown_tx: mpsc::Sender<()>,
    /// Iroh Router for ALPN-based connection dispatch
    router: Router,
}

impl Protocol {
    /// Start the protocol
    ///
    /// This initializes the database, creates an Iroh endpoint,
    /// and starts background tasks for DHT, Harbor sync, etc.
    pub async fn start(config: ProtocolConfig) -> Result<Self, ProtocolError> {
        // Initialize database
        let db_path = config
            .db_path
            .clone()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "harbor_protocol.db".to_string());

        // Generate or use provided key - convert to hex string for passphrase
        use zeroize::Zeroize;
        let mut db_key = config.db_key.unwrap_or_else(|| {
            // Generate a random key using rand
            use rand::{rngs::OsRng, Rng};
            let mut key = [0u8; 32];
            OsRng.fill(&mut key);
            key
        });
        let passphrase = hex::encode(&db_key);
        db_key.zeroize();

        let db = start_db(&db_path, &passphrase).map_err(|e| ProtocolError::Database(e.to_string()))?;

        // Get or create identity (wrapped in Arc for sharing with services)
        let identity = Arc::new(
            get_or_create_identity(&db).map_err(|e| ProtocolError::Database(e.to_string()))?
        );

        info!(
            endpoint_id = hex::encode(identity.public_key),
            created_at = identity.created_at,
            "Loaded identity from database"
        );

        // Create Iroh endpoint with our identity's secret key
        let secret_key = iroh::SecretKey::from_bytes(&identity.private_key);
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .bind()
            .await
            .map_err(|e| ProtocolError::StartFailed(format!("failed to create endpoint: {}", e)))?;

        info!(endpoint_id = %endpoint.id(), "Protocol started");

        // Wrap db in Arc<Mutex<>> for sharing with services
        let db = Arc::new(Mutex::new(db));

        // Initialize DHT
        let dht_service = DhtService::start(
            endpoint.clone(),
            &config.bootstrap_nodes,
            db.clone(),
            identity.public_key,
        ).await;

        // Create event channel
        let (event_tx, event_rx) = mpsc::channel(1000);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        let dht_service = Some(dht_service);

        // Initialize Send service
        let send_service = Arc::new(SendService::new(
            endpoint.clone(),
            identity.clone(),
            db.clone(),
            event_tx.clone(),
        ));

        // Initialize Harbor service
        let pow_config = if config.enable_pow {
            crate::resilience::PoWConfig {
                enabled: true,
                difficulty_bits: config.pow_difficulty,
                ..crate::resilience::PoWConfig::default()
            }
        } else {
            crate::resilience::PoWConfig::disabled()
        };
        let storage_config = crate::resilience::StorageConfig {
            max_total_bytes: config.max_storage_bytes,
            ..crate::resilience::StorageConfig::default()
        };
        let harbor_service = Arc::new(HarborService::new(
            endpoint.clone(),
            identity.clone(),
            db.clone(),
            event_tx.clone(),
            dht_service.clone(),
            pow_config,
            storage_config,
            Duration::from_secs(config.harbor_connect_timeout_secs),
            Duration::from_secs(config.harbor_response_timeout_secs),
        ));

        // Initialize Share service
        let blob_path = if let Some(ref path) = config.blob_path {
            path.clone()
        } else if let Some(ref db_path) = config.db_path {
            db_path.parent()
                .map(|p| p.join(".harbor_blobs"))
                .unwrap_or_else(|| PathBuf::from(".harbor_blobs"))
        } else {
            crate::data::default_blob_path().unwrap_or_else(|_| PathBuf::from(".harbor_blobs"))
        };
        let blob_store = Arc::new(BlobStore::new(&blob_path)
            .map_err(|e| ProtocolError::StartFailed(format!("failed to create blob store: {}", e)))?);

        let share_service = Arc::new(ShareService::new(
            endpoint.clone(),
            identity.clone(),
            db.clone(),
            blob_store,
            ShareConfig::default(),
            Some(send_service.clone()),
        ));

        // Initialize Sync service
        let sync_service = Arc::new(SyncService::new(
            endpoint.clone(),
            identity.clone(),
            db.clone(),
            event_tx.clone(),
            send_service.clone(),
        ));

        // Initialize Stream service
        let stream_service = StreamService::new(
            endpoint.clone(),
            identity.clone(),
            db.clone(),
            event_tx.clone(),
            send_service.clone(),
        );

        // Give SendService access to StreamService (for routing stream signaling)
        send_service.set_stream_service(stream_service.clone()).await;

        // Initialize Topic service
        let topic_service = Arc::new(TopicService::new(
            endpoint.clone(),
            identity.clone(),
            db.clone(),
            send_service.clone(),
        ));

        // Initialize Stats service
        let stats_service = Arc::new(StatsService::new(
            endpoint.clone(),
            identity.clone(),
            db.clone(),
            dht_service.clone(),
        ));

        // Build the iroh Router for ALPN-based connection dispatch
        let router = crate::handlers::build_router(
            endpoint.clone(),
            send_service.clone(),
            dht_service.clone(),
            harbor_service.clone(),
            share_service.clone(),
            sync_service.clone(),
            stream_service.clone(),
        );

        let protocol = Self {
            config,
            endpoint,
            identity,
            db,
            dht_service,
            send_service,
            harbor_service,
            share_service,
            sync_service,
            stream_service,
            topic_service,
            stats_service,
            event_tx,
            event_rx: Arc::new(RwLock::new(Some(event_rx))),
            running: Arc::new(RwLock::new(true)),
            tasks: Arc::new(RwLock::new(Vec::new())),
            shutdown_tx,
            router,
        };

        // Start background tasks
        protocol.start_background_tasks(shutdown_rx).await;

        Ok(protocol)
    }

    /// Stop the protocol
    pub async fn stop(&self) {
        info!("Stopping protocol...");

        // Signal shutdown
        {
            let mut running = self.running.write().await;
            *running = false;
        }

        // Send shutdown signal
        let _ = self.shutdown_tx.send(()).await;

        // Save DHT routing table before shutdown
        if let Some(ref dht_service) = self.dht_service {
            info!("Saving DHT routing table before shutdown...");
            if let Err(e) = dht_service.save_routing_table().await {
                tracing::warn!(error = %e, "Failed to save DHT routing table on shutdown");
            }
        }

        // Cancel background tasks
        {
            let mut tasks = self.tasks.write().await;
            for task in tasks.drain(..) {
                task.abort();
            }
        }

        // Shutdown the Router (closes endpoint and all protocol handlers)
        let _ = self.router.shutdown().await;

        info!("Protocol stopped");
    }

    /// Get the event receiver
    ///
    /// Get the protocol event receiver
    /// 
    /// This returns a receiver for all protocol events (messages, file events, etc.)
    /// Can only be called once - subsequent calls return None.
    pub async fn events(&self) -> Option<mpsc::Receiver<ProtocolEvent>> {
        let mut rx = self.event_rx.write().await;
        rx.take()
    }

    /// Get our EndpointID
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get the relay URL this node is connected to (if any)
    pub async fn relay_url(&self) -> Option<String> {
        let endpoint_addr = self.endpoint.addr();
        endpoint_addr.relay_urls().next().map(|url| url.to_string())
    }

    /// Get the blob storage path
    /// 
    /// Returns the configured blob path, or derives one from the db path,
    /// or falls back to the system default.
    pub fn blob_path(&self) -> PathBuf {
        if let Some(ref path) = self.config.blob_path {
            path.clone()
        } else if let Some(ref db_path) = self.config.db_path {
            // Derive from db_path: .harbor_blobs/ next to database
            db_path.parent()
                .map(|p| p.join(".harbor_blobs"))
                .unwrap_or_else(|| PathBuf::from(".harbor_blobs"))
        } else {
            // Fall back to system default
            crate::data::default_blob_path().unwrap_or_else(|_| PathBuf::from(".harbor_blobs"))
        }
    }

    /// Check if the protocol is running
    pub(crate) async fn check_running(&self) -> Result<(), ProtocolError> {
        let running = self.running.read().await;
        if !*running {
            return Err(ProtocolError::NotRunning);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_all_tables;
    use crate::data::{
        add_topic_member, get_all_topics, get_topic_members, remove_topic_member, subscribe_topic,
        unsubscribe_topic,
    };
    use crate::protocol::TopicInvite;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_all_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_protocol_config() {
        let config = ProtocolConfig::for_testing();
        assert!(!config.enable_pow);
    }

    #[test]
    fn test_max_message_size_constant() {
        assert_eq!(MAX_MESSAGE_SIZE, 512 * 1024);
    }

    // ========== Topic Operations (DB-only tests) ==========

    #[test]
    fn test_create_topic_db_operations() {
        let conn = setup_test_db();
        let topic_id = test_id(1);
        let member_id = test_id(10);

        // Subscribe to topic
        subscribe_topic(&conn, &topic_id).unwrap();

        // Add member
        add_topic_member(&conn, &topic_id, &member_id).unwrap();

        // Verify
        let members = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0], member_id);
    }

    #[test]
    fn test_join_topic_db_operations() {
        let conn = setup_test_db();
        let topic_id = test_id(1);
        let existing_member = test_id(10);
        let new_member = test_id(11);

        // Simulate existing topic
        subscribe_topic(&conn, &topic_id).unwrap();
        add_topic_member(&conn, &topic_id, &existing_member).unwrap();

        // Join operation: add new member + existing members
        add_topic_member(&conn, &topic_id, &new_member).unwrap();

        // Verify
        let members = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(members.len(), 2);
        assert!(members.contains(&existing_member));
        assert!(members.contains(&new_member));
    }

    #[test]
    fn test_leave_topic_db_operations() {
        let conn = setup_test_db();
        let topic_id = test_id(1);
        let member1 = test_id(10);
        let member2 = test_id(11);

        // Setup topic with two members
        subscribe_topic(&conn, &topic_id).unwrap();
        add_topic_member(&conn, &topic_id, &member1).unwrap();
        add_topic_member(&conn, &topic_id, &member2).unwrap();

        // Member1 leaves (removes self from members)
        remove_topic_member(&conn, &topic_id, &member1).unwrap();

        // Verify member1 is removed but member2 remains
        let members = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(members.len(), 1);
        assert!(!members.contains(&member1));
        assert!(members.contains(&member2));

        // Now fully unsubscribe - this cascades and removes all members
        unsubscribe_topic(&conn, &topic_id).unwrap();

        // After unsubscribe, topic is gone (members cascade deleted)
        let members_after = get_topic_members(&conn, &topic_id).unwrap();
        assert!(members_after.is_empty());
    }

    #[test]
    fn test_list_topics_db_operations() {
        let conn = setup_test_db();

        // Create multiple topics
        for i in 0..5 {
            let topic_id = test_id(i);
            subscribe_topic(&conn, &topic_id).unwrap();
        }

        let topics = get_all_topics(&conn).unwrap();
        assert_eq!(topics.len(), 5);
    }

    #[test]
    fn test_get_invite_db_operations() {
        let conn = setup_test_db();
        let topic_id = test_id(1);
        let members = vec![test_id(10), test_id(11), test_id(12)];

        // Setup topic
        subscribe_topic(&conn, &topic_id).unwrap();
        for member in &members {
            add_topic_member(&conn, &topic_id, member).unwrap();
        }

        // Get members (simulates get_invite)
        let retrieved = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(retrieved.len(), 3);
    }

    // ========== Error Condition Tests ==========

    #[test]
    fn test_topic_not_found_condition() {
        let conn = setup_test_db();
        let nonexistent = test_id(99);

        // Getting members for non-existent topic should return empty
        let members = get_topic_members(&conn, &nonexistent).unwrap();
        assert!(members.is_empty());
    }

    #[test]
    fn test_message_size_validation() {
        // Test that we have the right constant for validation
        let small_msg = vec![0u8; 1000];
        let max_msg = vec![0u8; MAX_MESSAGE_SIZE];
        let too_large = vec![0u8; MAX_MESSAGE_SIZE + 1];

        assert!(small_msg.len() <= MAX_MESSAGE_SIZE);
        assert!(max_msg.len() <= MAX_MESSAGE_SIZE);
        assert!(too_large.len() > MAX_MESSAGE_SIZE);
    }

    // ========== TopicInvite Integration ==========

    #[test]
    fn test_topic_invite_from_db() {
        let conn = setup_test_db();
        let topic_id = test_id(42);
        let members = vec![test_id(1), test_id(2)];

        // Setup
        subscribe_topic(&conn, &topic_id).unwrap();
        for m in &members {
            add_topic_member(&conn, &topic_id, m).unwrap();
        }

        // Create invite from DB state
        let db_members = get_topic_members(&conn, &topic_id).unwrap();
        let invite = TopicInvite::new(topic_id, db_members);

        // Serialize and restore
        let hex = invite.to_hex().unwrap();
        let restored = TopicInvite::from_hex(&hex).unwrap();

        assert_eq!(restored.topic_id, topic_id);
        assert_eq!(restored.members.len(), 2);
    }
}
