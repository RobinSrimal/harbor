//! Packet replication handler
//!
//! Replicates unacknowledged packets to Harbor Nodes for offline delivery.

use std::sync::Arc;

use iroh::Endpoint;
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, trace};

use crate::data::send::{
    get_packets_needing_replication, delete_outgoing_packet,
};
use crate::network::dht::DhtService;
use crate::network::harbor::HarborService;
use crate::network::harbor::protocol::HarborPacketType;
use crate::resilience::{PoWChallenge, PoWConfig, compute_pow};

use std::time::Duration;

use crate::protocol::Protocol;

impl Protocol {
    /// Run the replication checker
    pub(crate) async fn run_replication_checker(
        db: Arc<Mutex<Connection>>,
        endpoint: Endpoint,
        our_id: [u8; 32],
        dht_service: Option<Arc<DhtService>>,
        running: Arc<RwLock<bool>>,
        check_interval: Duration,
        replication_factor: usize,
        max_replication_attempts: usize,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) {
        loop {
            // Check if we should stop
            if !*running.read().await {
                break;
            }

            tokio::time::sleep(check_interval).await;

            // Get packets needing replication (unacked after timeout)
            let packets = {
                let db_lock = db.lock().await;
                match get_packets_needing_replication(&db_lock) {
                    Ok(p) => p,
                    Err(e) => {
                        error!(error = %e, "failed to get packets needing replication");
                        continue;
                    }
                }
            };

            if packets.is_empty() {
                continue;
            }

            debug!(count = packets.len(), "packets needing replication");

            // For each packet, find Harbor Nodes via DHT and replicate
            for packet in &packets {
                // Find Harbor Nodes for this topic's HarborID
                let harbor_nodes = HarborService::find_harbor_nodes(&dht_service, &packet.harbor_id).await;

                if harbor_nodes.is_empty() {
                    debug!(
                        harbor_id = hex::encode(packet.harbor_id),
                        "no Harbor Nodes found"
                    );
                    continue;
                }

                // Get unacked recipients for this packet
                let unacked_recipients: Vec<[u8; 32]> = {
                    let db_lock = db.lock().await;
                    crate::data::send::get_unacknowledged_recipients(&db_lock, &packet.packet_id)
                        .unwrap_or_default()
                };

                if unacked_recipients.is_empty() {
                    // All recipients acknowledged - delete (shouldn't normally happen
                    // as packets are deleted on full ack, but handle edge cases)
                    let db_lock = db.lock().await;
                    let _ = delete_outgoing_packet(&db_lock, &packet.packet_id);
                    continue;
                }

                // Get packet type from stored value
                let packet_type = match packet.packet_type {
                    1 => HarborPacketType::Join,
                    2 => HarborPacketType::Leave,
                    _ => HarborPacketType::Content,
                };

                // Compute Proof of Work for this store request
                // Uses default difficulty - Harbor Nodes may reject if they require higher
                let pow_config = PoWConfig::default();
                let challenge = PoWChallenge::new(
                    packet.harbor_id,
                    packet.packet_id,
                    pow_config.difficulty_bits,
                );
                let proof_of_work = compute_pow(&challenge);

                // Create StoreRequest
                let store_req = crate::network::harbor::protocol::StoreRequest {
                    packet_data: packet.packet_data.clone(),
                    packet_id: packet.packet_id,
                    harbor_id: packet.harbor_id,
                    sender_id: our_id,
                    recipients: unacked_recipients,
                    packet_type,
                    proof_of_work: Some(proof_of_work),
                };

                // Send to Harbor Nodes (best effort, try up to max_replication_attempts)
                let mut success_count = 0;
                for harbor_node in harbor_nodes.iter().take(max_replication_attempts) {
                    if success_count >= replication_factor {
                        break;
                    }

                    match HarborService::send_harbor_store(&endpoint, harbor_node, &store_req, connect_timeout, response_timeout).await {
                        Ok(true) => {
                            success_count += 1;
                            trace!(
                                harbor_node = hex::encode(harbor_node),
                                packet_id = hex::encode(packet.packet_id),
                                "replicated to Harbor Node"
                            );
                        }
                        Ok(false) => {
                            debug!(
                                harbor_node = hex::encode(harbor_node),
                                "Harbor Node rejected store"
                            );
                        }
                        Err(e) => {
                            debug!(
                                harbor_node = hex::encode(harbor_node),
                                error = %e,
                                "failed to store on Harbor Node"
                            );
                        }
                    }
                }

                if success_count > 0 {
                    // Delete packet - Harbor now has responsibility for delivery
                    let db_lock = db.lock().await;
                    let _ = delete_outgoing_packet(&db_lock, &packet.packet_id);
                    info!(
                        packet_id = hex::encode(packet.packet_id),
                        harbor_count = success_count,
                        "replicated to Harbor, deleted from outgoing"
                    );
                }
            }
        }

        info!("Replication checker stopped");
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::ProtocolConfig;

    #[test]
    fn test_replication_check_interval_default_is_reasonable() {
        let config = ProtocolConfig::default();
        // Should check frequently enough for timely delivery
        assert!(config.replication_check_interval_secs >= 10);
        // But not too frequently (waste of resources)
        assert!(config.replication_check_interval_secs <= 120);
    }

    #[test]
    fn test_replication_check_interval_testing_is_fast() {
        let config = ProtocolConfig::for_testing();
        // Testing config should be fast for quick tests
        assert!(config.replication_check_interval_secs <= 10);
    }

    #[test]
    fn test_replication_factor_default_is_reasonable() {
        let config = ProtocolConfig::default();
        // At least 2 for redundancy
        assert!(config.replication_factor >= 2);
        // But not too many (network overhead)
        assert!(config.replication_factor <= 10);
    }

    #[test]
    fn test_replication_factor_testing_is_higher() {
        let config = ProtocolConfig::for_testing();
        // Testing should have higher replication for reliability
        assert!(config.replication_factor >= 3);
    }
    
    #[test]
    fn test_max_replication_attempts_is_reasonable() {
        let config = ProtocolConfig::default();
        // Should try more nodes than the target
        assert!(config.max_replication_attempts >= config.replication_factor);
        // But not too many
        assert!(config.max_replication_attempts <= 20);
    }
}

