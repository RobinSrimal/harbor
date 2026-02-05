//! Harbor pull loop
//!
//! Periodically pulls missed packets from Harbor Nodes for offline delivery.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::data::{get_topics_for_member, get_joined_at, mark_pulled, was_pulled};
use crate::network::harbor::HarborService;
use crate::network::process::ProcessScope;
use crate::network::stream::StreamService;
use crate::protocol::Protocol;
use crate::security::harbor_id_from_topic;

impl Protocol {
    /// Run the Harbor pull loop
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

            // Get all topics we're subscribed to
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

            // === Pull DM packets (harbor_id = our endpoint_id) ===
            {
                let dm_harbor_id = our_id;
                let harbor_nodes = HarborService::find_harbor_nodes(&dht_service, &dm_harbor_id).await;

                if !harbor_nodes.is_empty() {
                    let already_have: Vec<[u8; 32]> = {
                        let db_lock = db.lock().await;
                        // Use our_id as "topic_id" for DM dedup tracking
                        crate::data::harbor::get_pulled_packet_ids(&db_lock, &our_id)
                            .unwrap_or_default()
                    };

                    let pull_req = crate::network::harbor::protocol::PullRequest {
                        harbor_id: dm_harbor_id,
                        recipient_id: our_id,
                        since_timestamp: 0,
                        already_have,
                        relay_url: None,
                    };

                    for harbor_node in harbor_nodes.iter().take(pull_max_nodes) {
                        match harbor_service.send_harbor_pull(harbor_node, &pull_req).await {
                            Ok(packets) => {
                                for packet_info in packets {
                                    // Check if already pulled
                                    {
                                        let db_lock = db.lock().await;
                                        if was_pulled(&db_lock, &our_id, &packet_info.packet_id).unwrap_or(true) {
                                            continue;
                                        }
                                    }

                                    // Process the packet
                                    match harbor_service
                                        .process_pulled_packet(&packet_info, ProcessScope::Dm, &stream_service)
                                        .await
                                    {
                                        Ok(()) => {
                                            // Mark as pulled
                                            {
                                                let db_lock = db.lock().await;
                                                let _ = mark_pulled(&db_lock, &our_id, &packet_info.packet_id);
                                            }

                                            // Send ack to Harbor Node
                                            let ack = crate::network::harbor::protocol::DeliveryAck {
                                                packet_id: packet_info.packet_id,
                                                recipient_id: our_id,
                                            };
                                            let _ = harbor_service.send_harbor_ack(harbor_node, &ack).await;
                                        }
                                        Err(e) => {
                                            debug!(error = %e, "failed to process pulled DM packet");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                debug!(error = %e, "failed to pull DMs from Harbor Node");
                            }
                        }
                    }
                }
            }

            // === Pull topic packets ===
            for topic_id in topics {
                let harbor_id = harbor_id_from_topic(&topic_id);

                // Find Harbor Nodes for this topic
                let harbor_nodes = HarborService::find_harbor_nodes(&dht_service, &harbor_id).await;

                if harbor_nodes.is_empty() {
                    continue;
                }

                // Get packet IDs we already have and our join timestamp
                let (already_have, joined_at): (Vec<[u8; 32]>, i64) = {
                    let db_lock = db.lock().await;
                    let already_have = crate::data::harbor::get_pulled_packet_ids(&db_lock, &topic_id)
                        .unwrap_or_default();
                    let joined_at = get_joined_at(&db_lock, &topic_id).unwrap_or(0);
                    (already_have, joined_at)
                };

                // Create PullRequest - only get packets sent after we joined
                let pull_req = crate::network::harbor::protocol::PullRequest {
                    harbor_id,
                    recipient_id: our_id,
                    since_timestamp: joined_at,
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
                                topic = hex::encode(topic_id),
                                count = packets.len(),
                                harbor_node = hex::encode(harbor_node),
                                "pulled packets from Harbor"
                            );

                            // Process each pulled packet
                            for packet_info in packets {
                                // Check if we already processed this
                                {
                                    let db_lock = db.lock().await;
                                    if was_pulled(&db_lock, &topic_id, &packet_info.packet_id).unwrap_or(true) {
                                        continue;
                                    }
                                }

                                // Process the packet
                                let scope = ProcessScope::Topic { topic_id };
                                match harbor_service
                                    .process_pulled_packet(&packet_info, scope, &stream_service)
                                    .await
                                {
                                    Ok(()) => {
                                        // Mark as pulled
                                        {
                                            let db_lock = db.lock().await;
                                            let _ = mark_pulled(&db_lock, &topic_id, &packet_info.packet_id);
                                        }

                                        // Send ack to Harbor Node
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
                                            "failed to process pulled topic packet"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            debug!(
                                harbor_node = hex::encode(harbor_node),
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
