//! Background tasks for the Protocol
//!
//! This module contains the long-running background tasks:
//! - Replication checker (replicates unacked packets to Harbor Nodes)
//! - Harbor pull loop (fetches missed packets from Harbor Nodes)
//! - Harbor sync loop (syncs between Harbor Nodes for redundancy)
//! - DHT save loop (persists routing table)
//! - DHT bootstrap/refresh loop (maintains routing table health)
//! - Cleanup loop (removes expired/acknowledged data)

mod harbor_pull;
mod harbor_sync;
mod maintenance;
mod replication;
mod share_pull;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::network::dht::ApiClient as DhtApiClient;

use crate::protocol::Protocol;

/// Number of rapid lookups during initial bootstrap phase
const DHT_INITIAL_LOOKUP_COUNT: usize = 6;

impl Protocol {
    /// Start background tasks (incoming handler, replication checker, harbor pull)
    pub(crate) async fn start_background_tasks(&self, _shutdown_rx: mpsc::Receiver<()>) {
        let mut tasks = self.tasks.write().await;

        // 1. Incoming message handler (handles Send, DHT, Harbor, Share protocols)
        let endpoint = self.endpoint.clone();
        let db = self.db.clone();
        let event_tx = self.event_tx.clone();
        let our_id = self.identity.public_key;
        let running = self.running.clone();
        let dht_client = self.dht_client.clone();
        
        // Initialize blob store for Share protocol
        let blob_path = self.blob_path();
        let blob_store = match crate::data::BlobStore::new(&blob_path) {
            Ok(store) => Some(std::sync::Arc::new(store)),
            Err(e) => {
                warn!(error = %e, path = %blob_path.display(), "failed to initialize blob store");
                None
            }
        };

        let incoming_task = tokio::spawn(async move {
            Self::run_incoming_handler(endpoint, db, event_tx, our_id, running, dht_client, blob_store).await;
        });
        tasks.push(incoming_task);

        // 2. Replication checker (checks for unacked packets and replicates to Harbor)
        let db = self.db.clone();
        let endpoint = self.endpoint.clone();
        let running = self.running.clone();
        let our_id = self.identity.public_key;
        let dht_client = self.dht_client.clone();
        let replication_interval = Duration::from_secs(self.config.replication_check_interval_secs);
        let replication_factor = self.config.replication_factor;
        let max_replication_attempts = self.config.max_replication_attempts;
        let harbor_connect_timeout = Duration::from_secs(self.config.harbor_connect_timeout_secs);
        let harbor_response_timeout = Duration::from_secs(self.config.harbor_response_timeout_secs);

        let replication_task = tokio::spawn(async move {
            Self::run_replication_checker(
                db, endpoint, our_id, dht_client, running, replication_interval,
                replication_factor, max_replication_attempts,
                harbor_connect_timeout, harbor_response_timeout,
            ).await;
        });
        tasks.push(replication_task);

        // 3. Harbor pull loop (periodically pull missed packets from Harbor Nodes)
        let db = self.db.clone();
        let endpoint = self.endpoint.clone();
        let running = self.running.clone();
        let event_tx = self.event_tx.clone();
        let our_id = self.identity.public_key;
        let dht_client = self.dht_client.clone();
        let pull_interval = Duration::from_secs(self.config.harbor_pull_interval_secs);
        let pull_max_nodes = self.config.harbor_pull_max_nodes;
        let pull_early_stop = self.config.harbor_pull_early_stop;
        let harbor_connect_timeout = Duration::from_secs(self.config.harbor_connect_timeout_secs);
        let harbor_response_timeout = Duration::from_secs(self.config.harbor_response_timeout_secs);

        let pull_task = tokio::spawn(async move {
            Self::run_harbor_pull_loop(
                db,
                endpoint,
                event_tx,
                our_id,
                dht_client,
                running,
                pull_interval,
                pull_max_nodes,
                pull_early_stop,
                harbor_connect_timeout,
                harbor_response_timeout,
            )
            .await;
        });
        tasks.push(pull_task);

        // 4. Harbor sync loop (Harbor Node to Harbor Node sync for redundancy)
        let db = self.db.clone();
        let endpoint = self.endpoint.clone();
        let running = self.running.clone();
        let our_id = self.identity.public_key;
        let dht_client = self.dht_client.clone();
        let sync_interval = Duration::from_secs(self.config.harbor_sync_interval_secs);
        let sync_candidates = self.config.harbor_sync_candidates;

        let sync_task = tokio::spawn(async move {
            Self::run_harbor_sync_loop(
                db,
                endpoint,
                our_id,
                dht_client,
                running,
                sync_interval,
                sync_candidates,
            )
            .await;
        });
        tasks.push(sync_task);

        // 5. DHT routing table persistence (saves periodically)
        let db = self.db.clone();
        let running = self.running.clone();
        let dht_client = self.dht_client.clone();
        let dht_save_interval = Duration::from_secs(self.config.dht_save_interval_secs);

        let dht_save_task = tokio::spawn(async move {
            Self::run_dht_save_loop(db, dht_client, running, dht_save_interval).await;
        });
        tasks.push(dht_save_task);

        // 6. DHT bootstrap and refresh loop (maintains routing table health)
        let running = self.running.clone();
        let dht_client = self.dht_client.clone();
        let dht_bootstrap_delay = Duration::from_secs(self.config.dht_bootstrap_delay_secs);
        let dht_initial_refresh = Duration::from_secs(self.config.dht_initial_refresh_interval_secs);
        let dht_stable_refresh = Duration::from_secs(self.config.dht_stable_refresh_interval_secs);

        let dht_refresh_task = tokio::spawn(async move {
            Self::run_dht_refresh_loop(
                dht_client, running,
                dht_bootstrap_delay, dht_initial_refresh, dht_stable_refresh,
            ).await;
        });
        tasks.push(dht_refresh_task);

        // 7. Cleanup task (removes expired/acknowledged packets)
        let db = self.db.clone();
        let running = self.running.clone();
        let cleanup_interval = Duration::from_secs(self.config.cleanup_interval_secs);

        let cleanup_task = tokio::spawn(async move {
            Self::run_cleanup_loop(db, running, cleanup_interval).await;
        });
        tasks.push(cleanup_task);

        // 8. Share pull task (retries incomplete blob downloads)
        let db = self.db.clone();
        let endpoint = self.endpoint.clone();
        let running = self.running.clone();
        let our_id = self.identity.public_key;
        let share_pull_interval = Duration::from_secs(self.config.share_pull_interval_secs);
        let share_blob_path = self.blob_path();
        let share_event_tx = self.event_tx.clone();

        let share_pull_task = tokio::spawn(async move {
            share_pull::run_share_pull_task(
                db,
                endpoint,
                our_id,
                running,
                share_pull_interval,
                share_blob_path,
                share_event_tx,
            ).await;
        });
        tasks.push(share_pull_task);

        info!("Background tasks started");
    }

    /// Run the DHT bootstrap and refresh loop
    ///
    /// On startup: waits for relay connection, then does rapid lookups to bootstrap
    /// After initial phase: does periodic random lookups to maintain routing table
    async fn run_dht_refresh_loop(
        dht_client: Option<DhtApiClient>,
        running: Arc<RwLock<bool>>,
        bootstrap_delay: Duration,
        initial_refresh_interval: Duration,
        stable_refresh_interval: Duration,
    ) {
        let Some(dht) = dht_client else {
            debug!("DHT client not available, skipping refresh loop");
            return;
        };

        // Wait a bit for relay connection before bootstrapping
        info!(
            "DHT: waiting {}s for relay connection before bootstrap...",
            bootstrap_delay.as_secs()
        );
        tokio::time::sleep(bootstrap_delay).await;

        // Phase 1: Initial rapid lookups to bootstrap the DHT
        // This ensures we quickly discover peers even when nodes join in batches
        info!("DHT: starting initial bootstrap phase ({} lookups at {}s intervals)...",
            DHT_INITIAL_LOOKUP_COUNT, initial_refresh_interval.as_secs());

        let mut initial_interval = tokio::time::interval(initial_refresh_interval);
        initial_interval.tick().await; // Skip immediate tick

        for i in 0..DHT_INITIAL_LOOKUP_COUNT {
            // Check if still running
            {
                let running = running.read().await;
                if !*running {
                    return;
                }
            }

            // Alternate between self-lookup and random lookup for better coverage
            if i % 2 == 0 {
                debug!("DHT: bootstrap phase - self-lookup ({}/{})", i + 1, DHT_INITIAL_LOOKUP_COUNT);
                match dht.self_lookup().await {
                    Ok(()) => debug!("DHT: self-lookup completed"),
                    Err(e) => warn!("DHT: self-lookup failed: {}", e),
                }
            } else {
                debug!("DHT: bootstrap phase - random lookup ({}/{})", i + 1, DHT_INITIAL_LOOKUP_COUNT);
                match dht.random_lookup().await {
                    Ok(()) => debug!("DHT: random lookup completed"),
                    Err(e) => warn!("DHT: random lookup failed: {}", e),
                }
            }

            initial_interval.tick().await;
        }

        info!("DHT: bootstrap phase complete, switching to stable refresh interval ({}s)",
            stable_refresh_interval.as_secs());

        // Phase 2: Stable periodic refresh
        let mut stable_interval = tokio::time::interval(stable_refresh_interval);
        stable_interval.tick().await; // Skip immediate tick

        loop {
            // Check if still running
            {
                let running = running.read().await;
                if !*running {
                    break;
                }
            }

            stable_interval.tick().await;

            // Do a random lookup to refresh distant buckets
            debug!("DHT: performing random lookup to refresh routing table...");
            match dht.random_lookup().await {
                Ok(()) => debug!("DHT: random lookup completed"),
                Err(e) => warn!("DHT: random lookup failed: {}", e),
            }
        }

        debug!("DHT refresh loop stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ProtocolConfig;

    #[test]
    fn test_dht_bootstrap_delay_default_is_reasonable() {
        let config = ProtocolConfig::default();
        // Bootstrap delay should be long enough for relay connection
        assert!(config.dht_bootstrap_delay_secs >= 3);
        assert!(config.dht_bootstrap_delay_secs <= 30);
    }

    #[test]
    fn test_dht_initial_refresh_interval_default_is_fast() {
        let config = ProtocolConfig::default();
        // Initial refresh should be quick for fast convergence
        assert!(config.dht_initial_refresh_interval_secs >= 5);
        assert!(config.dht_initial_refresh_interval_secs <= 30);
    }

    #[test]
    fn test_dht_stable_refresh_interval_default_is_reasonable() {
        let config = ProtocolConfig::default();
        // Stable refresh interval should be measured in minutes
        assert!(config.dht_stable_refresh_interval_secs >= 60);
        // But not too long (max 1 hour)
        assert!(config.dht_stable_refresh_interval_secs <= 3600);
    }

    #[test]
    fn test_dht_initial_lookup_count() {
        // Should do enough lookups to discover peers
        assert!(DHT_INITIAL_LOOKUP_COUNT >= 3);
        // But not too many (would be slow)
        assert!(DHT_INITIAL_LOOKUP_COUNT <= 20);
    }

    #[test]
    fn test_intervals_ordering() {
        let config = ProtocolConfig::default();
        // Initial interval should be faster than stable
        assert!(config.dht_initial_refresh_interval_secs < config.dht_stable_refresh_interval_secs);
        // Bootstrap delay should be shorter than initial interval
        assert!(config.dht_bootstrap_delay_secs <= config.dht_initial_refresh_interval_secs);
    }

    #[test]
    fn test_bootstrap_phase_duration() {
        let config = ProtocolConfig::default();
        // Total bootstrap phase duration (in seconds)
        let bootstrap_duration = config.dht_bootstrap_delay_secs 
            + config.dht_initial_refresh_interval_secs * DHT_INITIAL_LOOKUP_COUNT as u64;
        // Should complete within about 1-2 minutes
        assert!(bootstrap_duration >= 30);
        assert!(bootstrap_duration <= 180);
    }
}
