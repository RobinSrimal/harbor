//! Storage management for Harbor protocol
//!
//! Manages:
//! - Total storage quotas (default 10GB)
//! - Per-HarborID quotas
//! - Eviction policy for when quotas are exceeded
//!
//! # Eviction Policy
//!
//! When storage limits are exceeded, packets are evicted in this order:
//! 1. Fully-delivered packets (all recipients marked delivered), oldest first
//! 2. Partially-delivered packets, oldest first
//!
//! This prioritizes keeping undelivered messages in cache.

use rusqlite::Connection;
use std::fmt;

/// Parse a 32-byte ID from a database row, returning None if invalid length
fn parse_id(id_vec: &[u8]) -> Option<[u8; 32]> {
    if id_vec.len() != 32 {
        return None;
    }
    let mut id = [0u8; 32];
    id.copy_from_slice(id_vec);
    Some(id)
}

/// Storage configuration
#[derive(Debug, Clone, Copy)]
pub struct StorageConfig {
    /// Maximum total storage in bytes (default 10GB)
    pub max_total_bytes: u64,
    /// Maximum storage per HarborID in bytes (default 100MB)
    pub max_per_harbor_bytes: u64,
    /// Whether storage limits are enforced
    pub enabled: bool,
}

impl StorageConfig {
    /// Minimum storage allowed for any device (10 GB)
    pub const MIN_STORAGE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

    /// Default per-harbor ratio (1% of total)
    const PER_HARBOR_RATIO: u64 = 100;

    /// Mobile configuration (10GB total, fixed - not user configurable)
    ///
    /// Mobile devices have stricter storage constraints, so this is
    /// intentionally not configurable by the user.
    pub fn mobile() -> Self {
        Self {
            max_total_bytes: Self::MIN_STORAGE_BYTES,     // 10 GB
            max_per_harbor_bytes: 100 * 1024 * 1024,      // 100 MB per topic
            enabled: true,
        }
    }

    /// Desktop configuration (50GB default, user configurable)
    pub fn desktop() -> Self {
        Self {
            max_total_bytes: 50 * 1024 * 1024 * 1024,     // 50 GB
            max_per_harbor_bytes: 500 * 1024 * 1024,      // 500 MB per topic
            enabled: true,
        }
    }

    /// Desktop configuration with user-specified size
    ///
    /// Enforces minimum of 10GB. Per-harbor limit scales proportionally.
    pub fn desktop_with_size(user_bytes: u64) -> Self {
        let total = user_bytes.max(Self::MIN_STORAGE_BYTES);
        Self {
            max_total_bytes: total,
            max_per_harbor_bytes: total / Self::PER_HARBOR_RATIO,
            enabled: true,
        }
    }

    /// Platform-appropriate default configuration
    ///
    /// Uses compile-time detection:
    /// - iOS/Android → mobile (10GB fixed)
    /// - Everything else → desktop (50GB default)
    pub fn platform_default() -> Self {
        #[cfg(any(target_os = "ios", target_os = "android"))]
        {
            Self::mobile()
        }
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        {
            Self::desktop()
        }
    }

    /// Testing configuration (small limits)
    pub fn for_testing() -> Self {
        Self {
            max_total_bytes: 10 * 1024 * 1024,            // 10 MB
            max_per_harbor_bytes: 1024 * 1024,             // 1 MB per topic
            enabled: true,
        }
    }

    /// Disabled configuration (for testing or special cases)
    pub fn disabled() -> Self {
        Self {
            max_total_bytes: u64::MAX,
            max_per_harbor_bytes: u64::MAX,
            enabled: false,
        }
    }

    /// Check if this is a valid configuration
    pub fn is_valid(&self) -> bool {
        !self.enabled || self.max_total_bytes >= Self::MIN_STORAGE_BYTES
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self::platform_default()
    }
}

/// Current storage statistics
#[derive(Debug, Clone, Default)]
pub struct StorageStats {
    /// Total bytes stored in harbor cache
    pub total_bytes: u64,
    /// Number of packets stored
    pub packet_count: u64,
    /// Number of unique HarborIDs
    pub harbor_count: u64,
    /// Bytes per HarborID (top consumers)
    pub per_harbor: Vec<([u8; 32], u64)>,
}

/// Storage manager for Harbor cache
pub struct StorageManager {
    config: StorageConfig,
}

impl StorageManager {
    /// Create a new storage manager
    pub fn new(config: StorageConfig) -> Self {
        Self { config }
    }

    /// Get current storage statistics
    pub fn get_stats(&self, conn: &Connection) -> Result<StorageStats, rusqlite::Error> {
        // Total bytes and packet count
        let (total_bytes, packet_count): (i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(packet_data)), 0), COUNT(*) 
             FROM harbor_cache",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        // Harbor count
        let harbor_count: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT harbor_id) FROM harbor_cache",
            [],
            |row| row.get(0),
        )?;

        // Per-harbor breakdown (top 10)
        let mut stmt = conn.prepare(
            "SELECT harbor_id, SUM(LENGTH(packet_data)) as bytes
             FROM harbor_cache
             GROUP BY harbor_id
             ORDER BY bytes DESC
             LIMIT 10"
        )?;

        let per_harbor: Vec<([u8; 32], u64)> = stmt
            .query_map([], |row| {
                let harbor_id: Vec<u8> = row.get(0)?;
                let bytes: i64 = row.get(1)?;
                Ok((harbor_id, bytes as u64))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(id_vec, bytes)| {
                parse_id(&id_vec).map(|id| (id, bytes))
            })
            .collect();

        Ok(StorageStats {
            total_bytes: total_bytes as u64,
            packet_count: packet_count as u64,
            harbor_count: harbor_count as u64,
            per_harbor,
        })
    }

    /// Check if storing a packet would exceed limits
    pub fn can_store(
        &self,
        conn: &Connection,
        harbor_id: &[u8; 32],
        packet_size: u64,
    ) -> Result<StorageCheckResult, rusqlite::Error> {
        if !self.config.enabled {
            return Ok(StorageCheckResult::Allowed);
        }

        let stats = self.get_stats(conn)?;

        // Check total limit
        if stats.total_bytes + packet_size > self.config.max_total_bytes {
            return Ok(StorageCheckResult::TotalLimitExceeded {
                current: stats.total_bytes,
                limit: self.config.max_total_bytes,
            });
        }

        // Check per-harbor limit
        let harbor_bytes = self.get_harbor_bytes(conn, harbor_id)?;
        if harbor_bytes + packet_size > self.config.max_per_harbor_bytes {
            return Ok(StorageCheckResult::HarborLimitExceeded {
                harbor_id: *harbor_id,
                current: harbor_bytes,
                limit: self.config.max_per_harbor_bytes,
            });
        }

        Ok(StorageCheckResult::Allowed)
    }

    /// Get bytes stored for a specific HarborID
    fn get_harbor_bytes(
        &self,
        conn: &Connection,
        harbor_id: &[u8; 32],
    ) -> Result<u64, rusqlite::Error> {
        let bytes: i64 = conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(packet_data)), 0) 
             FROM harbor_cache 
             WHERE harbor_id = ?1",
            [harbor_id.as_slice()],
            |row| row.get(0),
        )?;
        Ok(bytes as u64)
    }

    /// Evict packets to make room for new data
    /// Returns the number of packets evicted
    pub fn evict_if_needed(
        &self,
        conn: &mut Connection,
        needed_bytes: u64,
    ) -> Result<usize, rusqlite::Error> {
        if !self.config.enabled {
            return Ok(0);
        }

        let stats = self.get_stats(conn)?;
        
        // Calculate how much we need to free
        let target = self.config.max_total_bytes.saturating_sub(needed_bytes);
        if stats.total_bytes <= target {
            return Ok(0);
        }

        let to_free = stats.total_bytes - target;
        self.evict_bytes(conn, to_free)
    }

    /// Evict packets to free up the specified number of bytes
    /// Eviction policy: oldest packets first, prioritize fully-delivered packets
    fn evict_bytes(
        &self,
        conn: &mut Connection,
        bytes_to_free: u64,
    ) -> Result<usize, rusqlite::Error> {
        // Collect packets to evict before starting transaction
        let mut to_evict: Vec<([u8; 32], u64)> = Vec::new();
        let mut freed = 0u64;

        // First, try fully-delivered packets (oldest first)
        {
            let mut stmt = conn.prepare(
                "SELECT c.packet_id, LENGTH(c.packet_data) as size
                 FROM harbor_cache c
                 WHERE NOT EXISTS (
                     SELECT 1 FROM harbor_recipients r 
                     WHERE r.packet_id = c.packet_id AND r.delivered = 0
                 )
                 ORDER BY c.created_at ASC"
            )?;

            let packets: Vec<([u8; 32], u64)> = stmt
                .query_map([], |row| {
                    let id: Vec<u8> = row.get(0)?;
                    let size: i64 = row.get(1)?;
                    Ok((id, size as u64))
                })?
                .filter_map(|r| r.ok())
                .filter_map(|(id_vec, size)| {
                    parse_id(&id_vec).map(|id| (id, size))
                })
                .collect();

            for (packet_id, size) in packets {
                if freed >= bytes_to_free {
                    break;
                }
                to_evict.push((packet_id, size));
                freed += size;
            }
        }

        // If still need more space, add oldest packets with remaining recipients
        if freed < bytes_to_free {
            let mut stmt = conn.prepare(
                "SELECT packet_id, LENGTH(packet_data) as size
                 FROM harbor_cache
                 ORDER BY created_at ASC"
            )?;

            let packets: Vec<([u8; 32], u64)> = stmt
                .query_map([], |row| {
                    let id: Vec<u8> = row.get(0)?;
                    let size: i64 = row.get(1)?;
                    Ok((id, size as u64))
                })?
                .filter_map(|r| r.ok())
                .filter_map(|(id_vec, size)| {
                    parse_id(&id_vec).map(|id| (id, size))
                })
                .collect();

            for (packet_id, size) in packets {
                if freed >= bytes_to_free {
                    break;
                }
                // Skip if already marked for eviction
                if !to_evict.iter().any(|(id, _)| id == &packet_id) {
                    to_evict.push((packet_id, size));
                    freed += size;
                }
            }
        }

        // Perform deletions in a transaction
        let tx = conn.transaction()?;
        for (packet_id, _) in &to_evict {
            // ON DELETE CASCADE handles harbor_recipients
            tx.execute(
                "DELETE FROM harbor_cache WHERE packet_id = ?1",
                [packet_id.as_slice()],
            )?;
        }
        tx.commit()?;

        Ok(to_evict.len())
    }

    /// Evict packets for a specific HarborID that exceeds its quota
    pub fn evict_for_harbor(
        &self,
        conn: &mut Connection,
        harbor_id: &[u8; 32],
        needed_bytes: u64,
    ) -> Result<usize, rusqlite::Error> {
        if !self.config.enabled {
            return Ok(0);
        }

        let current = self.get_harbor_bytes(conn, harbor_id)?;
        let target = self.config.max_per_harbor_bytes.saturating_sub(needed_bytes);
        
        if current <= target {
            return Ok(0);
        }

        let to_free = current - target;

        // Collect packets to evict
        let to_evict: Vec<[u8; 32]>;
        {
            let mut stmt = conn.prepare(
                "SELECT packet_id, LENGTH(packet_data) as size
                 FROM harbor_cache
                 WHERE harbor_id = ?1
                 ORDER BY created_at ASC"
            )?;

            let packets: Vec<([u8; 32], u64)> = stmt
                .query_map([harbor_id.as_slice()], |row| {
                    let id: Vec<u8> = row.get(0)?;
                    let size: i64 = row.get(1)?;
                    Ok((id, size as u64))
                })?
                .filter_map(|r| r.ok())
                .filter_map(|(id_vec, size)| {
                    parse_id(&id_vec).map(|id| (id, size))
                })
                .collect();

            let mut freed = 0u64;
            to_evict = packets
                .into_iter()
                .take_while(|(_, size)| {
                    if freed >= to_free {
                        return false;
                    }
                    freed += size;
                    true
                })
                .map(|(id, _)| id)
                .collect();
        }

        // Perform deletions in a transaction
        let tx = conn.transaction()?;
        for packet_id in &to_evict {
            // ON DELETE CASCADE handles harbor_recipients
            tx.execute(
                "DELETE FROM harbor_cache WHERE packet_id = ?1",
                [packet_id.as_slice()],
            )?;
        }
        tx.commit()?;

        Ok(to_evict.len())
    }

    /// Get the configuration
    pub fn config(&self) -> &StorageConfig {
        &self.config
    }
}

/// Result of a storage check
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageCheckResult {
    /// Storage is allowed
    Allowed,
    /// Total storage limit exceeded
    TotalLimitExceeded {
        current: u64,
        limit: u64,
    },
    /// Per-HarborID limit exceeded
    HarborLimitExceeded {
        harbor_id: [u8; 32],
        current: u64,
        limit: u64,
    },
}

impl StorageCheckResult {
    pub fn is_allowed(&self) -> bool {
        matches!(self, StorageCheckResult::Allowed)
    }
}

impl fmt::Display for StorageCheckResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageCheckResult::Allowed => write!(f, "Storage allowed"),
            StorageCheckResult::TotalLimitExceeded { current, limit } => {
                write!(
                    f,
                    "Total storage limit exceeded: {} bytes used, {} bytes limit",
                    current, limit
                )
            }
            StorageCheckResult::HarborLimitExceeded {
                harbor_id,
                current,
                limit,
            } => {
                write!(
                    f,
                    "Per-harbor limit exceeded for {}: {} bytes used, {} bytes limit",
                    hex::encode(&harbor_id[..8]),
                    current,
                    limit
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::{create_harbor_table, create_peer_table};
    use crate::data::harbor::cache_packet;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_harbor_table(&conn).unwrap();
        conn
    }

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_storage_stats_empty() {
        let conn = setup_db();
        let manager = StorageManager::new(StorageConfig::for_testing());

        let stats = manager.get_stats(&conn).unwrap();
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.packet_count, 0);
        assert_eq!(stats.harbor_count, 0);
    }

    #[test]
    fn test_storage_stats_with_data() {
        let mut conn = setup_db();
        let manager = StorageManager::new(StorageConfig::for_testing());

        // Add some packets
        let data = vec![0u8; 1000];
        cache_packet(
            &mut conn,
            &test_id(1),
            &test_id(10),
            &test_id(20),
            &data,
            &[test_id(30)],
            false,
        ).unwrap();

        cache_packet(
            &mut conn,
            &test_id(2),
            &test_id(10),
            &test_id(21),
            &data,
            &[test_id(30)],
            false,
        ).unwrap();

        let stats = manager.get_stats(&conn).unwrap();
        assert_eq!(stats.total_bytes, 2000);
        assert_eq!(stats.packet_count, 2);
        assert_eq!(stats.harbor_count, 1);
    }

    #[test]
    fn test_can_store_under_limits() {
        let conn = setup_db();
        let config = StorageConfig {
            max_total_bytes: 10000,
            max_per_harbor_bytes: 5000,
            enabled: true,
        };
        let manager = StorageManager::new(config);

        let result = manager.can_store(&conn, &test_id(1), 1000).unwrap();
        assert!(result.is_allowed());
    }

    #[test]
    fn test_can_store_total_limit() {
        let mut conn = setup_db();
        let config = StorageConfig {
            max_total_bytes: 1500,
            max_per_harbor_bytes: 5000,
            enabled: true,
        };
        let manager = StorageManager::new(config);

        // Add some data
        let data = vec![0u8; 1000];
        cache_packet(
            &mut conn,
            &test_id(1),
            &test_id(10),
            &test_id(20),
            &data,
            &[test_id(30)],
            false,
        ).unwrap();

        // Should fail - would exceed total
        let result = manager.can_store(&conn, &test_id(10), 600).unwrap();
        assert!(matches!(result, StorageCheckResult::TotalLimitExceeded { .. }));

        // Smaller should pass
        let result = manager.can_store(&conn, &test_id(10), 400).unwrap();
        assert!(result.is_allowed());
    }

    #[test]
    fn test_can_store_harbor_limit() {
        let mut conn = setup_db();
        let config = StorageConfig {
            max_total_bytes: 100000,
            max_per_harbor_bytes: 1500,
            enabled: true,
        };
        let manager = StorageManager::new(config);

        // Add data for harbor 10
        let data = vec![0u8; 1000];
        cache_packet(
            &mut conn,
            &test_id(1),
            &test_id(10),
            &test_id(20),
            &data,
            &[test_id(30)],
            false,
        ).unwrap();

        // Same harbor should fail
        let result = manager.can_store(&conn, &test_id(10), 600).unwrap();
        assert!(matches!(result, StorageCheckResult::HarborLimitExceeded { .. }));

        // Different harbor should pass
        let result = manager.can_store(&conn, &test_id(11), 600).unwrap();
        assert!(result.is_allowed());
    }

    #[test]
    fn test_evict_if_needed() {
        let mut conn = setup_db();
        let config = StorageConfig {
            max_total_bytes: 2500,
            max_per_harbor_bytes: 100000,
            enabled: true,
        };
        let manager = StorageManager::new(config);

        // Add 3 packets of 1000 bytes each = 3000 bytes (over limit)
        let data = vec![0u8; 1000];
        for i in 0..3 {
            cache_packet(
                &mut conn,
                &test_id(i),
                &test_id(10),
                &test_id(20),
                &data,
                &[test_id(30)],
                false,
            ).unwrap();
        }

        // Need 1000 more bytes, so need to free ~1500 to get under 2500-1000=1500
        let evicted = manager.evict_if_needed(&mut conn, 1000).unwrap();
        assert!(evicted > 0);

        let stats = manager.get_stats(&conn).unwrap();
        assert!(stats.total_bytes + 1000 <= config.max_total_bytes);
    }

    #[test]
    fn test_evict_for_harbor() {
        let mut conn = setup_db();
        let config = StorageConfig {
            max_total_bytes: 100000,
            max_per_harbor_bytes: 1500,
            enabled: true,
        };
        let manager = StorageManager::new(config);

        // Add packets for harbor 10
        let data = vec![0u8; 1000];
        cache_packet(
            &mut conn,
            &test_id(1),
            &test_id(10),
            &test_id(20),
            &data,
            &[test_id(30)],
            false,
        ).unwrap();

        cache_packet(
            &mut conn,
            &test_id(2),
            &test_id(10),
            &test_id(21),
            &data,
            &[test_id(30)],
            false,
        ).unwrap();

        // Harbor 10 has 2000 bytes, limit is 1500
        // Need room for 500 more
        let evicted = manager.evict_for_harbor(&mut conn, &test_id(10), 500).unwrap();
        assert!(evicted > 0);
    }

    #[test]
    fn test_storage_disabled() {
        let conn = setup_db();
        let config = StorageConfig::disabled();
        let manager = StorageManager::new(config);

        // Should always allow when disabled
        let result = manager.can_store(&conn, &test_id(1), u64::MAX / 2).unwrap();
        assert!(result.is_allowed());
    }

    #[test]
    fn test_storage_check_result_display() {
        let allowed = StorageCheckResult::Allowed;
        assert_eq!(allowed.to_string(), "Storage allowed");

        let total = StorageCheckResult::TotalLimitExceeded {
            current: 1000,
            limit: 500,
        };
        assert!(total.to_string().contains("1000"));
        assert!(total.to_string().contains("500"));

        let harbor = StorageCheckResult::HarborLimitExceeded {
            harbor_id: test_id(42),
            current: 2000,
            limit: 1000,
        };
        let display = harbor.to_string();
        assert!(display.contains("2000"));
        assert!(display.contains("1000"));
        // First 8 bytes of [42u8; 32] = "2a2a2a2a2a2a2a2a"
        assert!(display.contains("2a2a2a2a"));
    }

    #[test]
    fn test_storage_check_result_is_allowed() {
        assert!(StorageCheckResult::Allowed.is_allowed());
        assert!(!StorageCheckResult::TotalLimitExceeded {
            current: 100,
            limit: 50,
        }
        .is_allowed());
        assert!(!StorageCheckResult::HarborLimitExceeded {
            harbor_id: test_id(1),
            current: 100,
            limit: 50,
        }
        .is_allowed());
    }

    #[test]
    fn test_evict_when_under_limit() {
        let mut conn = setup_db();
        let config = StorageConfig {
            max_total_bytes: 100000,
            max_per_harbor_bytes: 50000,
            enabled: true,
        };
        let manager = StorageManager::new(config);

        // Add small amount of data
        let data = vec![0u8; 100];
        cache_packet(
            &mut conn,
            &test_id(1),
            &test_id(10),
            &test_id(20),
            &data,
            &[test_id(30)],
            false,
        )
        .unwrap();

        // Try to evict when we're well under limit
        let evicted = manager.evict_if_needed(&mut conn, 100).unwrap();
        assert_eq!(evicted, 0);
    }

    #[test]
    fn test_evict_zero_needed() {
        let mut conn = setup_db();
        let config = StorageConfig::for_testing();
        let manager = StorageManager::new(config);

        let evicted = manager.evict_if_needed(&mut conn, 0).unwrap();
        assert_eq!(evicted, 0);
    }

    #[test]
    fn test_config_presets() {
        let mobile = StorageConfig::mobile();
        assert_eq!(mobile.max_total_bytes, StorageConfig::MIN_STORAGE_BYTES);
        assert!(mobile.enabled);
        assert!(mobile.is_valid());

        let desktop = StorageConfig::desktop();
        assert_eq!(desktop.max_total_bytes, 50 * 1024 * 1024 * 1024);
        assert!(desktop.enabled);
        assert!(desktop.is_valid());

        let testing = StorageConfig::for_testing();
        assert_eq!(testing.max_total_bytes, 10 * 1024 * 1024);
        assert!(testing.enabled);
        // Testing config is below MIN but that's intentional for tests

        let disabled = StorageConfig::disabled();
        assert!(!disabled.enabled);
        assert!(disabled.is_valid()); // Disabled configs are always valid
    }

    #[test]
    fn test_desktop_with_size() {
        // Normal case - user specifies 100GB
        let config = StorageConfig::desktop_with_size(100 * 1024 * 1024 * 1024);
        assert_eq!(config.max_total_bytes, 100 * 1024 * 1024 * 1024);
        assert_eq!(config.max_per_harbor_bytes, 1024 * 1024 * 1024); // 1GB (1% of 100GB)
        assert!(config.is_valid());

        // Below minimum - enforces 10GB minimum
        let config = StorageConfig::desktop_with_size(5 * 1024 * 1024 * 1024); // 5GB requested
        assert_eq!(config.max_total_bytes, StorageConfig::MIN_STORAGE_BYTES); // Gets 10GB
        assert!(config.is_valid());

        // Zero - enforces minimum
        let config = StorageConfig::desktop_with_size(0);
        assert_eq!(config.max_total_bytes, StorageConfig::MIN_STORAGE_BYTES);
        assert!(config.is_valid());
    }

    #[test]
    fn test_platform_default() {
        let config = StorageConfig::platform_default();
        assert!(config.enabled);
        // On desktop (where tests run), should be desktop config
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        assert_eq!(config.max_total_bytes, 50 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_default_uses_platform() {
        let default = StorageConfig::default();
        let platform = StorageConfig::platform_default();
        assert_eq!(default.max_total_bytes, platform.max_total_bytes);
    }

    #[test]
    fn test_storage_stats_multiple_harbors() {
        let mut conn = setup_db();
        let manager = StorageManager::new(StorageConfig::for_testing());

        let data = vec![0u8; 500];

        // Add packets to different harbors
        for harbor_seed in 10..15 {
            cache_packet(
                &mut conn,
                &test_id(harbor_seed),
                &test_id(harbor_seed),
                &test_id(20),
                &data,
                &[test_id(30)],
                false,
            )
            .unwrap();
        }

        let stats = manager.get_stats(&conn).unwrap();
        assert_eq!(stats.packet_count, 5);
        assert_eq!(stats.harbor_count, 5);
        assert_eq!(stats.total_bytes, 2500);
        assert_eq!(stats.per_harbor.len(), 5);
    }

    #[test]
    fn test_evict_disabled() {
        let mut conn = setup_db();
        let config = StorageConfig::disabled();
        let manager = StorageManager::new(config);

        // Even with data, eviction should do nothing when disabled
        let evicted = manager.evict_if_needed(&mut conn, u64::MAX).unwrap();
        assert_eq!(evicted, 0);
    }
}
