//! Incoming Harbor message handlers
//!
//! Handles incoming requests from other nodes:
//! - Store requests (storing packets for offline recipients)
//! - Pull requests (retrieving missed packets)
//! - Acknowledgments (marking packets as delivered)
//! - Sync requests (Harbor-to-Harbor synchronization)
//! - Connection handler (accepts QUIC connections, dispatches to handlers)

use rusqlite::Connection;
use tracing::{debug, info, trace, warn};

use crate::data::harbor::{
    all_recipients_delivered, cache_packet, delete_packet, get_packets_for_recipient,
    get_packets_for_sync, mark_delivered, PacketType,
};
use crate::resilience::{RateLimitResult, StorageCheckResult};
use crate::security::send::packet::{verify_for_storage, PacketError};

use super::protocol::{
    DeliveryAck, DeliveryUpdate, HarborMessage, HarborPacketType, PacketInfo, PullRequest,
    PullResponse, StoreRequest, StoreResponse, SyncRequest, SyncResponse,
};
use super::service::{HarborError, HarborService};

/// Convert from protocol packet type to data layer packet type
fn harbor_packet_type_to_data(hpt: HarborPacketType) -> PacketType {
    match hpt {
        HarborPacketType::Content => PacketType::Content,
        HarborPacketType::Join => PacketType::Join,
        HarborPacketType::Leave => PacketType::Leave,
    }
}

/// Convert from data layer packet type to protocol packet type
fn data_packet_type_to_harbor(pt: PacketType) -> HarborPacketType {
    match pt {
        PacketType::Content => HarborPacketType::Content,
        PacketType::Join => HarborPacketType::Join,
        PacketType::Leave => HarborPacketType::Leave,
    }
}

impl HarborService {
    /// Check rate limit for a connection
    pub(super) fn check_connection_rate_limit(
        &self,
        endpoint_id: &[u8; 32],
    ) -> Result<(), HarborError> {
        let mut limiter = self.rate_limiter.lock().unwrap();
        match limiter.check_connection(endpoint_id) {
            RateLimitResult::Allowed => Ok(()),
            RateLimitResult::Limited { retry_after } => {
                warn!(
                    endpoint = %hex::encode(endpoint_id),
                    retry_after_ms = retry_after.as_millis(),
                    "Harbor: rate limited connection"
                );
                Err(HarborError::RateLimited { retry_after })
            }
        }
    }

    /// Check rate limit for a store request (both connection and harbor limits)
    fn check_store_rate_limit(
        &self,
        endpoint_id: &[u8; 32],
        harbor_id: &[u8; 32],
    ) -> Result<(), HarborError> {
        let mut limiter = self.rate_limiter.lock().unwrap();
        match limiter.check_store_request(endpoint_id, harbor_id) {
            RateLimitResult::Allowed => Ok(()),
            RateLimitResult::Limited { retry_after } => {
                warn!(
                    endpoint = %hex::encode(endpoint_id),
                    harbor_id = %hex::encode(harbor_id),
                    retry_after_ms = retry_after.as_millis(),
                    "Harbor: rate limited store request"
                );
                Err(HarborError::RateLimited { retry_after })
            }
        }
    }

    // ============ Server Side (Harbor Node) ============

    /// Handle a store request
    ///
    /// Called when another node wants to store a packet on us.
    ///
    /// # Verification
    ///
    /// Before storing, we verify:
    /// 1. Packet data is not empty
    /// 2. Recipients list is not empty
    /// 3. Packet size is within limits
    /// 4. Packet format is valid (can be deserialized)
    /// 5. HarborID in packet matches the request
    /// 6. Sender ID in packet matches the request
    /// 7. **Signature is valid** (proves the sender created this packet)
    ///
    /// Note: We CANNOT verify the MAC (requires topic_id which we don't have).
    pub fn handle_store(
        &self,
        conn: &mut Connection,
        request: StoreRequest,
    ) -> Result<StoreResponse, HarborError> {
        let packet_id_hex = hex::encode(request.packet_id);
        let harbor_id_hex = hex::encode(request.harbor_id);
        let sender_hex = hex::encode(request.sender_id);

        info!(
            packet_id = %packet_id_hex,
            harbor_id = %harbor_id_hex,
            sender = %sender_hex,
            recipients = request.recipients.len(),
            size = request.packet_data.len(),
            "Harbor STORE: received store request"
        );

        // Check rate limits (both connection and harbor limits)
        self.check_store_rate_limit(&request.sender_id, &request.harbor_id)?;

        // Verify Proof of Work if enabled
        if self.pow_config.enabled {
            match &request.proof_of_work {
                Some(pow) => {
                    use crate::resilience::{verify_pow, PoWVerifyResult};

                    // Verify PoW is bound to this request
                    if pow.harbor_id != request.harbor_id {
                        warn!(
                            packet_id = %packet_id_hex,
                            "Harbor STORE: rejected - PoW harbor_id mismatch"
                        );
                        return Err(HarborError::InvalidPoW(PoWVerifyResult::InsufficientDifficulty));
                    }
                    if pow.packet_id != request.packet_id {
                        warn!(
                            packet_id = %packet_id_hex,
                            "Harbor STORE: rejected - PoW packet_id mismatch"
                        );
                        return Err(HarborError::InvalidPoW(PoWVerifyResult::InsufficientDifficulty));
                    }

                    // Verify PoW meets requirements
                    let result = verify_pow(pow, &self.pow_config);
                    if !result.is_valid() {
                        warn!(
                            packet_id = %packet_id_hex,
                            result = %result,
                            "Harbor STORE: rejected - invalid PoW"
                        );
                        return Err(HarborError::InvalidPoW(result));
                    }
                    trace!(packet_id = %packet_id_hex, nonce = pow.nonce, "PoW verified");
                }
                None => {
                    warn!(
                        packet_id = %packet_id_hex,
                        "Harbor STORE: rejected - PoW required but not provided"
                    );
                    return Err(HarborError::PoWRequired);
                }
            }
        }

        // Check storage limits before accepting
        let packet_size = request.packet_data.len() as u64;
        match self.storage_manager.can_store(conn, &request.harbor_id, packet_size) {
            Ok(StorageCheckResult::Allowed) => {
                // Storage allowed, continue
            }
            Ok(StorageCheckResult::TotalLimitExceeded { current, limit }) => {
                warn!(
                    packet_id = %packet_id_hex,
                    current_bytes = current,
                    limit_bytes = limit,
                    "Harbor STORE: rejected - storage full"
                );
                return Err(HarborError::StorageFull { current, limit });
            }
            Ok(StorageCheckResult::HarborLimitExceeded {
                harbor_id,
                current,
                limit,
            }) => {
                warn!(
                    packet_id = %packet_id_hex,
                    harbor_id = %hex::encode(harbor_id),
                    current_bytes = current,
                    limit_bytes = limit,
                    "Harbor STORE: rejected - harbor quota exceeded"
                );
                return Err(HarborError::HarborQuotaExceeded {
                    harbor_id,
                    current,
                    limit,
                });
            }
            Err(e) => {
                warn!(
                    packet_id = %packet_id_hex,
                    error = %e,
                    "Harbor STORE: storage check failed"
                );
                return Err(HarborError::Database(e));
            }
        }

        // Validate request - basic checks
        if request.packet_data.is_empty() {
            warn!(
                packet_id = %packet_id_hex,
                "Harbor STORE: rejected - empty packet data"
            );
            return Ok(StoreResponse {
                packet_id: request.packet_id,
                success: false,
                error: Some("empty packet data".to_string()),
            });
        }

        if request.recipients.is_empty() {
            warn!(
                packet_id = %packet_id_hex,
                "Harbor STORE: rejected - no recipients"
            );
            return Ok(StoreResponse {
                packet_id: request.packet_id,
                success: false,
                error: Some("no recipients".to_string()),
            });
        }

        // Cryptographic verification: format, consistency, and SIGNATURE
        // This prevents:
        // - Garbage packets (format check)
        // - Spoofed sender IDs (signature check)
        // - Misrouted packets (harbor_id check)
        match verify_for_storage(&request.packet_data, &request.harbor_id, &request.sender_id) {
            Ok(packet) => {
                // Verify packet_id matches
                if packet.packet_id != request.packet_id {
                    warn!(
                        packet_id = %packet_id_hex,
                        actual_id = %hex::encode(packet.packet_id),
                        "Harbor STORE: rejected - packet_id mismatch"
                    );
                    return Ok(StoreResponse {
                        packet_id: request.packet_id,
                        success: false,
                        error: Some("packet_id mismatch".to_string()),
                    });
                }
                debug!(
                    packet_id = %packet_id_hex,
                    "Harbor STORE: packet verified (format + signature)"
                );
            }
            Err(e) => {
                let error_msg = match &e {
                    PacketError::InvalidFormat(msg) => format!("invalid format: {}", msg),
                    PacketError::InvalidSignature => "invalid signature".to_string(),
                    PacketError::HarborIdMismatch => "harbor_id mismatch".to_string(),
                    PacketError::SenderIdMismatch => "sender_id mismatch".to_string(),
                    PacketError::PayloadTooLarge { size, max } => {
                        format!("payload too large: {} > {}", size, max)
                    }
                    _ => e.to_string(),
                };
                warn!(
                    packet_id = %packet_id_hex,
                    error = %error_msg,
                    "Harbor STORE: rejected - verification failed"
                );
                return Ok(StoreResponse {
                    packet_id: request.packet_id,
                    success: false,
                    error: Some(error_msg),
                });
            }
        }

        // Convert packet type
        let packet_type = harbor_packet_type_to_data(request.packet_type);

        // Store the packet (now verified!)
        cache_packet(
            conn,
            &request.packet_id,
            &request.harbor_id,
            &request.sender_id,
            &request.packet_data,
            packet_type,
            &request.recipients,
            false, // Not synced, this is an original
        )
        .map_err(|e| {
            warn!(packet_id = %packet_id_hex, error = %e, "Harbor STORE: database error");
            HarborError::Database(e)
        })?;

        // Log recipient IDs for debugging
        let recipient_hexes: Vec<String> = request
            .recipients
            .iter()
            .map(|r| hex::encode(&r[..8]))
            .collect();
        info!(
            packet_id = %packet_id_hex,
            harbor_id = %harbor_id_hex,
            recipients = ?recipient_hexes,
            "Harbor STORE: SUCCESS - stored verified packet for {} recipients",
            request.recipients.len()
        );

        Ok(StoreResponse {
            packet_id: request.packet_id,
            success: true,
            error: None,
        })
    }

    /// Handle a pull request
    ///
    /// Called when a client wants to retrieve missed packets.
    /// The `authenticated_node_id` is the verified NodeId from the iroh connection
    /// (obtained via `connection.remote_id()`).
    pub fn handle_pull(
        &self,
        conn: &Connection,
        request: PullRequest,
        authenticated_node_id: &[u8; 32],
    ) -> Result<PullResponse, HarborError> {
        let harbor_id_hex = hex::encode(request.harbor_id);
        let recipient_hex = hex::encode(request.recipient_id);
        let auth_hex = hex::encode(authenticated_node_id);

        info!(
            harbor_id = %harbor_id_hex,
            recipient = %recipient_hex,
            already_have = request.already_have.len(),
            "Harbor PULL: received pull request"
        );

        // Check rate limit
        self.check_connection_rate_limit(authenticated_node_id)?;

        // Verify the claimed recipient_id matches the authenticated connection
        if &request.recipient_id != authenticated_node_id {
            warn!(
                harbor_id = %harbor_id_hex,
                claimed = %recipient_hex,
                actual = %auth_hex,
                "Harbor PULL: UNAUTHORIZED - identity mismatch"
            );
            return Err(HarborError::Unauthorized);
        }

        let packets = get_packets_for_recipient(
            conn,
            &request.harbor_id,
            &request.recipient_id,
            request.since_timestamp,
        )
        .map_err(HarborError::Database)?;

        let total_available = packets.len();

        // Filter out packets the client already has
        let already_have: std::collections::HashSet<[u8; 32]> =
            request.already_have.into_iter().collect();

        let packets: Vec<PacketInfo> = packets
            .into_iter()
            .filter(|p| !already_have.contains(&p.packet_id))
            .map(|p| PacketInfo {
                packet_id: p.packet_id,
                sender_id: p.sender_id,
                packet_data: p.packet_data,
                created_at: p.created_at,
                packet_type: data_packet_type_to_harbor(p.packet_type),
            })
            .collect();

        info!(
            harbor_id = %harbor_id_hex,
            recipient = %recipient_hex,
            available = total_available,
            returning = packets.len(),
            "Harbor PULL: SUCCESS - returning {} packets (filtered {} already-have)",
            packets.len(),
            total_available - packets.len()
        );

        Ok(PullResponse {
            harbor_id: request.harbor_id,
            packets,
        })
    }

    /// Handle a delivery acknowledgment
    ///
    /// Called when a client confirms they received a packet.
    /// The `authenticated_node_id` is the verified NodeId from the iroh connection
    /// (obtained via `connection.remote_id()`).
    pub fn handle_ack(
        &self,
        conn: &Connection,
        ack: DeliveryAck,
        authenticated_node_id: &[u8; 32],
    ) -> Result<(), HarborError> {
        let packet_id_hex = hex::encode(ack.packet_id);
        let recipient_hex = hex::encode(ack.recipient_id);
        let auth_hex = hex::encode(authenticated_node_id);

        debug!(
            packet_id = %packet_id_hex,
            recipient = %recipient_hex,
            "Harbor ACK: received delivery acknowledgment"
        );

        // Check rate limit
        self.check_connection_rate_limit(authenticated_node_id)?;

        // Verify the claimed recipient_id matches the authenticated connection
        if &ack.recipient_id != authenticated_node_id {
            warn!(
                packet_id = %packet_id_hex,
                claimed = %recipient_hex,
                actual = %auth_hex,
                "Harbor ACK: UNAUTHORIZED - identity mismatch"
            );
            return Err(HarborError::Unauthorized);
        }

        mark_delivered(conn, &ack.packet_id, &ack.recipient_id).map_err(HarborError::Database)?;

        // Check if all recipients got the packet
        if all_recipients_delivered(conn, &ack.packet_id).map_err(HarborError::Database)? {
            // Clean up - packet is no longer needed
            delete_packet(conn, &ack.packet_id).map_err(HarborError::Database)?;
            info!(
                packet_id = %packet_id_hex,
                "Harbor ACK: all recipients received - packet deleted"
            );
        } else {
            debug!(
                packet_id = %packet_id_hex,
                recipient = %recipient_hex,
                "Harbor ACK: marked delivered (other recipients remaining)"
            );
        }

        Ok(())
    }

    /// Handle a sync request from another Harbor Node
    pub fn handle_sync(
        &self,
        conn: &Connection,
        request: SyncRequest,
        authenticated_node_id: &[u8; 32],
    ) -> Result<SyncResponse, HarborError> {
        let harbor_id_hex = hex::encode(request.harbor_id);

        info!(
            harbor_id = %harbor_id_hex,
            their_packets = request.have_packets.len(),
            their_delivery_updates = request.delivery_updates.len(),
            their_members = request.member_entries.len(),
            "Harbor SYNC: received sync request from peer"
        );

        // Check rate limit
        self.check_connection_rate_limit(authenticated_node_id)?;

        // Get our packets for this harbor_id
        let our_packets =
            get_packets_for_sync(conn, &request.harbor_id).map_err(HarborError::Database)?;

        let our_packet_count = our_packets.len();

        // Find packets they don't have
        let have_set: std::collections::HashSet<[u8; 32]> =
            request.have_packets.into_iter().collect();

        let missing_packets: Vec<PacketInfo> = our_packets
            .into_iter()
            .filter(|p| !have_set.contains(&p.packet_id))
            .map(|p| PacketInfo {
                packet_id: p.packet_id,
                sender_id: p.sender_id,
                packet_data: p.packet_data,
                created_at: p.created_at,
                packet_type: data_packet_type_to_harbor(p.packet_type),
            })
            .collect();

        // Apply their delivery updates
        let mut delivery_updates_applied = 0;
        for update in &request.delivery_updates {
            for recipient in &update.delivered_to {
                let _ = mark_delivered(conn, &update.packet_id, recipient);
                delivery_updates_applied += 1;
            }
        }

        // Get our delivery updates for them
        let our_delivery_updates = self.get_delivery_updates(conn, &request.harbor_id)?;

        info!(
            harbor_id = %harbor_id_hex,
            we_have = our_packet_count,
            sending_missing = missing_packets.len(),
            applied_their_updates = delivery_updates_applied,
            sending_our_updates = our_delivery_updates.len(),
            "Harbor SYNC: SUCCESS - sending {} missing packets, {} delivery updates",
            missing_packets.len(),
            our_delivery_updates.len()
        );

        Ok(SyncResponse {
            harbor_id: request.harbor_id,
            missing_packets,
            delivery_updates: our_delivery_updates,
            members: vec![], // Member tracking removed for Tier 1
        })
    }

    /// Get delivery updates to share with other Harbor Nodes
    ///
    /// Returns a list of packets and which recipients have received them.
    /// Other Harbor Nodes can use this to mark deliveries on their side,
    /// enabling distributed delivery tracking.
    pub(super) fn get_delivery_updates(
        &self,
        conn: &Connection,
        harbor_id: &[u8; 32],
    ) -> Result<Vec<DeliveryUpdate>, HarborError> {
        let packets = get_packets_for_sync(conn, harbor_id).map_err(HarborError::Database)?;

        let mut updates = Vec::new();
        for packet in packets {
            let delivered = crate::data::harbor::get_delivered_recipients(conn, &packet.packet_id)
                .map_err(HarborError::Database)?;

            // Only include packets that have some deliveries to share
            if !delivered.is_empty() {
                updates.push(DeliveryUpdate {
                    packet_id: packet.packet_id,
                    delivered_to: delivered,
                });
            }
        }

        Ok(updates)
    }

    /// Process incoming harbor message and return response
    ///
    /// The `authenticated_node_id` is the verified NodeId from the iroh connection
    /// (obtained via `connection.remote_id()`). This is used to verify that
    /// Pull and Ack requests come from the claimed recipient.
    pub fn handle_message(
        &self,
        conn: &mut Connection,
        message: HarborMessage,
        authenticated_node_id: &[u8; 32],
    ) -> Result<Option<HarborMessage>, HarborError> {
        match message {
            HarborMessage::Store(req) => {
                let resp = self.handle_store(conn, req)?;
                Ok(Some(HarborMessage::StoreResponse(resp)))
            }
            HarborMessage::Pull(req) => {
                let resp = self.handle_pull(conn, req, authenticated_node_id)?;
                Ok(Some(HarborMessage::PullResponse(resp)))
            }
            HarborMessage::Ack(ack) => {
                self.handle_ack(conn, ack, authenticated_node_id)?;
                Ok(None) // No response needed
            }
            HarborMessage::SyncRequest(req) => {
                let resp = self.handle_sync(conn, req, authenticated_node_id)?;
                Ok(Some(HarborMessage::SyncResponse(resp)))
            }
            HarborMessage::StoreResponse(_)
            | HarborMessage::PullResponse(_)
            | HarborMessage::SyncResponse(_) => {
                // These are responses, not requests - shouldn't be handled here
                Ok(None)
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::harbor::{cache_packet, get_cached_packet, get_undelivered_recipients};
    use crate::data::schema::create_harbor_table;
    use crate::security::create_key_pair::generate_key_pair;
    use crate::security::send::packet::PacketBuilder;
    use crate::security::topic_keys::harbor_id_from_topic;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_harbor_table(&conn).unwrap();
        conn
    }

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_handle_store() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Create a valid packet with proper cryptographic signatures
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let sender_keypair = generate_key_pair();

        let packet = PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .expect("packet build should succeed");

        let packet_bytes = packet.to_bytes().expect("serialization should succeed");

        let request = StoreRequest {
            packet_data: packet_bytes,
            packet_id: packet.packet_id,
            harbor_id,
            sender_id: sender_keypair.public_key,
            recipients: vec![test_id(40), test_id(41)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None, // PoW disabled in without_rate_limiting()
        };

        let response = service.handle_store(&mut conn, request).unwrap();
        assert!(response.success, "store should succeed: {:?}", response.error);

        // Verify packet was stored
        let cached = get_cached_packet(&conn, &packet.packet_id).unwrap();
        assert!(cached.is_some());
    }

    #[test]
    fn test_handle_store_empty_packet() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        let request = StoreRequest {
            packet_data: vec![],
            packet_id: test_id(10),
            harbor_id: test_id(20),
            sender_id: test_id(30),
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };

        let response = service.handle_store(&mut conn, request).unwrap();
        assert!(!response.success);
    }

    #[test]
    fn test_handle_pull() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store some packets
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet 1",
            PacketType::Content,
            &[test_id(1)],
            false,
        )
        .unwrap();

        cache_packet(
            &mut conn,
            &test_id(11),
            &test_id(20),
            &test_id(31),
            b"packet 2",
            PacketType::Content,
            &[test_id(1)],
            false,
        )
        .unwrap();

        let request = PullRequest {
            harbor_id: test_id(20),
            recipient_id: test_id(1),
            since_timestamp: 0,
            already_have: vec![],
            relay_url: None,
        };

        // Pass authenticated node ID (same as recipient_id)
        let response = service.handle_pull(&conn, request, &test_id(1)).unwrap();
        assert_eq!(response.packets.len(), 2);
    }

    #[test]
    fn test_handle_pull_unauthorized() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet",
            PacketType::Content,
            &[test_id(1)],
            false,
        )
        .unwrap();

        let request = PullRequest {
            harbor_id: test_id(20),
            recipient_id: test_id(1), // Claims to be test_id(1)
            since_timestamp: 0,
            already_have: vec![],
            relay_url: None,
        };

        // But authenticated as test_id(99) - should fail
        let result = service.handle_pull(&conn, request, &test_id(99));
        assert!(matches!(result, Err(HarborError::Unauthorized)));
    }

    #[test]
    fn test_handle_pull_with_already_have() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet 1",
            PacketType::Content,
            &[test_id(1)],
            false,
        )
        .unwrap();

        cache_packet(
            &mut conn,
            &test_id(11),
            &test_id(20),
            &test_id(31),
            b"packet 2",
            PacketType::Content,
            &[test_id(1)],
            false,
        )
        .unwrap();

        let request = PullRequest {
            harbor_id: test_id(20),
            recipient_id: test_id(1),
            since_timestamp: 0,
            already_have: vec![test_id(10)], // Already have packet 10
            relay_url: None,
        };

        let response = service.handle_pull(&conn, request, &test_id(1)).unwrap();
        assert_eq!(response.packets.len(), 1);
        assert_eq!(response.packets[0].packet_id, test_id(11));
    }

    #[test]
    fn test_handle_ack() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet with one recipient
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet",
            PacketType::Content,
            &[test_id(1)],
            false,
        )
        .unwrap();

        // Ack it
        let ack = DeliveryAck {
            packet_id: test_id(10),
            recipient_id: test_id(1),
        };

        // Pass authenticated node ID (same as recipient_id)
        service.handle_ack(&conn, ack, &test_id(1)).unwrap();

        // Packet should be deleted (all recipients received)
        let cached = get_cached_packet(&conn, &test_id(10)).unwrap();
        assert!(cached.is_none());
    }

    #[test]
    fn test_handle_ack_unauthorized() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet",
            PacketType::Content,
            &[test_id(1)],
            false,
        )
        .unwrap();

        let ack = DeliveryAck {
            packet_id: test_id(10),
            recipient_id: test_id(1), // Claims to be test_id(1)
        };

        // But authenticated as test_id(99) - should fail
        let result = service.handle_ack(&conn, ack, &test_id(99));
        assert!(matches!(result, Err(HarborError::Unauthorized)));
    }

    #[test]
    fn test_handle_ack_partial() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet with two recipients
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet",
            PacketType::Content,
            &[test_id(1), test_id(2)],
            false,
        )
        .unwrap();

        // Ack from only one recipient
        let ack = DeliveryAck {
            packet_id: test_id(10),
            recipient_id: test_id(1),
        };

        service.handle_ack(&conn, ack, &test_id(1)).unwrap();

        // Packet should still exist (one recipient remaining)
        let cached = get_cached_packet(&conn, &test_id(10)).unwrap();
        assert!(cached.is_some());
    }

    #[test]
    fn test_handle_sync_basic() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet on this node
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20), // harbor_id
            &test_id(30),
            b"packet data",
            PacketType::Content,
            &[test_id(40)],
            false,
        )
        .unwrap();

        // Another node syncs with us, doesn't have our packet
        let peer_id = test_id(99); // Simulated peer doing the sync
        let request = SyncRequest {
            harbor_id: test_id(20),
            have_packets: vec![], // They have nothing
            delivery_updates: vec![],
            member_entries: vec![],
        };

        let response = service.handle_sync(&conn, request, &peer_id).unwrap();

        // They should receive our packet
        assert_eq!(response.missing_packets.len(), 1);
        assert_eq!(response.missing_packets[0].packet_id, test_id(10));
    }

    #[test]
    fn test_handle_sync_no_missing() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet data",
            PacketType::Content,
            &[test_id(40)],
            false,
        )
        .unwrap();

        // They already have our packet
        let peer_id = test_id(99);
        let request = SyncRequest {
            harbor_id: test_id(20),
            have_packets: vec![test_id(10)], // They already have it
            delivery_updates: vec![],
            member_entries: vec![],
        };

        let response = service.handle_sync(&conn, request, &peer_id).unwrap();

        // No missing packets
        assert!(response.missing_packets.is_empty());
    }

    #[test]
    fn test_handle_sync_with_delivery_updates() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet with recipients
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet data",
            PacketType::Content,
            &[test_id(40), test_id(41)],
            false,
        )
        .unwrap();

        // They tell us test_id(40) received the packet
        let peer_id = test_id(99);
        let request = SyncRequest {
            harbor_id: test_id(20),
            have_packets: vec![test_id(10)],
            delivery_updates: vec![DeliveryUpdate {
                packet_id: test_id(10),
                delivered_to: vec![test_id(40)],
            }],
            member_entries: vec![],
        };

        let _response = service.handle_sync(&conn, request, &peer_id).unwrap();

        // Check that delivery was marked
        let undelivered = get_undelivered_recipients(&conn, &test_id(10)).unwrap();
        assert_eq!(undelivered.len(), 1);
        assert_eq!(undelivered[0], test_id(41)); // Only 41 remaining
    }

    #[test]
    fn test_handle_message_routing() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Test store request (authenticated_node_id not checked for Store)
        let store_msg = HarborMessage::Store(StoreRequest {
            packet_data: b"data".to_vec(),
            packet_id: test_id(10),
            harbor_id: test_id(20),
            sender_id: test_id(30),
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        });

        // Authenticated node ID is test_id(40)
        let response = service
            .handle_message(&mut conn, store_msg, &test_id(40))
            .unwrap();
        assert!(matches!(response, Some(HarborMessage::StoreResponse(_))));

        // Test pull request - authenticated as test_id(40) which matches recipient_id
        let pull_msg = HarborMessage::Pull(PullRequest {
            harbor_id: test_id(20),
            recipient_id: test_id(40),
            since_timestamp: 0,
            already_have: vec![],
            relay_url: None,
        });

        let response = service
            .handle_message(&mut conn, pull_msg, &test_id(40))
            .unwrap();
        assert!(matches!(response, Some(HarborMessage::PullResponse(_))));
    }

    #[test]
    fn test_handle_message_pull_unauthorized() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet first
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet",
            PacketType::Content,
            &[test_id(40)],
            false,
        )
        .unwrap();

        // Try to pull as test_id(40) but authenticated as test_id(99)
        let pull_msg = HarborMessage::Pull(PullRequest {
            harbor_id: test_id(20),
            recipient_id: test_id(40), // Claims to be 40
            since_timestamp: 0,
            already_have: vec![],
            relay_url: None,
        });

        let result = service.handle_message(&mut conn, pull_msg, &test_id(99)); // But authenticated as 99
        assert!(matches!(result, Err(HarborError::Unauthorized)));
    }

    #[test]
    fn test_handle_sync_message_routing() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet",
            PacketType::Content,
            &[test_id(40)],
            false,
        )
        .unwrap();

        let sync_msg = HarborMessage::SyncRequest(SyncRequest {
            harbor_id: test_id(20),
            have_packets: vec![],
            delivery_updates: vec![],
            member_entries: vec![],
        });

        // Sync requests don't need authentication (any Harbor Node can sync)
        let response = service
            .handle_message(&mut conn, sync_msg, &test_id(99))
            .unwrap();
        assert!(matches!(response, Some(HarborMessage::SyncResponse(_))));

        if let Some(HarborMessage::SyncResponse(resp)) = response {
            assert_eq!(resp.missing_packets.len(), 1);
        }
    }

    #[test]
    fn test_get_delivery_updates() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet with two recipients
        cache_packet(
            &mut conn,
            &test_id(10),
            &test_id(20), // harbor_id
            &test_id(30),
            b"packet data",
            PacketType::Content,
            &[test_id(40), test_id(41)],
            false,
        )
        .unwrap();

        // Initially no delivery updates (nothing delivered yet)
        let updates = service.get_delivery_updates(&conn, &test_id(20)).unwrap();
        assert!(updates.is_empty());

        // Mark one as delivered
        mark_delivered(&conn, &test_id(10), &test_id(40)).unwrap();

        // Now we should have a delivery update
        let updates = service.get_delivery_updates(&conn, &test_id(20)).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].packet_id, test_id(10));
        assert_eq!(updates[0].delivered_to.len(), 1);
        assert!(updates[0].delivered_to.contains(&test_id(40)));
    }
}
