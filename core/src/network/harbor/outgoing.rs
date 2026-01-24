//! Outgoing Harbor operations (client side)
//!
//! Handles operations when acting as a client to Harbor Nodes:
//! - Creating store requests (replicating packets to Harbor Nodes)
//! - Creating pull requests (retrieving missed packets)
//! - Processing pull responses
//! - Creating acknowledgments
//! - Creating and applying sync requests/responses (Harbor-to-Harbor)
//! - Maintenance (cleanup)

use rusqlite::Connection;
use tracing::{debug, info, trace};

use crate::data::harbor::{
    cache_packet, cleanup_expired, get_active_harbor_ids, get_cached_packet, get_packets_for_sync,
    get_undelivered_recipients, mark_pulled, was_pulled, PacketType,
};

use super::protocol::{
    DeliveryAck, HarborPacketType, PacketInfo, PullRequest, PullResponse, StoreRequest,
    SyncRequest, SyncResponse,
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

impl HarborService {
    // ============ Client Side ============

    /// Create a store request for a packet
    ///
    /// Called when we need to replicate a packet to Harbor Nodes.
    pub fn create_store_request(
        &self,
        packet_id: [u8; 32],
        harbor_id: [u8; 32],
        packet_data: Vec<u8>,
        undelivered_recipients: Vec<[u8; 32]>,
        packet_type: HarborPacketType,
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
            packet_type,
            proof_of_work: None, // Client must set this before sending
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
    pub fn create_ack(&self, packet_id: [u8; 32]) -> DeliveryAck {
        trace!(
            packet_id = %hex::encode(packet_id),
            "Harbor CLIENT: creating delivery ack"
        );
        DeliveryAck {
            packet_id,
            recipient_id: self.endpoint_id,
        }
    }

    // ============ Maintenance ============

    /// Get HarborIDs we're actively serving as a Harbor Node
    pub fn get_active_harbor_ids(&self, conn: &Connection) -> Result<Vec<[u8; 32]>, HarborError> {
        let ids = get_active_harbor_ids(conn).map_err(HarborError::Database)?;
        trace!(count = ids.len(), "Harbor: found {} active HarborIDs", ids.len());
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
            let packets =
                get_packets_for_sync(conn, &harbor_id).map_err(HarborError::Database)?;

            let have_packets: Vec<[u8; 32]> = packets.iter().map(|p| p.packet_id).collect();
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
                    member_entries: vec![], // Member tracking removed for Tier 1
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
        for packet in &response.missing_packets {
            // Get recipients from existing packet if we have it
            let recipients = if let Some(existing) =
                get_cached_packet(conn, &packet.packet_id).map_err(HarborError::Database)?
            {
                get_undelivered_recipients(conn, &existing.packet_id)
                    .map_err(HarborError::Database)?
            } else {
                // No existing packet - store with empty recipients
                // (delivery tracking comes from the sender)
                vec![]
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
                harbor_packet_type_to_data(packet.packet_type),
                &recipients,
                true, // Synced from another Harbor Node
            )
            .map_err(HarborError::Database)?;
        }

        // Apply delivery updates
        let mut updates_applied = 0;
        for update in &response.delivery_updates {
            for recipient in &update.delivered_to {
                let _ = crate::data::harbor::mark_delivered(conn, &update.packet_id, recipient);
                updates_applied += 1;
            }
        }

        info!(
            harbor_id = %harbor_hex,
            packets_stored = missing_count,
            deliveries_marked = updates_applied,
            "Harbor: applied sync response - stored {} packets, marked {} deliveries",
            missing_count,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::protocol::DeliveryUpdate;
    use crate::data::dht::current_timestamp;
    use crate::data::harbor::{cache_packet, get_cached_packet, mark_delivered};
    use crate::data::schema::create_harbor_table;

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
    fn test_create_store_request() {
        let service = HarborService::without_rate_limiting(test_id(1));

        let request = service.create_store_request(
            test_id(10),
            test_id(20),
            b"packet data".to_vec(),
            vec![test_id(40), test_id(41)],
            HarborPacketType::Content,
        );

        assert_eq!(request.packet_id, test_id(10));
        assert_eq!(request.harbor_id, test_id(20));
        assert_eq!(request.sender_id, test_id(1));
        assert_eq!(request.recipients.len(), 2);
        assert_eq!(request.packet_type, HarborPacketType::Content);
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
                packet_id: test_id(10),
                sender_id: test_id(30),
                packet_data: b"packet".to_vec(),
                created_at: current_timestamp(),
                packet_type: HarborPacketType::Content,
            }],
        };

        let packets = service
            .process_pull_response(&conn, &topic_id, response)
            .unwrap();
        assert_eq!(packets.len(), 1);

        // Should be marked as pulled
        assert!(was_pulled(&conn, &topic_id, &test_id(10)).unwrap());
    }

    #[test]
    fn test_create_ack() {
        let service = HarborService::without_rate_limiting(test_id(1));
        let ack = service.create_ack(test_id(10));

        assert_eq!(ack.packet_id, test_id(10));
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
            &test_id(10),
            &test_id(20), // harbor_id 1
            &test_id(30),
            b"packet 1",
            PacketType::Content,
            &[test_id(40)],
            false,
        )
        .unwrap();

        cache_packet(
            &mut conn,
            &test_id(11),
            &test_id(21), // harbor_id 2
            &test_id(31),
            b"packet 2",
            PacketType::Content,
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
                packet_id: test_id(10),
                sender_id: test_id(30),
                packet_data: b"synced packet".to_vec(),
                created_at: current_timestamp(),
                packet_type: HarborPacketType::Content,
            }],
            delivery_updates: vec![],
            members: vec![],
        };

        service.apply_sync_response(&mut conn, response).unwrap();

        // Packet should now be cached
        let cached = get_cached_packet(&conn, &test_id(10)).unwrap();
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
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet",
            PacketType::Content,
            &[test_id(40), test_id(41)],
            false,
        )
        .unwrap();

        // Receive sync response with delivery update
        let response = SyncResponse {
            harbor_id: test_id(20),
            missing_packets: vec![],
            delivery_updates: vec![DeliveryUpdate {
                packet_id: test_id(10),
                delivered_to: vec![test_id(40)],
            }],
            members: vec![],
        };

        service.apply_sync_response(&mut conn, response).unwrap();

        // Delivery should be marked
        let undelivered = get_undelivered_recipients(&conn, &test_id(10)).unwrap();
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
            &test_id(10),
            &harbor_id,
            &test_id(30),
            b"packet A",
            PacketType::Content,
            &[test_id(40)],
            false,
        )
        .unwrap();

        // Node 2 has packet B
        cache_packet(
            &mut conn2,
            &test_id(11),
            &harbor_id,
            &test_id(31),
            b"packet B",
            PacketType::Content,
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
        assert_eq!(response.missing_packets[0].packet_id, test_id(11));

        // Node 1 applies the response
        service1.apply_sync_response(&mut conn1, response).unwrap();

        // Node 1 should now have both packets
        assert!(get_cached_packet(&conn1, &test_id(10)).unwrap().is_some());
        assert!(get_cached_packet(&conn1, &test_id(11)).unwrap().is_some());
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
            &test_id(10),
            &test_id(20),
            &test_id(30),
            b"packet data",
            PacketType::Content,
            &[test_id(40), test_id(41)],
            false,
        )
        .unwrap();

        mark_delivered(&conn, &test_id(10), &test_id(40)).unwrap();

        // Create sync requests - should include delivery updates
        let requests = service.create_sync_requests(&conn).unwrap();
        assert_eq!(requests.len(), 1);

        let (_, sync_request) = &requests[0];
        assert_eq!(sync_request.delivery_updates.len(), 1);
        assert_eq!(sync_request.delivery_updates[0].packet_id, test_id(10));
        assert!(sync_request.delivery_updates[0]
            .delivered_to
            .contains(&test_id(40)));
    }
}
