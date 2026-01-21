//! Protocol configuration

use std::fmt;
use std::path::PathBuf;

/// Configuration for the Harbor protocol
#[derive(Clone)]
pub struct ProtocolConfig {
    /// Path to the database file
    /// If None, uses a default path in the user's data directory
    pub db_path: Option<PathBuf>,
    
    /// Path to blob storage directory
    /// If None, uses .harbor_blobs/ next to the database
    pub blob_path: Option<PathBuf>,
    
    /// Database encryption key (32 bytes)
    /// If None, generates a random key (not recommended for production)
    pub db_key: Option<[u8; 32]>,
    
    /// Bootstrap nodes for DHT (EndpointIDs as hex strings)
    pub bootstrap_nodes: Vec<String>,
    
    /// Maximum storage for Harbor cache (bytes)
    /// Default: 10GB
    pub max_storage_bytes: u64,
    
    /// How often to sync with other Harbor Nodes (seconds)
    /// This is Harbor-to-Harbor sync for redundancy.
    /// Default: 300 (5 minutes)
    pub harbor_sync_interval_secs: u64,
    
    /// Number of closest Harbor Nodes to consider for sync
    /// Default: 5
    pub harbor_sync_candidates: usize,
    
    /// How often to pull from Harbor Nodes (seconds)
    /// Default: 60 (1 minute)
    pub harbor_pull_interval_secs: u64,
    
    /// How often to check for packets needing replication (seconds)
    /// Default: 30
    pub replication_check_interval_secs: u64,
    
    /// Number of Harbor Nodes to replicate each packet to
    /// Default: 3
    pub replication_factor: usize,
    
    /// Maximum number of Harbor Nodes to try when replicating
    /// Default: 6 (replication_factor * 2)
    pub max_replication_attempts: usize,
    
    /// Maximum number of Harbor Nodes to pull from
    /// Default: 5
    pub harbor_pull_max_nodes: usize,
    
    /// Stop pulling early after this many consecutive empty responses
    /// Default: 2
    pub harbor_pull_early_stop: usize,
    
    /// Timeout for connecting to Harbor Nodes (seconds)
    /// Default: 5
    pub harbor_connect_timeout_secs: u64,
    
    /// Timeout for Harbor Node responses (seconds)
    /// Default: 30
    pub harbor_response_timeout_secs: u64,
    
    /// Delay before DHT bootstrap (seconds)
    /// Default: 5
    pub dht_bootstrap_delay_secs: u64,
    
    /// DHT refresh interval during initial phase (seconds)
    /// Default: 10
    pub dht_initial_refresh_interval_secs: u64,
    
    /// DHT refresh interval once stable (seconds)
    /// Default: 120
    pub dht_stable_refresh_interval_secs: u64,
    
    /// DHT routing table save interval (seconds)
    /// Default: 60
    pub dht_save_interval_secs: u64,
    
    /// Cleanup interval for expired data (seconds)
    /// Default: 3600 (1 hour)
    pub cleanup_interval_secs: u64,
    
    /// Enable proof of work for Harbor stores
    /// Default: true
    pub enable_pow: bool,
    
    /// PoW difficulty (leading zero bits)
    /// Default: 18 (~10-50ms)
    pub pow_difficulty: u8,
    
    /// How often to check for incomplete blobs and retry pulling (seconds)
    /// Default: 30
    pub share_pull_interval_secs: u64,
}

impl fmt::Debug for ProtocolConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtocolConfig")
            .field("db_path", &self.db_path)
            .field("blob_path", &self.blob_path)
            .field("db_key", &self.db_key.as_ref().map(|_| "[REDACTED]"))
            .field("bootstrap_nodes", &self.bootstrap_nodes)
            .field("max_storage_bytes", &self.max_storage_bytes)
            .field("harbor_sync_interval_secs", &self.harbor_sync_interval_secs)
            .field("harbor_sync_candidates", &self.harbor_sync_candidates)
            .field("harbor_pull_interval_secs", &self.harbor_pull_interval_secs)
            .field("replication_check_interval_secs", &self.replication_check_interval_secs)
            .field("replication_factor", &self.replication_factor)
            .field("max_replication_attempts", &self.max_replication_attempts)
            .field("harbor_pull_max_nodes", &self.harbor_pull_max_nodes)
            .field("harbor_pull_early_stop", &self.harbor_pull_early_stop)
            .field("harbor_connect_timeout_secs", &self.harbor_connect_timeout_secs)
            .field("harbor_response_timeout_secs", &self.harbor_response_timeout_secs)
            .field("dht_bootstrap_delay_secs", &self.dht_bootstrap_delay_secs)
            .field("dht_initial_refresh_interval_secs", &self.dht_initial_refresh_interval_secs)
            .field("dht_stable_refresh_interval_secs", &self.dht_stable_refresh_interval_secs)
            .field("dht_save_interval_secs", &self.dht_save_interval_secs)
            .field("cleanup_interval_secs", &self.cleanup_interval_secs)
            .field("enable_pow", &self.enable_pow)
            .field("pow_difficulty", &self.pow_difficulty)
            .field("share_pull_interval_secs", &self.share_pull_interval_secs)
            .finish()
    }
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            db_path: None,
            blob_path: None,
            db_key: None,
            bootstrap_nodes: vec![
                // Default bootstrap node (can be overridden)
                "6bcf661fb9f503d534f732686163c3fd739f7aa10354966a06177cb5a7a6d103".to_string(),
            ],
            max_storage_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
            harbor_sync_interval_secs: 300, // 5 minutes
            harbor_sync_candidates: 5,
            harbor_pull_interval_secs: 60,
            replication_check_interval_secs: 30,
            replication_factor: 3,
            max_replication_attempts: 6,
            harbor_pull_max_nodes: 5,
            harbor_pull_early_stop: 2,
            harbor_connect_timeout_secs: 5,
            harbor_response_timeout_secs: 30,
            dht_bootstrap_delay_secs: 5,
            dht_initial_refresh_interval_secs: 10,
            dht_stable_refresh_interval_secs: 120,
            dht_save_interval_secs: 60,
            cleanup_interval_secs: 3600,
            enable_pow: true,
            pow_difficulty: 18,
            share_pull_interval_secs: 30,
        }
    }
}

impl ProtocolConfig {
    /// Create a new config with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the database path
    pub fn with_db_path(mut self, path: PathBuf) -> Self {
        self.db_path = Some(path);
        self
    }

    /// Set the blob storage path
    pub fn with_blob_path(mut self, path: PathBuf) -> Self {
        self.blob_path = Some(path);
        self
    }

    /// Set the database encryption key
    pub fn with_db_key(mut self, key: [u8; 32]) -> Self {
        self.db_key = Some(key);
        self
    }

    /// Add a bootstrap node
    pub fn with_bootstrap_node(mut self, node_id: String) -> Self {
        self.bootstrap_nodes.push(node_id);
        self
    }

    /// Set bootstrap nodes (replaces existing)
    pub fn with_bootstrap_nodes(mut self, nodes: Vec<String>) -> Self {
        self.bootstrap_nodes = nodes;
        self
    }

    /// Set maximum storage
    pub fn with_max_storage(mut self, bytes: u64) -> Self {
        self.max_storage_bytes = bytes;
        self
    }

    /// Disable proof of work (not recommended)
    pub fn without_pow(mut self) -> Self {
        self.enable_pow = false;
        self
    }

    /// Set PoW difficulty (leading zero bits)
    pub fn with_pow_difficulty(mut self, difficulty: u8) -> Self {
        self.pow_difficulty = difficulty;
        self
    }

    /// Set Harbor-to-Harbor sync interval
    pub fn with_harbor_sync_interval(mut self, secs: u64) -> Self {
        self.harbor_sync_interval_secs = secs;
        self
    }

    /// Set number of Harbor sync candidates
    pub fn with_harbor_sync_candidates(mut self, count: usize) -> Self {
        self.harbor_sync_candidates = count;
        self
    }

    /// Set Harbor pull interval
    pub fn with_harbor_pull_interval(mut self, secs: u64) -> Self {
        self.harbor_pull_interval_secs = secs;
        self
    }

    /// Configuration for testing (smaller limits, no PoW, higher replication)
    pub fn for_testing() -> Self {
        Self {
            db_path: None,
            blob_path: None,
            db_key: None,
            bootstrap_nodes: vec![],
            max_storage_bytes: 10 * 1024 * 1024, // 10 MB
            harbor_sync_interval_secs: 5,    // Faster for testing
            harbor_sync_candidates: 8,       // Store on more nodes
            harbor_pull_interval_secs: 3,    // Faster pulls
            replication_check_interval_secs: 5,
            replication_factor: 5,           // Higher for testing reliability
            max_replication_attempts: 10,    // Try more nodes
            harbor_pull_max_nodes: 12,       // Pull from more nodes
            harbor_pull_early_stop: 10,      // Much more thorough
            harbor_connect_timeout_secs: 5,  // Same as default
            harbor_response_timeout_secs: 30, // Same as default
            dht_bootstrap_delay_secs: 5,     // Same as default
            dht_initial_refresh_interval_secs: 10, // Same as default
            dht_stable_refresh_interval_secs: 120, // Same as default
            dht_save_interval_secs: 60,      // Same as default
            cleanup_interval_secs: 3600,     // Same as default
            enable_pow: false,
            pow_difficulty: 8,
            share_pull_interval_secs: 10,    // Faster for testing
        }
    }
    
    /// Set replication check interval
    pub fn with_replication_check_interval(mut self, secs: u64) -> Self {
        self.replication_check_interval_secs = secs;
        self
    }
    
    /// Set replication factor (number of Harbor nodes to replicate to)
    pub fn with_replication_factor(mut self, factor: usize) -> Self {
        self.replication_factor = factor;
        self
    }
    
    /// Set max replication attempts (max nodes to try when replicating)
    pub fn with_max_replication_attempts(mut self, attempts: usize) -> Self {
        self.max_replication_attempts = attempts;
        self
    }
    
    /// Set max Harbor nodes to pull from
    pub fn with_harbor_pull_max_nodes(mut self, count: usize) -> Self {
        self.harbor_pull_max_nodes = count;
        self
    }
    
    /// Set early stop threshold for Harbor pull
    pub fn with_harbor_pull_early_stop(mut self, count: usize) -> Self {
        self.harbor_pull_early_stop = count;
        self
    }
    
    /// Set Harbor connect timeout
    pub fn with_harbor_connect_timeout(mut self, secs: u64) -> Self {
        self.harbor_connect_timeout_secs = secs;
        self
    }
    
    /// Set Harbor response timeout
    pub fn with_harbor_response_timeout(mut self, secs: u64) -> Self {
        self.harbor_response_timeout_secs = secs;
        self
    }
    
    /// Set DHT bootstrap delay
    pub fn with_dht_bootstrap_delay(mut self, secs: u64) -> Self {
        self.dht_bootstrap_delay_secs = secs;
        self
    }
    
    /// Set DHT initial refresh interval
    pub fn with_dht_initial_refresh_interval(mut self, secs: u64) -> Self {
        self.dht_initial_refresh_interval_secs = secs;
        self
    }
    
    /// Set DHT stable refresh interval
    pub fn with_dht_stable_refresh_interval(mut self, secs: u64) -> Self {
        self.dht_stable_refresh_interval_secs = secs;
        self
    }
    
    /// Set DHT save interval
    pub fn with_dht_save_interval(mut self, secs: u64) -> Self {
        self.dht_save_interval_secs = secs;
        self
    }
    
    /// Set cleanup interval
    pub fn with_cleanup_interval(mut self, secs: u64) -> Self {
        self.cleanup_interval_secs = secs;
        self
    }
    
    /// Set share pull retry interval
    pub fn with_share_pull_interval(mut self, secs: u64) -> Self {
        self.share_pull_interval_secs = secs;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_default_config() {
        let config = ProtocolConfig::default();
        assert_eq!(config.max_storage_bytes, 10 * 1024 * 1024 * 1024);
        assert!(config.enable_pow);
        assert!(!config.bootstrap_nodes.is_empty());
        assert_eq!(config.pow_difficulty, 18);
        assert_eq!(config.harbor_sync_interval_secs, 300);
        assert_eq!(config.harbor_sync_candidates, 5);
        assert_eq!(config.harbor_pull_interval_secs, 60);
        assert_eq!(config.replication_check_interval_secs, 30);
        assert_eq!(config.replication_factor, 3);
        assert_eq!(config.max_replication_attempts, 6);
        assert_eq!(config.harbor_pull_max_nodes, 5);
        assert_eq!(config.harbor_pull_early_stop, 2);
    }

    #[test]
    fn test_new_equals_default() {
        let config1 = ProtocolConfig::new();
        let config2 = ProtocolConfig::default();
        
        assert_eq!(config1.max_storage_bytes, config2.max_storage_bytes);
        assert_eq!(config1.enable_pow, config2.enable_pow);
    }

    #[test]
    fn test_builder_pattern() {
        let config = ProtocolConfig::new()
            .with_max_storage(1024)
            .without_pow()
            .with_bootstrap_node("abc123".to_string());

        assert_eq!(config.max_storage_bytes, 1024);
        assert!(!config.enable_pow);
        assert!(config.bootstrap_nodes.contains(&"abc123".to_string()));
    }

    #[test]
    fn test_with_db_path() {
        let path = PathBuf::from("/tmp/test.db");
        let config = ProtocolConfig::new().with_db_path(path.clone());
        
        assert_eq!(config.db_path, Some(path));
    }

    #[test]
    fn test_with_db_key() {
        let key = [42u8; 32];
        let config = ProtocolConfig::new().with_db_key(key);
        
        assert_eq!(config.db_key, Some(key));
    }

    #[test]
    fn test_with_bootstrap_nodes_replaces() {
        let config = ProtocolConfig::new()
            .with_bootstrap_node("first".to_string())
            .with_bootstrap_nodes(vec!["a".to_string(), "b".to_string()]);
        
        // with_bootstrap_nodes replaces, so "first" should be gone
        assert_eq!(config.bootstrap_nodes.len(), 2);
        assert!(!config.bootstrap_nodes.contains(&"first".to_string()));
        assert!(config.bootstrap_nodes.contains(&"a".to_string()));
    }

    #[test]
    fn test_with_bootstrap_node_adds() {
        let config = ProtocolConfig::new()
            .with_bootstrap_node("extra".to_string());
        
        // Should have default + extra
        assert!(config.bootstrap_nodes.len() >= 2);
        assert!(config.bootstrap_nodes.contains(&"extra".to_string()));
    }

    #[test]
    fn test_testing_config() {
        let config = ProtocolConfig::for_testing();
        assert!(!config.enable_pow);
        assert!(config.bootstrap_nodes.is_empty());
        assert_eq!(config.max_storage_bytes, 10 * 1024 * 1024); // 10 MB
        assert_eq!(config.harbor_sync_interval_secs, 5);
        assert_eq!(config.harbor_sync_candidates, 8);
        assert_eq!(config.harbor_pull_interval_secs, 3);
        assert_eq!(config.replication_check_interval_secs, 5);
        // Testing config has higher replication for reliability
        assert_eq!(config.replication_factor, 5);
        assert_eq!(config.max_replication_attempts, 10);
        assert_eq!(config.harbor_pull_max_nodes, 12);
        assert_eq!(config.harbor_pull_early_stop, 10);
    }

    #[test]
    fn test_builder_chain() {
        let config = ProtocolConfig::new()
            .with_db_path(PathBuf::from("/test"))
            .with_db_key([1u8; 32])
            .with_max_storage(5000)
            .without_pow()
            .with_bootstrap_nodes(vec!["node1".to_string()]);

        assert_eq!(config.db_path, Some(PathBuf::from("/test")));
        assert_eq!(config.db_key, Some([1u8; 32]));
        assert_eq!(config.max_storage_bytes, 5000);
        assert!(!config.enable_pow);
        assert_eq!(config.bootstrap_nodes, vec!["node1".to_string()]);
    }

    #[test]
    fn test_config_clone() {
        let config1 = ProtocolConfig::new()
            .with_max_storage(1234)
            .with_bootstrap_node("test".to_string());
        
        let config2 = config1.clone();
        
        assert_eq!(config1.max_storage_bytes, config2.max_storage_bytes);
        assert_eq!(config1.bootstrap_nodes, config2.bootstrap_nodes);
    }

    #[test]
    fn test_debug_redacts_db_key() {
        let config = ProtocolConfig::new().with_db_key([0xABu8; 32]);
        let debug_output = format!("{:?}", config);
        
        // Should contain REDACTED, not the actual key bytes
        assert!(debug_output.contains("[REDACTED]"));
        assert!(!debug_output.contains("171")); // 0xAB = 171, should not appear
        assert!(!debug_output.contains("0xab"));
        assert!(!debug_output.contains("0xAB"));
    }

    #[test]
    fn test_debug_shows_none_for_missing_key() {
        let config = ProtocolConfig::new();
        let debug_output = format!("{:?}", config);
        
        // When db_key is None, should show None not REDACTED
        assert!(debug_output.contains("db_key: None"));
    }

    #[test]
    fn test_with_pow_difficulty() {
        let config = ProtocolConfig::new().with_pow_difficulty(24);
        assert_eq!(config.pow_difficulty, 24);
    }

    #[test]
    fn test_with_harbor_sync_interval() {
        let config = ProtocolConfig::new().with_harbor_sync_interval(600);
        assert_eq!(config.harbor_sync_interval_secs, 600);
    }

    #[test]
    fn test_with_harbor_sync_candidates() {
        let config = ProtocolConfig::new().with_harbor_sync_candidates(10);
        assert_eq!(config.harbor_sync_candidates, 10);
    }

    #[test]
    fn test_with_harbor_pull_interval() {
        let config = ProtocolConfig::new().with_harbor_pull_interval(120);
        assert_eq!(config.harbor_pull_interval_secs, 120);
    }

    #[test]
    fn test_full_builder_chain() {
        let config = ProtocolConfig::new()
            .with_db_path(PathBuf::from("/custom/path"))
            .with_db_key([99u8; 32])
            .with_bootstrap_nodes(vec!["node1".to_string(), "node2".to_string()])
            .with_max_storage(1_000_000)
            .with_pow_difficulty(20)
            .with_harbor_sync_interval(600)
            .with_harbor_sync_candidates(8)
            .with_harbor_pull_interval(30);

        assert_eq!(config.db_path, Some(PathBuf::from("/custom/path")));
        assert_eq!(config.db_key, Some([99u8; 32]));
        assert_eq!(config.bootstrap_nodes.len(), 2);
        assert_eq!(config.max_storage_bytes, 1_000_000);
        assert!(config.enable_pow);
        assert_eq!(config.pow_difficulty, 20);
        assert_eq!(config.harbor_sync_interval_secs, 600);
        assert_eq!(config.harbor_sync_candidates, 8);
        assert_eq!(config.harbor_pull_interval_secs, 30);
    }
}

