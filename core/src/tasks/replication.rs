//! Packet replication handler
//!
//! Replicates unacknowledged packets to Harbor Nodes for offline delivery.
//!
//! # Crypto Optimization
//!
//! The outgoing_packets table stores RAW payloads (not encrypted).
//! This task seals packets on-the-fly before sending to Harbor:
//! - Direct delivery uses QUIC TLS only (no app-level crypto)
//! - Harbor storage requires full crypto (seal: encrypt + MAC + sign)

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};

use crate::data::send::{
    get_packets_needing_replication, delete_outgoing_packet,
};
use crate::network::harbor::HarborService;
use crate::network::harbor::protocol::HarborPacketType;
use crate::security::send::{seal_topic_packet_with_epoch, seal_dm_packet, EpochKeys};

use crate::protocol::Protocol;

impl Protocol {
    /// Run the replication checker
    pub(crate) async fn run_replication_checker(
        harbor_service: Arc<HarborService>,
        our_id: [u8; 32],
        running: Arc<RwLock<bool>>,
        check_interval: Duration,
        replication_factor: usize,
        max_replication_attempts: usize,
    ) {
        let db = harbor_service.db().clone();
        let dht_service = harbor_service.dht_service().clone();
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

                // Get identity for sealing
                let identity = harbor_service.identity();

                // Seal the raw payload for Harbor storage
                // DM check: harbor_id == topic_id means it's a DM (recipient_id used for both)
                // Topic check: harbor_id != topic_id (harbor_id is hash of topic_id)
                let is_dm = packet.harbor_id == packet.topic_id;

                let (sealed_bytes, sealed_packet_id) = if is_dm {
                    // DM: seal with recipient's public key (stored as topic_id/harbor_id)
                    match seal_dm_packet(
                        &identity.private_key,
                        &identity.public_key,
                        &packet.topic_id, // recipient_public_key
                        &packet.packet_data,
                    ) {
                        Ok(send_packet) => {
                            let packet_id = send_packet.packet_id;
                            let bytes = match send_packet.to_bytes() {
                                Ok(b) => b,
                                Err(e) => {
                                    warn!(
                                        packet_id = hex::encode(packet.packet_id),
                                        error = %e,
                                        "failed to serialize DM packet for Harbor"
                                    );
                                    continue;
                                }
                            };
                            (bytes, packet_id)
                        }
                        Err(e) => {
                            warn!(
                                packet_id = hex::encode(packet.packet_id),
                                error = %e,
                                "failed to seal DM packet for Harbor"
                            );
                            continue;
                        }
                    }
                } else {
                    // Topic: seal with stored epoch key
                    // All topics have at least epoch 0 key stored at creation time
                    let current_epoch_key = {
                        let db_lock = db.lock().await;
                        crate::data::get_current_epoch_key(&db_lock, &packet.topic_id)
                            .ok()
                            .flatten()
                    };

                    let seal_result = match current_epoch_key {
                        Some(stored_key) => {
                            let epoch_keys = EpochKeys::derive_from_secret(
                                stored_key.epoch,
                                &stored_key.key_data,
                            );
                            trace!(
                                topic = hex::encode(&packet.topic_id[..8]),
                                epoch = stored_key.epoch,
                                "sealing topic packet with epoch key"
                            );
                            seal_topic_packet_with_epoch(
                                &packet.topic_id,
                                &identity.private_key,
                                &identity.public_key,
                                &packet.packet_data,
                                &epoch_keys,
                            )
                        }
                        None => {
                            // Should never happen - all topics have epoch 0 key
                            warn!(
                                topic = hex::encode(&packet.topic_id[..8]),
                                "no epoch key found for topic (should never happen)"
                            );
                            continue;
                        }
                    };

                    match seal_result {
                        Ok(send_packet) => {
                            let packet_id = send_packet.packet_id;
                            let bytes = match send_packet.to_bytes() {
                                Ok(b) => b,
                                Err(e) => {
                                    warn!(
                                        packet_id = hex::encode(packet.packet_id),
                                        error = %e,
                                        "failed to serialize topic packet for Harbor"
                                    );
                                    continue;
                                }
                            };
                            (bytes, packet_id)
                        }
                        Err(e) => {
                            warn!(
                                packet_id = hex::encode(packet.packet_id),
                                error = %e,
                                "failed to seal topic packet for Harbor"
                            );
                            continue;
                        }
                    }
                };

                trace!(
                    original_id = hex::encode(&packet.packet_id[..8]),
                    sealed_id = hex::encode(&sealed_packet_id[..8]),
                    is_dm = is_dm,
                    raw_size = packet.packet_data.len(),
                    sealed_size = sealed_bytes.len(),
                    "sealed packet for Harbor storage"
                );

                // Create StoreRequest with SEALED packet data and its packet_id
                let store_req = crate::network::harbor::protocol::StoreRequest {
                    packet_data: sealed_bytes,  // Sealed, not raw
                    packet_id: sealed_packet_id,  // Use the packet_id from the sealed packet
                    harbor_id: packet.harbor_id,
                    sender_id: our_id,
                    recipients: unacked_recipients,
                    packet_type,
                };

                // Send to Harbor Nodes (best effort, try up to max_replication_attempts)
                let mut success_count = 0;
                for harbor_node in harbor_nodes.iter().take(max_replication_attempts) {
                    if success_count >= replication_factor {
                        break;
                    }

                    match harbor_service.send_harbor_store(harbor_node, &store_req).await {
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

