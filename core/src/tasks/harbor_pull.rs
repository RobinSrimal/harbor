//! Harbor pull loop
//!
//! Periodically pulls missed packets from Harbor Nodes for offline delivery.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::data::{get_joined_at, get_topics_for_member, dedup_check_and_mark, DedupResult};
use crate::network::harbor::HarborService;
use crate::network::process::ProcessScope;
use crate::network::stream::StreamService;
use crate::protocol::Protocol;
use crate::security::harbor_id_from_topic;

impl Protocol {
    /// Run the Harbor pull loop
    ///
    /// Pulls missed packets from Harbor Nodes for all topics we're subscribed to.
    /// This includes our endpoint_id which is registered as a "DM topic" on init,
    /// allowing unified handling of both DMs and regular topic messages.
    pub(crate) async fn run_harbor_pull_loop(
        harbor_service: Arc<HarborService>,
        our_id: [u8; 32],
        running: Arc<RwLock<bool>>,
        pull_interval: Duration,
        pull_max_nodes: usize,
        pull_early_stop: usize,
        stream_service: Arc<StreamService>,
    ) {
        let db = harbor_service.db().clone();
        let dht_service = harbor_service.dht_service().clone();

        loop {
            // Check if we should stop
            if !*running.read().await {
                break;
            }

            tokio::time::sleep(pull_interval).await;

            // Get all topics we're subscribed to (includes our endpoint_id for DMs)
            let topics = {
                let db_lock = db.lock().await;
                match get_topics_for_member(&db_lock, &our_id) {
                    Ok(t) => t,
                    Err(e) => {
                        error!(error = %e, "failed to get topics");
                        continue;
                    }
                }
            };

            // Pull from each topic (unified loop for DMs and regular topics)
            for topic_id in topics {
                // For DMs, harbor_id is the raw endpoint_id (our_id)
                // For topics, harbor_id is hash(topic_id)
                let is_dm = topic_id == our_id;
                let harbor_id = if is_dm {
                    topic_id  // DMs use raw endpoint_id as harbor_id
                } else {
                    harbor_id_from_topic(&topic_id)  // Topics use hashed harbor_id
                };

                // Find Harbor Nodes from cache, fallback to DHT if empty
                let mut harbor_nodes = {
                    let db_lock = db.lock().await;
                    HarborService::find_harbor_nodes(&db_lock, &harbor_id)
                };

                // Fallback to DHT lookup if cache is empty (newly subscribed topic)
                if harbor_nodes.is_empty() {
                    harbor_nodes = HarborService::find_harbor_nodes_dht(&dht_service, &harbor_id).await;
                    // Cache the result for next time
                    if !harbor_nodes.is_empty() {
                        let db_lock = db.lock().await;
                        let _ = crate::data::harbor::replace_harbor_nodes(&db_lock, &harbor_id, &harbor_nodes);
                    }
                }

                if harbor_nodes.is_empty() {
                    continue;
                }

                // Get packet IDs we already have and our join timestamp
                let (already_have, joined_at): (Vec<[u8; 32]>, i64) = {
                    let db_lock = db.lock().await;
                    let already_have =
                        crate::data::harbor::get_pulled_packet_ids(&db_lock, &topic_id)
                            .unwrap_or_default();
                    let joined_at = get_joined_at(&db_lock, &topic_id).unwrap_or(0);
                    (already_have, joined_at)
                };

                // Create PullRequest
                // For DMs: use since_timestamp = 0 to get all undelivered messages
                //   (DMs don't have "joined at" semantics - you want all undelivered DMs)
                // For topics: use joined_at to filter out messages from before membership
                let since_timestamp = if is_dm { 0 } else { joined_at };
                let pull_req = crate::network::harbor::protocol::PullRequest {
                    harbor_id,
                    recipient_id: our_id,
                    since_timestamp,
                    already_have,
                    relay_url: None,
                };

                // Try multiple Harbor Nodes - different nodes may have different packets
                let mut consecutive_empty = 0;
                for harbor_node in harbor_nodes.iter().take(pull_max_nodes) {
                    match harbor_service.send_harbor_pull(harbor_node, &pull_req).await {
                        Ok(packets) => {
                            if packets.is_empty() {
                                consecutive_empty += 1;
                                if consecutive_empty >= pull_early_stop {
                                    break;
                                }
                                continue;
                            }

                            consecutive_empty = 0;

                            debug!(
                                topic = hex::encode(&topic_id[..8]),
                                count = packets.len(),
                                harbor_node = hex::encode(&harbor_node[..8]),
                                is_dm = is_dm,
                                "pulled packets from Harbor"
                            );

                            // Process each pulled packet
                            for packet_info in packets {
                                // Atomically check AND mark as seen (prevents TOCTOU race)
                                let dedup_result = {
                                    let db_lock = db.lock().await;
                                    dedup_check_and_mark(
                                        &db_lock,
                                        &topic_id,
                                        &packet_info.packet_id,
                                        &packet_info.sender_id,
                                        &our_id,
                                    )
                                    .unwrap_or(DedupResult::AlreadySeen)
                                };

                                if !dedup_result.should_process() {
                                    continue;
                                }

                                // Process the packet with appropriate scope
                                let scope = if is_dm {
                                    ProcessScope::Dm
                                } else {
                                    ProcessScope::Topic { topic_id }
                                };

                                match harbor_service
                                    .process_pulled_packet(&packet_info, scope, &stream_service)
                                    .await
                                {
                                    Ok(()) => {
                                        // Send ack to Harbor Node (already marked as seen above)
                                        let ack = crate::network::harbor::protocol::DeliveryAck {
                                            packet_id: packet_info.packet_id,
                                            recipient_id: our_id,
                                        };
                                        let _ = harbor_service.send_harbor_ack(harbor_node, &ack).await;
                                    }
                                    Err(e) => {
                                        debug!(
                                            error = %e,
                                            topic = %hex::encode(&topic_id[..8]),
                                            is_dm = is_dm,
                                            "failed to process pulled packet"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            debug!(
                                harbor_node = hex::encode(&harbor_node[..8]),
                                error = %e,
                                "failed to pull from Harbor Node"
                            );
                        }
                    }
                }
            }
        }

        info!("Harbor pull loop stopped");
    }
}
