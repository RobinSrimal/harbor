//! Harbor sync loop
//!
//! Syncs between Harbor Nodes for redundancy. Picks 1 random node from the
//! top N closest to each HarborID.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::network::harbor::HarborService;

use crate::protocol::Protocol;

impl Protocol {
    /// Run the Harbor sync loop (Harbor Node to Harbor Node sync)
    ///
    /// Periodically syncs with other Harbor Nodes to ensure redundancy.
    /// Picks 1 random node from the top N closest to each HarborID.
    pub(crate) async fn run_harbor_sync_loop(
        harbor_service: Arc<HarborService>,
        our_id: [u8; 32],
        running: Arc<RwLock<bool>>,
        sync_interval: Duration,
        sync_candidates: usize,
    ) {
        use rand::seq::SliceRandom;

        let db = harbor_service.db().clone();
        let dht_service = harbor_service.dht_service().clone();

        info!(
            our_id = hex::encode(our_id),
            interval_secs = sync_interval.as_secs(),
            candidates = sync_candidates,
            "Harbor sync loop started"
        );

        loop {
            // Check if we should stop
            if !*running.read().await {
                break;
            }

            tokio::time::sleep(sync_interval).await;

            info!("Harbor sync: starting sync cycle");

            // Get all HarborIDs we're storing packets for
            let sync_requests = {
                let db_lock = db.lock().await;
                match harbor_service.create_sync_requests(&db_lock) {
                    Ok(reqs) => reqs,
                    Err(e) => {
                        warn!(error = %e, "Harbor sync: failed to create sync requests");
                        continue;
                    }
                }
            };

            if sync_requests.is_empty() {
                info!("Harbor sync: no Harbor duties to sync (not storing any packets)");
                continue;
            }

            info!(
                harbor_ids = sync_requests.len(),
                "Harbor sync: syncing {} HarborIDs",
                sync_requests.len()
            );

            let mut sync_success = 0;
            let mut sync_failed = 0;

            for (harbor_id, sync_request) in sync_requests {
                let harbor_id_hex = hex::encode(harbor_id);

                // Find Harbor Nodes from cache, fallback to DHT if empty
                let mut harbor_nodes = {
                    let db_lock = db.lock().await;
                    HarborService::find_harbor_nodes(&db_lock, &harbor_id)
                };

                // Fallback to DHT lookup if cache is empty
                if harbor_nodes.is_empty() {
                    harbor_nodes =
                        HarborService::find_harbor_nodes_dht(&dht_service, &harbor_id).await;
                    // Cache the result for next time
                    if !harbor_nodes.is_empty() {
                        let db_lock = db.lock().await;
                        let _ = crate::data::harbor::replace_harbor_nodes(
                            &db_lock,
                            &harbor_id,
                            &harbor_nodes,
                        );
                    }
                }

                if harbor_nodes.is_empty() {
                    debug!(
                        harbor_id = %harbor_id_hex,
                        "Harbor sync: no nodes found for HarborID"
                    );
                    continue;
                }

                // Filter out ourselves
                let candidates: Vec<[u8; 32]> = harbor_nodes
                    .into_iter()
                    .filter(|n| *n != our_id)
                    .take(sync_candidates)
                    .collect();

                if candidates.is_empty() {
                    debug!(
                        harbor_id = %harbor_id_hex,
                        "Harbor sync: no other nodes to sync with (we are the only one)"
                    );
                    continue;
                }

                info!(
                    harbor_id = %harbor_id_hex,
                    candidates = candidates.len(),
                    packets_we_have = sync_request.have_packets.len(),
                    "Harbor sync: found {} candidates, we have {} packets",
                    candidates.len(),
                    sync_request.have_packets.len()
                );

                // Pick 1 random node from the candidates
                let sync_partner = {
                    let mut rng = rand::thread_rng();
                    *candidates.choose(&mut rng).unwrap()
                };

                let partner_hex = hex::encode(sync_partner);
                info!(
                    harbor_id = %harbor_id_hex,
                    partner = %partner_hex,
                    "Harbor sync: selected partner (random from top {})",
                    sync_candidates
                );

                // Connect and sync
                match harbor_service
                    .send_harbor_sync(&sync_partner, &sync_request)
                    .await
                {
                    Ok(response) => {
                        let missing_count = response.missing_packets.len();
                        let delivery_updates = response.delivery_updates.len();

                        // Apply the response
                        let mut db_lock = db.lock().await;
                        if let Err(e) = harbor_service.apply_sync_response(&mut db_lock, response) {
                            warn!(
                                harbor_id = %harbor_id_hex,
                                partner = %partner_hex,
                                error = %e,
                                "Harbor sync: failed to apply response"
                            );
                            sync_failed += 1;
                        } else {
                            info!(
                                harbor_id = %harbor_id_hex,
                                partner = %partner_hex,
                                received_packets = missing_count,
                                delivery_updates = delivery_updates,
                                "Harbor sync: SUCCESS - received {} packets, {} delivery updates",
                                missing_count,
                                delivery_updates
                            );
                            sync_success += 1;
                        }
                    }
                    Err(e) => {
                        warn!(
                            harbor_id = %harbor_id_hex,
                            partner = %partner_hex,
                            error = %e,
                            "Harbor sync: FAILED to connect to partner"
                        );
                        sync_failed += 1;
                    }
                }
            }

            info!(
                success = sync_success,
                failed = sync_failed,
                "Harbor sync: cycle complete - {} success, {} failed",
                sync_success,
                sync_failed
            );
        }

        info!("Harbor sync loop stopped");
    }
}
