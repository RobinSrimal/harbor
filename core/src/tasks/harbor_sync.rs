//! Harbor sync loop
//!
//! Syncs between Harbor Nodes for redundancy. Picks 1 random node from the
//! top N closest to each HarborID.

use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::network::dht::ApiClient as DhtApiClient;
use crate::network::harbor::protocol::{HARBOR_ALPN, HarborMessage};
use crate::network::harbor::service::HarborService;

use crate::protocol::{Protocol, ProtocolError};

impl Protocol {
    /// Run the Harbor sync loop (Harbor Node to Harbor Node sync)
    ///
    /// Periodically syncs with other Harbor Nodes to ensure redundancy.
    /// Picks 1 random node from the top N closest to each HarborID.
    pub(crate) async fn run_harbor_sync_loop(
        db: Arc<Mutex<Connection>>,
        endpoint: Endpoint,
        our_id: [u8; 32],
        dht_client: Option<DhtApiClient>,
        running: Arc<RwLock<bool>>,
        sync_interval: Duration,
        sync_candidates: usize,
    ) {
        use rand::seq::SliceRandom;

        let harbor_service = HarborService::new(our_id);
        
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
                
                // Find closest Harbor Nodes for this HarborID
                let harbor_nodes = Self::find_harbor_nodes(&dht_client, &harbor_id).await;

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
                match Self::send_harbor_sync(&endpoint, &sync_partner, &sync_request).await {
                    Ok(response) => {
                        let missing_count = response.missing_packets.len();
                        let delivery_updates = response.delivery_updates.len();
                        
                        // Apply the response
                        let mut db_lock = db.lock().await;
                        if let Err(e) = harbor_service.apply_sync_response(&mut *db_lock, response) {
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

    /// Send a SyncRequest to another Harbor Node
    async fn send_harbor_sync(
        endpoint: &Endpoint,
        harbor_node: &[u8; 32],
        request: &crate::network::harbor::protocol::SyncRequest,
    ) -> Result<crate::network::harbor::protocol::SyncResponse, ProtocolError> {
        let node_hex = hex::encode(harbor_node);
        debug!(partner = %node_hex, "Harbor sync: connecting to partner");

        let node_id = iroh::NodeId::from_bytes(harbor_node)
            .map_err(|_| ProtocolError::Network("invalid node id".into()))?;

        let conn = endpoint
            .connect(node_id, HARBOR_ALPN)
            .await
            .map_err(|e| {
                debug!(partner = %node_hex, error = %e, "Harbor sync: connection failed");
                ProtocolError::Network(e.to_string())
            })?;

        debug!(partner = %node_hex, "Harbor sync: connection established, opening stream");

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| {
                debug!(partner = %node_hex, error = %e, "Harbor sync: failed to open stream");
                ProtocolError::Network(e.to_string())
            })?;

        // Send request
        let msg = HarborMessage::SyncRequest(request.clone());
        let data = postcard::to_allocvec(&msg)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        debug!(
            partner = %node_hex,
            request_size = data.len(),
            packets_we_have = request.have_packets.len(),
            "Harbor sync: sending request"
        );

        send.write_all(&data).await
            .map_err(|e| ProtocolError::Network(e.to_string()))?;
        send.finish()
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        // Read response (iroh's read_to_end takes a size limit, returns Vec<u8>)
        let response_data = recv.read_to_end(1024 * 1024) // 1MB limit
            .await
            .map_err(|e| {
                debug!(partner = %node_hex, error = %e, "Harbor sync: failed to read response");
                ProtocolError::Network(e.to_string())
            })?;

        debug!(
            partner = %node_hex,
            response_size = response_data.len(),
            "Harbor sync: received response"
        );

        let response: HarborMessage = postcard::from_bytes(&response_data)
            .map_err(|e| ProtocolError::Network(e.to_string()))?;

        match response {
            HarborMessage::SyncResponse(resp) => {
                debug!(
                    partner = %node_hex,
                    missing_packets = resp.missing_packets.len(),
                    delivery_updates = resp.delivery_updates.len(),
                    "Harbor sync: parsed response"
                );
                Ok(resp)
            },
            other => {
                debug!(partner = %node_hex, "Harbor sync: unexpected response type");
                Err(ProtocolError::Network(format!("unexpected response: {:?}", std::mem::discriminant(&other))))
            }
        }
    }
}

