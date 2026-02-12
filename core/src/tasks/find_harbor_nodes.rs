//! Harbor node discovery task
//!
//! Periodically refreshes the harbor_nodes_cache table by performing DHT lookups
//! for all active harbor_ids (topics we're subscribed to).

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::data::harbor::{get_all_active_harbor_ids, replace_harbor_nodes};
use crate::network::dht::DhtService;
use crate::network::harbor::HarborService;
use crate::protocol::Protocol;

/// Maximum harbor nodes to cache per harbor_id
const MAX_HARBOR_NODES_PER_ID: usize = 30;

impl Protocol {
    /// Run the harbor node discovery loop
    ///
    /// Periodically performs DHT lookups for all active harbor_ids and
    /// updates the harbor_nodes_cache table with the results.
    pub(crate) async fn run_harbor_node_discovery(
        db: Arc<Mutex<Connection>>,
        dht_service: Option<Arc<DhtService>>,
        running: Arc<RwLock<bool>>,
        refresh_interval: Duration,
    ) {
        let Some(dht) = dht_service else {
            info!("Harbor node discovery: no DHT service, skipping");
            return;
        };

        info!(
            interval_secs = refresh_interval.as_secs(),
            "Harbor node discovery started"
        );

        loop {
            // Check if we should stop
            if !*running.read().await {
                break;
            }

            tokio::time::sleep(refresh_interval).await;

            // Get all active harbor_ids from topics table
            let harbor_ids = {
                let db_lock = db.lock().await;
                match get_all_active_harbor_ids(&db_lock) {
                    Ok(ids) => ids,
                    Err(e) => {
                        warn!(error = %e, "Harbor node discovery: failed to get active harbor_ids");
                        continue;
                    }
                }
            };

            if harbor_ids.is_empty() {
                debug!("Harbor node discovery: no active harbor_ids to refresh");
                continue;
            }

            debug!(
                count = harbor_ids.len(),
                "Harbor node discovery: refreshing cache for harbor_ids"
            );

            // Refresh cache for each harbor_id
            for harbor_id in harbor_ids {
                let nodes =
                    HarborService::find_harbor_nodes_dht(&Some(dht.clone()), &harbor_id).await;

                if nodes.is_empty() {
                    debug!(
                        harbor_id = hex::encode(&harbor_id[..8]),
                        "Harbor node discovery: no nodes found for harbor_id"
                    );
                    continue;
                }

                // Limit to MAX_HARBOR_NODES_PER_ID
                let nodes: Vec<_> = nodes.into_iter().take(MAX_HARBOR_NODES_PER_ID).collect();

                // Update cache
                let db_lock = db.lock().await;
                if let Err(e) = replace_harbor_nodes(&db_lock, &harbor_id, &nodes) {
                    warn!(
                        error = %e,
                        harbor_id = hex::encode(&harbor_id[..8]),
                        "Harbor node discovery: failed to update cache"
                    );
                } else {
                    debug!(
                        harbor_id = hex::encode(&harbor_id[..8]),
                        count = nodes.len(),
                        "Harbor node discovery: updated cache"
                    );
                }
            }
        }

        info!("Harbor node discovery stopped");
    }
}
