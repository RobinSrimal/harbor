//! Maintenance tasks
//!
//! DHT routing table persistence and cleanup of expired/acknowledged data.

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, trace, warn};

use crate::data::cleanup_stale_peers;
use crate::data::harbor::{cleanup_expired, cleanup_old_pulled_packets};
use crate::network::dht::DhtService;

use std::time::Duration;
use crate::protocol::Protocol;

impl Protocol {
    /// Run the DHT routing table save loop (periodic persistence)
    pub(crate) async fn run_dht_save_loop(
        dht_service: Option<Arc<DhtService>>,
        running: Arc<RwLock<bool>>,
        save_interval: Duration,
    ) {
        let Some(dht_service) = dht_service else {
            info!("DHT save loop: no DHT service, skipping");
            return;
        };

        info!(
            interval_secs = save_interval.as_secs(),
            "DHT save loop started"
        );

        loop {
            // Check if we should stop
            if !*running.read().await {
                // Save one final time before stopping
                info!("DHT save loop: saving before shutdown");
                if let Err(e) = dht_service.save_routing_table().await {
                    warn!(error = %e, "DHT save loop: failed to save on shutdown");
                }
                break;
            }

            tokio::time::sleep(save_interval).await;

            // Save routing table
            if let Err(e) = dht_service.save_routing_table().await {
                warn!(error = %e, "DHT save loop: failed to save routing table");
            }
        }

        info!("DHT save loop stopped");
    }

    /// Run the cleanup loop (removes expired data)
    ///
    /// This task runs periodically and cleans up:
    /// 1. Expired packets from Harbor cache (TTL-based, 3 months)
    /// 2. Old pulled packet tracking (TTL-based, 3 months)
    /// 3. Stale peers (not seen for 3 months)
    ///
    /// Note: Outgoing packets are deleted immediately when:
    /// - All recipients acknowledge, OR
    /// - Successfully replicated to Harbor
    pub(crate) async fn run_cleanup_loop(
        db: Arc<Mutex<Connection>>,
        running: Arc<RwLock<bool>>,
        cleanup_interval: Duration,
    ) {
        info!(
            interval_secs = cleanup_interval.as_secs(),
            "Cleanup loop started"
        );

        loop {
            // Check if we should stop
            if !*running.read().await {
                break;
            }

            tokio::time::sleep(cleanup_interval).await;

            // Skip if stopped during sleep
            if !*running.read().await {
                break;
            }

            let db_lock = db.lock().await;

            // 1. Clean expired harbor cache packets
            match cleanup_expired(&db_lock) {
                Ok(deleted) if deleted > 0 => {
                    info!(deleted = deleted, "Cleanup: removed {} expired harbor packets", deleted);
                }
                Ok(_) => {
                    trace!("Cleanup: no expired harbor packets");
                }
                Err(e) => {
                    warn!(error = %e, "Cleanup: failed to clean expired harbor packets");
                }
            }

            // 2. Clean old pulled packet tracking (same lifetime as harbor packets)
            match cleanup_old_pulled_packets(&db_lock) {
                Ok(deleted) if deleted > 0 => {
                    info!(deleted = deleted, "Cleanup: removed {} old pulled packet records", deleted);
                }
                Ok(_) => {
                    trace!("Cleanup: no old pulled packet records");
                }
                Err(e) => {
                    warn!(error = %e, "Cleanup: failed to clean old pulled packet records");
                }
            }

            // 4. Clean stale peers (not seen for PEER_RETENTION_SECS)
            match cleanup_stale_peers(&db_lock) {
                Ok(deleted) if deleted > 0 => {
                    info!(deleted = deleted, "Cleanup: removed {} stale peers", deleted);
                }
                Ok(_) => {
                    trace!("Cleanup: no stale peers");
                }
                Err(e) => {
                    warn!(error = %e, "Cleanup: failed to clean stale peers");
                }
            }
        }

        info!("Cleanup loop stopped");
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::ProtocolConfig;

    #[test]
    fn test_dht_save_interval_default_is_reasonable() {
        let config = ProtocolConfig::default();
        // Should save frequently enough to not lose much data
        assert!(config.dht_save_interval_secs >= 60);
        // But not too frequently (I/O overhead)
        assert!(config.dht_save_interval_secs <= 600);
    }

    #[test]
    fn test_cleanup_interval_default_is_reasonable() {
        let config = ProtocolConfig::default();
        // Cleanup can be infrequent (hourly is fine)
        assert!(config.cleanup_interval_secs >= 1800); // 30 min
        // But should run at least daily
        assert!(config.cleanup_interval_secs <= 86400);
    }

    #[test]
    fn test_intervals_ordering() {
        let config = ProtocolConfig::default();
        // DHT save should be more frequent than cleanup
        assert!(config.dht_save_interval_secs < config.cleanup_interval_secs);
    }
}

