//! Harbor client-side operations
//!
//! Handles operations when acting as a client to Harbor Nodes:
//! - Creating store requests (replicating packets to Harbor Nodes)
//! - Creating pull requests (retrieving missed packets)
//! - Processing pull responses
//! - Creating acknowledgments
//! - Creating and applying sync requests/responses (Harbor-to-Harbor)
//! - Finding Harbor Nodes via DHT
//! - Packet processing (decrypt + dispatch)
//! - Maintenance (cleanup)

use std::sync::Arc;

use rusqlite::Connection;
use tracing::{debug, info, trace, warn};

use crate::data::harbor::{
    WILDCARD_RECIPIENT, cache_packet, cleanup_expired, get_active_harbor_ids, get_cached_packet,
    get_packets_for_sync, get_undelivered_recipients, mark_pulled, was_pulled,
};
use crate::network::dht::DhtService;
use crate::network::packet::PacketType;
use crate::network::process::{ProcessContext, ProcessError, ProcessScope, process_packet};
use crate::network::stream::StreamService;
use crate::resilience::ProofOfWork;
use crate::security::{
    EpochKeys, PacketError, PacketId, SendPacket, VerificationMode, verify_and_decrypt_dm_packet,
    verify_and_decrypt_with_epoch,
};

use super::protocol::{
    DeliveryAck, PacketInfo, PullRequest, PullResponse, StoreRequest, SyncRequest, SyncResponse,
};
use super::service::{HarborError, HarborService};

fn validate_pulled_packet_sender(
    claimed_sender: &[u8; 32],
    signed_sender: &[u8; 32],
) -> Result<(), ProcessError> {
    if claimed_sender == signed_sender {
        return Ok(());
    }

    Err(ProcessError::Decode(format!(
        "pull sender mismatch: claimed {}, signed {}",
        hex::encode(&claimed_sender[..8]),
        hex::encode(&signed_sender[..8]),
    )))
}

fn verify_and_decrypt_topic_for_pull(
    send_packet: &SendPacket,
    topic_id: &[u8; 32],
    epoch_keys: &EpochKeys,
) -> Result<Vec<u8>, ProcessError> {
    match verify_and_decrypt_with_epoch(send_packet, topic_id, epoch_keys, VerificationMode::Full) {
        Ok(plaintext) => Ok(plaintext),
        Err(full_err) => {
            // Only allow MacOnly fallback when Full failed due signature issues.
            if !matches!(full_err, PacketError::InvalidSignature) {
                return Err(ProcessError::Decode(format!(
                    "topic decryption failed: {}",
                    full_err
                )));
            }

            let plaintext = verify_and_decrypt_with_epoch(
                send_packet,
                topic_id,
                epoch_keys,
                VerificationMode::MacOnly,
            )
            .map_err(|e| ProcessError::Decode(format!("topic decryption failed: {}", e)))?;

            let packet_type = plaintext
                .first()
                .copied()
                .and_then(PacketType::from_byte)
                .ok_or_else(|| {
                    ProcessError::Decode("topic decryption failed: unknown packet type".into())
                })?;

            if packet_type != PacketType::TopicJoin {
                return Err(ProcessError::Decode(format!(
                    "topic decryption failed: {}",
                    full_err
                )));
            }

            Ok(plaintext)
        }
    }
}

impl HarborService {
    // ============ Client Side ============

    /// Create a store request for a packet
    ///
    /// Called when we need to replicate a packet to Harbor Nodes.
    /// The PoW should be computed with context: `harbor_id || packet_id`
    pub fn create_store_request(
        &self,
        packet_id: PacketId,
        harbor_id: [u8; 32],
        packet_data: Vec<u8>,
        undelivered_recipients: Vec<[u8; 32]>,
        pow: ProofOfWork,
    ) -> StoreRequest {
        trace!(
            packet_id = %hex::encode(packet_id),
            harbor_id = %hex::encode(harbor_id),
            recipients = undelivered_recipients.len(),
            size = packet_data.len(),
            "Harbor CLIENT: creating store request"
        );

        StoreRequest {
            packet_data,
            packet_id,
            harbor_id,
            sender_id: self.endpoint_id,
            recipients: undelivered_recipients,
            pow,
        }
    }

    /// Create a pull request
    ///
    /// Called when we want to retrieve missed packets from Harbor Nodes.
    pub fn create_pull_request(
        &self,
        conn: &Connection,
        harbor_id: [u8; 32],
        since_timestamp: i64,
        topic_id: &[u8; 32],
    ) -> Result<PullRequest, HarborError> {
        self.create_pull_request_with_relay(conn, harbor_id, since_timestamp, topic_id, None)
    }

    /// Create a pull request with optional relay URL
    ///
    /// The relay URL is used by Harbor Nodes to track member connectivity info.
    pub fn create_pull_request_with_relay(
        &self,
        conn: &Connection,
        harbor_id: [u8; 32],
        since_timestamp: i64,
        topic_id: &[u8; 32],
        relay_url: Option<String>,
    ) -> Result<PullRequest, HarborError> {
        // Get packet IDs we already pulled
        let already_have = crate::data::harbor::get_pulled_packet_ids(conn, topic_id)
            .map_err(HarborError::Database)?;

        trace!(
            harbor_id = %hex::encode(harbor_id),
            topic_id = %hex::encode(topic_id),
            already_have = already_have.len(),
            relay_url = ?relay_url,
            "Harbor CLIENT: creating pull request"
        );

        Ok(PullRequest {
            harbor_id,
            recipient_id: self.endpoint_id,
            since_timestamp,
            already_have,
            relay_url,
        })
    }

    /// Process a pull response
    ///
    /// Records packets as pulled and returns them for processing.
    pub fn process_pull_response(
        &self,
        conn: &Connection,
        topic_id: &[u8; 32],
        response: PullResponse,
    ) -> Result<Vec<PacketInfo>, HarborError> {
        let topic_hex = hex::encode(topic_id);
        let harbor_hex = hex::encode(response.harbor_id);

        info!(
            topic_id = %topic_hex,
            harbor_id = %harbor_hex,
            packets_received = response.packets.len(),
            "Harbor CLIENT: processing pull response"
        );

        let mut new_packets = 0;
        let mut already_had = 0;

        // Mark each packet as pulled
        for packet in &response.packets {
            // Skip if already pulled
            if was_pulled(conn, topic_id, &packet.packet_id).map_err(HarborError::Database)? {
                already_had += 1;
                continue;
            }
            mark_pulled(conn, topic_id, &packet.packet_id).map_err(HarborError::Database)?;
            new_packets += 1;
        }

        info!(
            topic_id = %topic_hex,
            new_packets = new_packets,
            already_had = already_had,
            "Harbor CLIENT: processed pull response - {} new, {} already had",
            new_packets,
            already_had
        );

        Ok(response.packets)
    }

    /// Create an acknowledgment for a packet
    pub fn create_ack(&self, packet_id: PacketId) -> DeliveryAck {
        trace!(
            packet_id = %hex::encode(packet_id),
            "Harbor CLIENT: creating delivery ack"
        );
        DeliveryAck {
            packet_id,
            recipient_id: self.endpoint_id,
        }
    }

    // ============ Packet Processing ============

    /// Process a pulled packet using unified packet dispatch
    ///
    /// This is the main entry point for processing packets pulled from Harbor.
    /// It handles:
    /// 1. Parsing the SendPacket from raw bytes
    /// 2. Decryption based on scope (DM key vs topic key)
    /// 3. Unified dispatch via process_packet()
    ///
    /// # Arguments
    /// * `packet_info` - The packet info from the pull response
    /// * `scope` - Whether this is a DM or Topic pull (determines decryption key)
    /// * `stream_service` - Stream service for handling stream signaling
    ///
    /// # Returns
    /// Ok(()) if processing succeeded, or ProcessError if it failed
    pub async fn process_pulled_packet(
        &self,
        packet_info: &PacketInfo,
        scope: ProcessScope,
        stream_service: &Arc<StreamService>,
    ) -> Result<(), ProcessError> {
        // Parse the SendPacket
        let send_packet = SendPacket::from_bytes(&packet_info.packet_data)
            .map_err(|e| ProcessError::Decode(format!("failed to parse SendPacket: {}", e)))?;

        // Bind packet metadata sender to signed packet sender identity.
        validate_pulled_packet_sender(&packet_info.sender_id, &send_packet.endpoint_id)?;

        // Decrypt based on scope
        let plaintext = match &scope {
            ProcessScope::Dm => {
                // DM: use our private key
                let private_key = {
                    let db_lock = self.db().lock().await;
                    crate::data::identity::get_identity(&db_lock)
                        .map_err(|e| ProcessError::Database(e.to_string()))?
                        .ok_or_else(|| ProcessError::Database("no identity found".into()))?
                        .private_key
                };

                verify_and_decrypt_dm_packet(&send_packet, &private_key)
                    .map_err(|e| ProcessError::Decode(format!("DM decryption failed: {}", e)))?
            }
            ProcessScope::Topic { topic_id } => {
                // Topic: get epoch key and try Full, then MacOnly
                let epoch_key_opt = {
                    let db_lock = self.db().lock().await;
                    crate::data::get_epoch_key(&db_lock, topic_id, send_packet.epoch)
                        .ok()
                        .flatten()
                };

                let stored_key = epoch_key_opt.ok_or_else(|| {
                    ProcessError::Decode(format!(
                        "no epoch key for topic {} epoch {}",
                        hex::encode(&topic_id[..8]),
                        send_packet.epoch
                    ))
                })?;

                let epoch_keys =
                    EpochKeys::derive_from_secret(send_packet.epoch, &stored_key.key_data);

                verify_and_decrypt_topic_for_pull(&send_packet, topic_id, &epoch_keys)?
            }
        };

        // Build ProcessContext
        let ctx = ProcessContext {
            db: self.db().clone(),
            event_tx: self.event_tx().clone(),
            our_id: self.endpoint_id,
            stream_service: stream_service.clone(),
            scope,
        };

        // Dispatch to unified processor
        process_packet(
            &plaintext,
            packet_info.sender_id,
            packet_info.created_at,
            &ctx,
        )
        .await
    }

    // ============ Maintenance ============

    /// Get HarborIDs we're actively serving as a Harbor Node
    pub fn get_active_harbor_ids(&self, conn: &Connection) -> Result<Vec<[u8; 32]>, HarborError> {
        let ids = get_active_harbor_ids(conn).map_err(HarborError::Database)?;
        trace!(
            count = ids.len(),
            "Harbor: found {} active HarborIDs",
            ids.len()
        );
        Ok(ids)
    }

    /// Create sync requests for our active Harbor duties
    pub fn create_sync_requests(
        &self,
        conn: &Connection,
    ) -> Result<Vec<(/* harbor_id */ [u8; 32], SyncRequest)>, HarborError> {
        let harbor_ids = self.get_active_harbor_ids(conn)?;
        let mut requests = Vec::new();

        debug!(
            harbor_ids = harbor_ids.len(),
            "Harbor: creating sync requests for {} HarborIDs",
            harbor_ids.len()
        );

        for harbor_id in harbor_ids {
            let packets = get_packets_for_sync(conn, &harbor_id).map_err(HarborError::Database)?;

            let have_packets: Vec<PacketId> = packets.iter().map(|p| p.packet_id).collect();
            let delivery_updates = self.get_delivery_updates(conn, &harbor_id)?;

            trace!(
                harbor_id = %hex::encode(harbor_id),
                packets = have_packets.len(),
                "Harbor: sync request for HarborID has {} packets",
                have_packets.len()
            );

            requests.push((
                harbor_id,
                SyncRequest {
                    harbor_id,
                    have_packets,
                    delivery_updates,
                    member_entries: vec![], // Member sync disabled in current Harbor protocol.
                },
            ));
        }

        Ok(requests)
    }

    /// Apply a sync response
    pub fn apply_sync_response(
        &self,
        conn: &mut Connection,
        response: SyncResponse,
    ) -> Result<(), HarborError> {
        let harbor_hex = hex::encode(response.harbor_id);
        let missing_count = response.missing_packets.len();
        let update_count = response.delivery_updates.len();

        debug!(
            harbor_id = %harbor_hex,
            missing_packets = missing_count,
            delivery_updates = update_count,
            "Harbor: applying sync response"
        );

        // Store missing packets (as synced)
        let mut packets_stored = 0;
        let mut packets_defaulted = 0;
        for packet in &response.missing_packets {
            // Get recipients from existing packet if we have it
            let recipients = if let Some(existing) =
                get_cached_packet(conn, &packet.packet_id).map_err(HarborError::Database)?
            {
                get_undelivered_recipients(conn, &existing.packet_id)
                    .map_err(HarborError::Database)?
            } else {
                // For packets we don't have locally, rebuild a safe recipient fallback.
                // DM packets target exactly one recipient (harbor_id). Topic packets use
                // wildcard recipient because per-recipient tracking is not in sync payload.
                match SendPacket::from_bytes(&packet.packet_data) {
                    Ok(parsed) => {
                        if parsed.harbor_id != response.harbor_id {
                            warn!(
                                harbor_id = %harbor_hex,
                                packet_id = %hex::encode(packet.packet_id),
                                packet_harbor_id = %hex::encode(parsed.harbor_id),
                                "Harbor: skipping synced packet with harbor_id mismatch"
                            );
                            continue;
                        }
                        packets_defaulted += 1;
                        if parsed.is_dm() {
                            vec![parsed.harbor_id]
                        } else {
                            vec![WILDCARD_RECIPIENT]
                        }
                    }
                    Err(e) => {
                        // Legacy/compat path: keep packet available for pull and let
                        // downstream parse/decrypt fail if bytes are malformed.
                        warn!(
                            harbor_id = %harbor_hex,
                            packet_id = %hex::encode(packet.packet_id),
                            error = %e,
                            "Harbor: synced packet is not a valid SendPacket; defaulting recipients to wildcard"
                        );
                        packets_defaulted += 1;
                        vec![WILDCARD_RECIPIENT]
                    }
                }
            };

            trace!(
                packet_id = %hex::encode(packet.packet_id),
                "Harbor: storing synced packet"
            );

            cache_packet(
                conn,
                &packet.packet_id,
                &response.harbor_id,
                &packet.sender_id,
                &packet.packet_data,
                &recipients,
                true, // Synced from another Harbor Node
            )
            .map_err(HarborError::Database)?;
            packets_stored += 1;
        }

        // Apply delivery updates
        let mut updates_applied = 0;
        for update in &response.delivery_updates {
            for recipient in &update.delivered_to {
                if crate::data::harbor::mark_delivered(conn, &update.packet_id, recipient)
                    .map_err(HarborError::Database)?
                {
                    updates_applied += 1;
                }
            }
        }

        info!(
            harbor_id = %harbor_hex,
            missing_packets = missing_count,
            packets_stored = packets_stored,
            packets_with_default_recipients = packets_defaulted,
            deliveries_marked = updates_applied,
            "Harbor: applied sync response - stored {} packets ({} defaulted), marked {} deliveries",
            packets_stored,
            packets_defaulted,
            updates_applied
        );

        Ok(())
    }

    /// Cleanup expired packets
    pub fn cleanup(&self, conn: &Connection) -> Result<usize, HarborError> {
        let deleted = cleanup_expired(conn).map_err(HarborError::Database)?;
        if deleted > 0 {
            info!(
                deleted = deleted,
                "Harbor: cleaned up {} expired packets", deleted
            );
        }
        Ok(deleted)
    }

    // ============ Harbor Node Discovery ============

    /// Find Harbor Nodes for a given HarborID from cache
    ///
    /// Returns cached nodes from the harbor_nodes_cache table.
    /// The cache is populated by the background discovery task.
    pub fn find_harbor_nodes(conn: &rusqlite::Connection, harbor_id: &[u8; 32]) -> Vec<[u8; 32]> {
        crate::data::harbor::get_cached_harbor_nodes(conn, harbor_id).unwrap_or_default()
    }

    /// Find Harbor Nodes for a given HarborID using DHT lookup
    ///
    /// This performs an actual DHT lookup. Used by the background discovery task
    /// to refresh the cache. Other code should use `find_harbor_nodes` which reads from cache.
    pub async fn find_harbor_nodes_dht(
        dht_service: &Option<Arc<DhtService>>,
        harbor_id: &[u8; 32],
    ) -> Vec<[u8; 32]> {
        let Some(client) = dht_service else {
            debug!("DHT client not available, cannot find Harbor Nodes");
            return vec![];
        };

        use crate::network::dht::Id as DhtId;
        let target = DhtId::new(*harbor_id);
        let initial: Option<Vec<[u8; 32]>> = None;
        match client.lookup(target, initial).await {
            Ok(nodes) => {
                debug!(
                    harbor_id = hex::encode(harbor_id),
                    count = nodes.len(),
                    "found Harbor Nodes via DHT"
                );
                nodes
            }
            Err(e) => {
                debug!(error = %e, "DHT lookup failed");
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::DeliveryUpdate;
    use super::*;
    use crate::data::dht::current_timestamp;
    use crate::data::harbor::{
        cache_packet, get_cached_packet, get_packets_for_recipient, mark_delivered,
    };
    use crate::data::schema::{create_harbor_table, create_peer_table};
    use crate::network::packet::PacketType;
    use crate::resilience::ProofOfWork;
    use crate::security::create_key_pair::generate_key_pair;
    use crate::security::send::packet::{PacketBuilder, create_dm_packet, generate_packet_id};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_harbor_table(&conn).unwrap();
        conn
    }

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn test_packet_id(seed: u8) -> PacketId {
        [seed; 16]
    }

    /// Create a dummy PoW for tests (difficulty 0 = always passes)
    fn test_pow() -> ProofOfWork {
        ProofOfWork {
            timestamp: 0,
            nonce: 0,
            difficulty_bits: 0,
        }
    }

    #[test]
    fn test_create_store_request() {
        let service = HarborService::without_rate_limiting(test_id(1));

        let request = service.create_store_request(
            test_packet_id(10),
            test_id(20),
            b"packet data".to_vec(),
            vec![test_id(40), test_id(41)],
            test_pow(),
        );

        assert_eq!(request.packet_id, test_packet_id(10));
        assert_eq!(request.harbor_id, test_id(20));
        assert_eq!(request.sender_id, test_id(1));
        assert_eq!(request.recipients.len(), 2);
    }

    #[test]
    fn test_create_pull_request() {
        let conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        let topic_id = test_id(100);
        let request = service
            .create_pull_request(&conn, test_id(20), 0, &topic_id)
            .unwrap();

        assert_eq!(request.harbor_id, test_id(20));
        assert_eq!(request.recipient_id, test_id(1));
        assert!(request.already_have.is_empty());
    }

    #[test]
    fn test_process_pull_response() {
        let conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        let topic_id = test_id(100);
        let response = PullResponse {
            harbor_id: test_id(20),
            packets: vec![PacketInfo {
                packet_id: test_packet_id(10),
                sender_id: test_id(30),
                packet_data: b"packet".to_vec(),
                created_at: current_timestamp(),
            }],
        };

        let packets = service
            .process_pull_response(&conn, &topic_id, response)
            .unwrap();
        assert_eq!(packets.len(), 1);

        // Should be marked as pulled
        assert!(was_pulled(&conn, &topic_id, &test_packet_id(10)).unwrap());
    }

    #[test]
    fn test_create_ack() {
        let service = HarborService::without_rate_limiting(test_id(1));
        let ack = service.create_ack(test_packet_id(10));

        assert_eq!(ack.packet_id, test_packet_id(10));
        assert_eq!(ack.recipient_id, test_id(1));
    }

    #[test]
    fn test_get_active_harbor_ids_empty() {
        let conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        let ids = service.get_active_harbor_ids(&conn).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_create_sync_requests() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store packets for two different harbor_ids
        cache_packet(
            &mut conn,
            &test_packet_id(10),
            &test_id(20), // harbor_id 1
            &test_id(30),
            b"packet 1",
            &[test_id(40)],
            false,
        )
        .unwrap();

        cache_packet(
            &mut conn,
            &test_packet_id(11),
            &test_id(21), // harbor_id 2
            &test_id(31),
            b"packet 2",
            &[test_id(41)],
            false,
        )
        .unwrap();

        let requests = service.create_sync_requests(&conn).unwrap();

        // Should have sync requests for 2 harbor_ids
        assert_eq!(requests.len(), 2);

        // Each should have 1 packet in have_packets
        for (_harbor_id, request) in &requests {
            assert_eq!(request.have_packets.len(), 1);
        }
    }

    #[test]
    fn test_create_sync_requests_empty() {
        let conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // No packets stored
        let requests = service.create_sync_requests(&conn).unwrap();
        assert!(requests.is_empty());
    }

    #[test]
    fn test_apply_sync_response() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Receive a sync response with a packet we don't have
        let response = SyncResponse {
            harbor_id: test_id(20),
            missing_packets: vec![PacketInfo {
                packet_id: test_packet_id(10),
                sender_id: test_id(30),
                packet_data: b"synced packet".to_vec(),
                created_at: current_timestamp(),
            }],
            delivery_updates: vec![],
            members: vec![],
        };

        service.apply_sync_response(&mut conn, response).unwrap();

        // Packet should now be cached
        let cached = get_cached_packet(&conn, &test_packet_id(10)).unwrap();
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().packet_data, b"synced packet");
    }

    #[test]
    fn test_apply_sync_response_with_delivery_updates() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // First store a packet
        cache_packet(
            &mut conn,
            &test_packet_id(10),
            &test_id(20),
            &test_id(30),
            b"packet",
            &[test_id(40), test_id(41)],
            false,
        )
        .unwrap();

        // Receive sync response with delivery update
        let response = SyncResponse {
            harbor_id: test_id(20),
            missing_packets: vec![],
            delivery_updates: vec![DeliveryUpdate {
                packet_id: test_packet_id(10),
                delivered_to: vec![test_id(40)],
            }],
            members: vec![],
        };

        service.apply_sync_response(&mut conn, response).unwrap();

        // Delivery should be marked
        let undelivered = get_undelivered_recipients(&conn, &test_packet_id(10)).unwrap();
        assert_eq!(undelivered.len(), 1);
        assert_eq!(undelivered[0], test_id(41));
    }

    #[test]
    fn test_sync_roundtrip() {
        // Simulate two Harbor Nodes syncing
        let mut conn1 = setup_db();
        let mut conn2 = setup_db();
        let service1 = HarborService::without_rate_limiting(test_id(1));
        let service2 = HarborService::without_rate_limiting(test_id(2));

        let harbor_id = test_id(20);

        // Node 1 has packet A
        cache_packet(
            &mut conn1,
            &test_packet_id(10),
            &harbor_id,
            &test_id(30),
            b"packet A",
            &[test_id(40)],
            false,
        )
        .unwrap();

        // Node 2 has packet B
        cache_packet(
            &mut conn2,
            &test_packet_id(11),
            &harbor_id,
            &test_id(31),
            b"packet B",
            &[test_id(41)],
            false,
        )
        .unwrap();

        // Node 1 creates sync request
        let requests1 = service1.create_sync_requests(&conn1).unwrap();
        assert_eq!(requests1.len(), 1);
        let (_, sync_request) = &requests1[0];

        // Node 2 handles the sync request (using test_id(1) as authenticated node)
        let response = service2
            .handle_sync(&conn2, sync_request.clone(), &test_id(1))
            .unwrap();

        // Response should include packet B (which node 1 doesn't have)
        assert_eq!(response.missing_packets.len(), 1);
        assert_eq!(response.missing_packets[0].packet_id, test_packet_id(11));

        // Node 1 applies the response
        service1.apply_sync_response(&mut conn1, response).unwrap();

        // Node 1 should now have both packets
        assert!(
            get_cached_packet(&conn1, &test_packet_id(10))
                .unwrap()
                .is_some()
        );
        assert!(
            get_cached_packet(&conn1, &test_packet_id(11))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_cleanup() {
        let conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // No expired packets - should return 0
        let deleted = service.cleanup(&conn).unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_sync_includes_delivery_updates() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // Store a packet and mark partial delivery
        cache_packet(
            &mut conn,
            &test_packet_id(10),
            &test_id(20),
            &test_id(30),
            b"packet data",
            &[test_id(40), test_id(41)],
            false,
        )
        .unwrap();

        mark_delivered(&conn, &test_packet_id(10), &test_id(40)).unwrap();

        // Create sync requests - should include delivery updates
        let requests = service.create_sync_requests(&conn).unwrap();
        assert_eq!(requests.len(), 1);

        let (_, sync_request) = &requests[0];
        assert_eq!(sync_request.delivery_updates.len(), 1);
        assert_eq!(
            sync_request.delivery_updates[0].packet_id,
            test_packet_id(10)
        );
        assert!(
            sync_request.delivery_updates[0]
                .delivered_to
                .contains(&test_id(40))
        );
    }

    #[test]
    fn test_validate_pulled_packet_sender_rejects_mismatch() {
        let err = validate_pulled_packet_sender(&test_id(1), &test_id(2)).unwrap_err();
        assert!(err.to_string().contains("sender mismatch"));
    }

    #[test]
    fn test_verify_and_decrypt_topic_for_pull_allows_topic_join_mac_only_fallback() {
        let topic_id = test_id(90);
        let keypair = generate_key_pair();

        let packet = PacketBuilder::new(topic_id, keypair.private_key, keypair.public_key)
            .build(&[PacketType::TopicJoin.as_byte()], generate_packet_id())
            .unwrap();

        let mut tampered = packet.clone();
        tampered.signature = [0u8; 64];

        let epoch_keys = EpochKeys::derive_from_topic(&topic_id, tampered.epoch);
        let plaintext =
            verify_and_decrypt_topic_for_pull(&tampered, &topic_id, &epoch_keys).unwrap();
        assert_eq!(plaintext[0], PacketType::TopicJoin.as_byte());
    }

    #[test]
    fn test_verify_and_decrypt_topic_for_pull_rejects_non_join_mac_only_fallback() {
        let topic_id = test_id(91);
        let keypair = generate_key_pair();

        let packet = PacketBuilder::new(topic_id, keypair.private_key, keypair.public_key)
            .build(
                &[PacketType::TopicContent.as_byte(), 1, 2, 3],
                generate_packet_id(),
            )
            .unwrap();

        let mut tampered = packet.clone();
        tampered.signature = [0u8; 64];

        let epoch_keys = EpochKeys::derive_from_topic(&topic_id, tampered.epoch);
        let err = verify_and_decrypt_topic_for_pull(&tampered, &topic_id, &epoch_keys).unwrap_err();
        assert!(err.to_string().contains("signature"));
    }

    #[test]
    fn test_apply_sync_response_new_dm_packet_defaults_recipient_to_harbor_id() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        let sender = generate_key_pair();
        let recipient = generate_key_pair();
        let dm_packet = create_dm_packet(
            &sender.private_key,
            &sender.public_key,
            &recipient.public_key,
            b"synced dm",
            test_packet_id(77),
        )
        .unwrap();

        let response = SyncResponse {
            harbor_id: recipient.public_key,
            missing_packets: vec![PacketInfo {
                packet_id: dm_packet.packet_id,
                sender_id: sender.public_key,
                packet_data: dm_packet.to_bytes().unwrap(),
                created_at: current_timestamp(),
            }],
            delivery_updates: vec![],
            members: vec![],
        };

        service.apply_sync_response(&mut conn, response).unwrap();

        let visible_to_recipient =
            get_packets_for_recipient(&conn, &recipient.public_key, &recipient.public_key, 0)
                .unwrap();
        assert_eq!(visible_to_recipient.len(), 1);

        let visible_to_other =
            get_packets_for_recipient(&conn, &recipient.public_key, &test_id(200), 0).unwrap();
        assert!(visible_to_other.is_empty());
    }
}
