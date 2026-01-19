//! Harbor Node service
//!
//! Provides the logic for acting as a Harbor Node (storing/serving packets)
//! and as a client (storing/pulling packets from Harbor Nodes).
//!
//! See root README for `RUST_LOG` configuration.

use std::sync::Mutex;
use std::time::Duration;

use rusqlite::Connection;
use tracing::{debug, info, trace, warn};

use crate::data::harbor::{
    cache_packet, get_cached_packet, get_packets_for_recipient, get_packets_for_sync,
    mark_delivered, get_undelivered_recipients, all_recipients_delivered, delete_packet,
    get_active_harbor_ids, mark_pulled, was_pulled, cleanup_expired, PacketType,
};
use crate::resilience::{RateLimiter, RateLimitConfig, RateLimitResult};
use crate::resilience::{PoWConfig, verify_pow, PoWVerifyResult};
use crate::resilience::{StorageManager, StorageConfig, StorageCheckResult};
use crate::security::send::packet::{verify_for_storage, PacketError};

use super::protocol::{
    DeliveryAck, DeliveryUpdate, HarborMessage, HarborPacketType, PacketInfo, PullRequest, PullResponse,
    StoreRequest, StoreResponse, SyncRequest, SyncResponse,
};

/// Harbor Node service
///
/// Handles both server-side (acting as Harbor Node) and client-side
/// (storing to / pulling from Harbor Nodes) operations.
///
/// Includes abuse protection:
/// - Rate limiting (per-connection and per-HarborID limits)
/// - Proof of Work (requires computational work before storing)
/// - Storage limits (total and per-HarborID quotas)
pub struct HarborService {
    /// Local node's EndpointID
    endpoint_id: [u8; 32],
    /// Rate limiter for abuse prevention (thread-safe via Mutex)
    rate_limiter: Mutex<RateLimiter>,
    /// Proof of Work configuration
    pow_config: PoWConfig,
    /// Storage quota manager
    storage_manager: StorageManager,
}

impl HarborService {
    /// Create a new Harbor service with default configs (platform-appropriate storage)
    pub fn new(endpoint_id: [u8; 32]) -> Self {
        Self::with_full_config(
            endpoint_id,
            RateLimitConfig::default(),
            PoWConfig::default(),
            StorageConfig::default(),
        )
    }

    /// Create a new Harbor service with custom rate limit and PoW configs
    pub fn with_config(
        endpoint_id: [u8; 32],
        rate_config: RateLimitConfig,
        pow_config: PoWConfig,
    ) -> Self {
        Self::with_full_config(endpoint_id, rate_config, pow_config, StorageConfig::default())
    }

    /// Create a new Harbor service with all custom configs
    pub fn with_full_config(
        endpoint_id: [u8; 32],
        rate_config: RateLimitConfig,
        pow_config: PoWConfig,
        storage_config: StorageConfig,
    ) -> Self {
        Self {
            endpoint_id,
            rate_limiter: Mutex::new(RateLimiter::with_config(rate_config)),
            pow_config,
            storage_manager: StorageManager::new(storage_config),
        }
    }

    /// Create a new Harbor service with custom rate limit config (default PoW and storage)
    pub fn with_rate_limit_config(endpoint_id: [u8; 32], config: RateLimitConfig) -> Self {
        Self::with_config(endpoint_id, config, PoWConfig::default())
    }

    /// Create a new Harbor service with all abuse protection disabled (for testing)
    pub fn without_rate_limiting(endpoint_id: [u8; 32]) -> Self {
        Self::with_full_config(
            endpoint_id,
            RateLimitConfig { enabled: false, ..Default::default() },
            PoWConfig::disabled(),
            StorageConfig::disabled(),
        )
    }

    /// Get the PoW difficulty bits (for clients to know what difficulty to use)
    pub fn pow_difficulty(&self) -> u8 {
        self.pow_config.difficulty_bits
    }

    /// Check if PoW is required
    pub fn pow_enabled(&self) -> bool {
        self.pow_config.enabled
    }

    /// Get the storage configuration
    pub fn storage_config(&self) -> &StorageConfig {
        self.storage_manager.config()
    }

    /// Check rate limit for a connection
    fn check_connection_rate_limit(&self, endpoint_id: &[u8; 32]) -> Result<(), HarborError> {
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
    fn check_store_rate_limit(&self, endpoint_id: &[u8; 32], harbor_id: &[u8; 32]) -> Result<(), HarborError> {
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
                    warn!(packet_id = %packet_id_hex, "Harbor STORE: rejected - PoW required but not provided");
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
            Ok(StorageCheckResult::HarborLimitExceeded { harbor_id, current, limit }) => {
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
            warn!(packet_id = %packet_id_hex, "Harbor STORE: rejected - empty packet data");
            return Ok(StoreResponse {
                packet_id: request.packet_id,
                success: false,
                error: Some("empty packet data".to_string()),
            });
        }

        if request.recipients.is_empty() {
            warn!(packet_id = %packet_id_hex, "Harbor STORE: rejected - no recipients");
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
        let recipient_hexes: Vec<String> = request.recipients.iter()
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

        mark_delivered(conn, &ack.packet_id, &ack.recipient_id)
            .map_err(HarborError::Database)?;

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
        let our_packets = get_packets_for_sync(conn, &request.harbor_id)
            .map_err(HarborError::Database)?;

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
    fn get_delivery_updates(
        &self,
        conn: &Connection,
        harbor_id: &[u8; 32],
    ) -> Result<Vec<DeliveryUpdate>, HarborError> {
        let packets = get_packets_for_sync(conn, harbor_id)
            .map_err(HarborError::Database)?;

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
            mark_pulled(conn, topic_id, &packet.packet_id)
                .map_err(HarborError::Database)?;
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
            let packets = get_packets_for_sync(conn, &harbor_id)
                .map_err(HarborError::Database)?;

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
                let _ = mark_delivered(conn, &update.packet_id, recipient);
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
            info!(deleted = deleted, "Harbor: cleaned up {} expired packets", deleted);
        }
        Ok(deleted)
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
            HarborMessage::StoreResponse(_) |
            HarborMessage::PullResponse(_) |
            HarborMessage::SyncResponse(_) => {
                // These are responses, not requests - shouldn't be handled here
                Ok(None)
            }
        }
    }
}

// ============ Type Conversion Helpers ============

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

/// Harbor service error
#[derive(Debug)]
pub enum HarborError {
    /// Database error
    Database(rusqlite::Error),
    /// Protocol error
    Protocol(String),
    /// Unauthorized - claimed identity doesn't match authenticated connection
    Unauthorized,
    /// Rate limited - too many requests
    RateLimited {
        /// Time to wait before retrying
        retry_after: Duration,
    },
    /// Proof of Work required but not provided
    PoWRequired,
    /// Invalid Proof of Work
    InvalidPoW(PoWVerifyResult),
    /// Total storage limit exceeded
    StorageFull {
        /// Current total bytes stored
        current: u64,
        /// Maximum allowed bytes
        limit: u64,
    },
    /// Per-HarborID storage quota exceeded
    HarborQuotaExceeded {
        /// The HarborID that exceeded quota
        harbor_id: [u8; 32],
        /// Current bytes stored for this HarborID
        current: u64,
        /// Maximum allowed bytes per HarborID
        limit: u64,
    },
}

impl std::fmt::Display for HarborError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarborError::Database(e) => write!(f, "database error: {}", e),
            HarborError::Protocol(e) => write!(f, "protocol error: {}", e),
            HarborError::Unauthorized => write!(f, "unauthorized: claimed identity doesn't match connection"),
            HarborError::RateLimited { retry_after } => {
                write!(f, "rate limited: retry after {}ms", retry_after.as_millis())
            }
            HarborError::PoWRequired => write!(f, "proof of work required"),
            HarborError::InvalidPoW(result) => write!(f, "invalid proof of work: {}", result),
            HarborError::StorageFull { current, limit } => {
                write!(f, "storage full: {} bytes used, {} bytes limit", current, limit)
            }
            HarborError::HarborQuotaExceeded { harbor_id, current, limit } => {
                write!(
                    f,
                    "harbor quota exceeded for {}: {} bytes used, {} bytes limit",
                    hex::encode(&harbor_id[..8]),
                    current,
                    limit
                )
            }
        }
    }
}

impl std::error::Error for HarborError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::dht::current_timestamp;
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
            packets: vec![
                PacketInfo {
                    packet_id: test_id(10),
                    sender_id: test_id(30),
                    packet_data: b"packet".to_vec(),
                    created_at: current_timestamp(),
                    packet_type: HarborPacketType::Content,
                },
            ],
        };

        let packets = service.process_pull_response(&conn, &topic_id, response).unwrap();
        assert_eq!(packets.len(), 1);

        // Should be marked as pulled
        assert!(was_pulled(&conn, &topic_id, &test_id(10)).unwrap());
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
        let response = service.handle_message(&mut conn, store_msg, &test_id(40)).unwrap();
        assert!(matches!(response, Some(HarborMessage::StoreResponse(_))));

        // Test pull request - authenticated as test_id(40) which matches recipient_id
        let pull_msg = HarborMessage::Pull(PullRequest {
            harbor_id: test_id(20),
            recipient_id: test_id(40),
            since_timestamp: 0,
            already_have: vec![],
            relay_url: None,
        });

        let response = service.handle_message(&mut conn, pull_msg, &test_id(40)).unwrap();
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

    // ============ Sync Tests ============

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
            delivery_updates: vec![
                DeliveryUpdate {
                    packet_id: test_id(10),
                    delivered_to: vec![test_id(40)],
                },
            ],
            member_entries: vec![],
        };

        let _response = service.handle_sync(&conn, request, &peer_id).unwrap();

        // Check that delivery was marked
        let undelivered = get_undelivered_recipients(&conn, &test_id(10)).unwrap();
        assert_eq!(undelivered.len(), 1);
        assert_eq!(undelivered[0], test_id(41)); // Only 41 remaining
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
            missing_packets: vec![
                PacketInfo {
                    packet_id: test_id(10),
                    sender_id: test_id(30),
                    packet_data: b"synced packet".to_vec(),
                    created_at: current_timestamp(),
                    packet_type: HarborPacketType::Content,
                },
            ],
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
            delivery_updates: vec![
                DeliveryUpdate {
                    packet_id: test_id(10),
                    delivered_to: vec![test_id(40)],
                },
            ],
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
        let response = service2.handle_sync(&conn2, sync_request.clone(), &test_id(1)).unwrap();
        
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
        let response = service.handle_message(&mut conn, sync_msg, &test_id(99)).unwrap();
        assert!(matches!(response, Some(HarborMessage::SyncResponse(_))));
        
        if let Some(HarborMessage::SyncResponse(resp)) = response {
            assert_eq!(resp.missing_packets.len(), 1);
        }
    }

    #[test]
    fn test_harbor_error_display() {
        let db_err = HarborError::Database(rusqlite::Error::InvalidQuery);
        assert!(db_err.to_string().contains("database error"));

        let proto_err = HarborError::Protocol("test error".to_string());
        assert_eq!(proto_err.to_string(), "protocol error: test error");

        let unauth_err = HarborError::Unauthorized;
        assert!(unauth_err.to_string().contains("unauthorized"));

        let rate_err = HarborError::RateLimited { retry_after: Duration::from_millis(1000) };
        assert!(rate_err.to_string().contains("rate limited"));
        assert!(rate_err.to_string().contains("1000ms"));

        let pow_required = HarborError::PoWRequired;
        assert!(pow_required.to_string().contains("proof of work required"));

        let pow_invalid = HarborError::InvalidPoW(PoWVerifyResult::Expired);
        assert!(pow_invalid.to_string().contains("invalid proof of work"));
        assert!(pow_invalid.to_string().contains("expired"));

        let storage_full = HarborError::StorageFull {
            current: 100,
            limit: 50,
        };
        assert!(storage_full.to_string().contains("storage full"));
        assert!(storage_full.to_string().contains("100"));
        assert!(storage_full.to_string().contains("50"));

        let quota_exceeded = HarborError::HarborQuotaExceeded {
            harbor_id: test_id(42),
            current: 200,
            limit: 100,
        };
        assert!(quota_exceeded.to_string().contains("harbor quota exceeded"));
        assert!(quota_exceeded.to_string().contains("2a2a2a2a")); // hex of [42; 32][..8]
        assert!(quota_exceeded.to_string().contains("200"));
    }

    #[test]
    fn test_rate_limiting_blocks_excessive_requests() {
        use crate::resilience::RateLimitConfig;
        
        let mut conn = setup_db();
        // Very strict rate limit: 2 requests per window
        let rate_config = RateLimitConfig {
            enabled: true,
            max_requests_per_connection: 2,
            max_stores_per_harbor_id: 100, // High so connection limit hits first
            window_duration: Duration::from_secs(60),
        };
        // Disable PoW so we can test rate limiting in isolation
        let pow_config = PoWConfig::disabled();
        let service = HarborService::with_config(test_id(1), rate_config, pow_config);

        let sender_keypair = crate::security::create_key_pair::generate_key_pair();
        let topic_id = test_id(20);
        
        // Create a valid packet for the store request
        let packet = crate::security::send::packet::PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();
        
        let packet_bytes = packet.to_bytes().unwrap();
        let harbor_id = crate::security::topic_keys::harbor_id_from_topic(&topic_id);

        // First 2 requests should succeed
        for i in 0..2 {
            let request = StoreRequest {
                packet_id: test_id(100 + i),
                harbor_id,
                sender_id: sender_keypair.public_key,
                packet_data: packet_bytes.clone(),
                recipients: vec![test_id(40)],
                packet_type: HarborPacketType::Content,
                proof_of_work: None, // PoW disabled in this test
            };
            let result = service.handle_store(&mut conn, request);
            assert!(result.is_ok(), "Request {} should succeed", i);
        }

        // Third request should be rate limited
        let request = StoreRequest {
            packet_id: test_id(102),
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes,
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request);
        assert!(matches!(result, Err(HarborError::RateLimited { .. })));
    }

    #[test]
    fn test_rate_limiting_disabled() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        let sender_keypair = crate::security::create_key_pair::generate_key_pair();
        let topic_id = test_id(20);
        
        let packet = crate::security::send::packet::PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();
        
        let packet_bytes = packet.to_bytes().unwrap();
        let harbor_id = crate::security::topic_keys::harbor_id_from_topic(&topic_id);

        // Many requests should all succeed when rate limiting is disabled
        for i in 0..100 {
            let request = StoreRequest {
                packet_id: test_id(100 + i),
                harbor_id,
                sender_id: sender_keypair.public_key,
                packet_data: packet_bytes.clone(),
                recipients: vec![test_id(40)],
                packet_type: HarborPacketType::Content,
                proof_of_work: None,
            };
            let result = service.handle_store(&mut conn, request);
            assert!(result.is_ok(), "Request {} should succeed with rate limiting disabled", i);
        }
    }

    #[test]
    fn test_create_ack() {
        let service = HarborService::without_rate_limiting(test_id(1));
        let ack = service.create_ack(test_id(10));
        
        assert_eq!(ack.packet_id, test_id(10));
        assert_eq!(ack.recipient_id, test_id(1));
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
    fn test_get_active_harbor_ids_empty() {
        let conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));
        
        let ids = service.get_active_harbor_ids(&conn).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_handle_store_no_recipients() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        let request = StoreRequest {
            packet_data: b"data".to_vec(),
            packet_id: test_id(10),
            harbor_id: test_id(20),
            sender_id: test_id(30),
            recipients: vec![], // No recipients
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };

        let response = service.handle_store(&mut conn, request).unwrap();
        assert!(!response.success);
        assert!(response.error.as_ref().unwrap().contains("no recipients"));
    }

    #[test]
    fn test_packet_type_conversion() {
        // Protocol -> Data
        assert_eq!(
            harbor_packet_type_to_data(HarborPacketType::Content),
            PacketType::Content
        );
        assert_eq!(
            harbor_packet_type_to_data(HarborPacketType::Join),
            PacketType::Join
        );
        assert_eq!(
            harbor_packet_type_to_data(HarborPacketType::Leave),
            PacketType::Leave
        );

        // Data -> Protocol
        assert_eq!(
            data_packet_type_to_harbor(PacketType::Content),
            HarborPacketType::Content
        );
        assert_eq!(
            data_packet_type_to_harbor(PacketType::Join),
            HarborPacketType::Join
        );
        assert_eq!(
            data_packet_type_to_harbor(PacketType::Leave),
            HarborPacketType::Leave
        );
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
        assert!(sync_request.delivery_updates[0].delivered_to.contains(&test_id(40)));
    }

    #[test]
    fn test_storage_limits_block_when_full() {
        use crate::resilience::StorageConfig;

        let mut conn = setup_db();
        
        let sender_keypair = crate::security::create_key_pair::generate_key_pair();
        let topic_id = test_id(20);

        let packet = crate::security::send::packet::PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();

        let packet_bytes = packet.to_bytes().unwrap();
        let packet_size = packet_bytes.len() as u64;
        let harbor_id = crate::security::topic_keys::harbor_id_from_topic(&topic_id);

        // Set limit to allow exactly 1 packet but not 2
        let storage_config = StorageConfig {
            max_total_bytes: packet_size + 10, // Just enough for 1 packet
            max_per_harbor_bytes: packet_size * 10, // High so total limit hits first
            enabled: true,
        };
        let service = HarborService::with_full_config(
            test_id(1),
            RateLimitConfig { enabled: false, ..Default::default() },
            PoWConfig::disabled(),
            storage_config,
        );

        // First request should succeed
        let request1 = StoreRequest {
            packet_id: packet.packet_id, // Use actual packet ID
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes.clone(),
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request1);
        let response = result.expect("First request should not error");
        assert!(response.success, "First store should succeed: {:?}", response.error);

        // Second request should fail - storage full
        // Need a new packet with different packet_id
        let packet2 = crate::security::send::packet::PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"second message")
        .unwrap();
        let packet_bytes2 = packet2.to_bytes().unwrap();

        let request2 = StoreRequest {
            packet_id: packet2.packet_id,
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes2,
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request2);
        assert!(
            matches!(result, Err(HarborError::StorageFull { .. })),
            "Second request should fail with StorageFull, got: {:?}",
            result
        );
    }

    #[test]
    fn test_storage_per_harbor_limit() {
        use crate::resilience::StorageConfig;

        let mut conn = setup_db();
        
        let sender_keypair = crate::security::create_key_pair::generate_key_pair();
        let topic_id = test_id(20);

        let packet = crate::security::send::packet::PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();

        let packet_bytes = packet.to_bytes().unwrap();
        let packet_size = packet_bytes.len() as u64;
        let harbor_id = crate::security::topic_keys::harbor_id_from_topic(&topic_id);

        // Set per-harbor limit to allow exactly 1 packet but not 2
        let storage_config = StorageConfig {
            max_total_bytes: packet_size * 100, // High so per-harbor limit hits first
            max_per_harbor_bytes: packet_size + 10, // Just enough for 1 packet
            enabled: true,
        };
        let service = HarborService::with_full_config(
            test_id(1),
            RateLimitConfig { enabled: false, ..Default::default() },
            PoWConfig::disabled(),
            storage_config,
        );

        // First request to harbor_id should succeed
        let request1 = StoreRequest {
            packet_id: packet.packet_id, // Use actual packet ID
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes.clone(),
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request1);
        let response = result.expect("First request should not error");
        assert!(response.success, "First store should succeed: {:?}", response.error);

        // Second request to same harbor should fail - per-harbor quota exceeded
        let packet2 = crate::security::send::packet::PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"second message")
        .unwrap();
        let packet_bytes2 = packet2.to_bytes().unwrap();

        let request2 = StoreRequest {
            packet_id: packet2.packet_id,
            harbor_id,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes2,
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request2);
        assert!(
            matches!(result, Err(HarborError::HarborQuotaExceeded { .. })),
            "Second request should fail with HarborQuotaExceeded, got: {:?}",
            result
        );

        // Request to DIFFERENT harbor should succeed (different quota)
        let topic_id2 = test_id(21);
        let packet3 = crate::security::send::packet::PacketBuilder::new(
            topic_id2,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"third message")
        .unwrap();
        let packet_bytes3 = packet3.to_bytes().unwrap();
        let harbor_id2 = crate::security::topic_keys::harbor_id_from_topic(&topic_id2);

        let request3 = StoreRequest {
            packet_id: packet3.packet_id,
            harbor_id: harbor_id2,
            sender_id: sender_keypair.public_key,
            packet_data: packet_bytes3,
            recipients: vec![test_id(40)],
            packet_type: HarborPacketType::Content,
            proof_of_work: None,
        };
        let result = service.handle_store(&mut conn, request3);
        let response = result.expect("Third request should not error");
        assert!(response.success, "Request to different harbor should succeed: {:?}", response.error);
    }

    #[test]
    fn test_storage_disabled() {
        let mut conn = setup_db();
        let service = HarborService::without_rate_limiting(test_id(1));

        // without_rate_limiting uses StorageConfig::disabled()
        assert!(!service.storage_config().enabled);

        let sender_keypair = crate::security::create_key_pair::generate_key_pair();
        let topic_id = test_id(20);

        let packet = crate::security::send::packet::PacketBuilder::new(
            topic_id,
            sender_keypair.private_key,
            sender_keypair.public_key,
        )
        .build(b"test message")
        .unwrap();

        let packet_bytes = packet.to_bytes().unwrap();
        let harbor_id = crate::security::topic_keys::harbor_id_from_topic(&topic_id);

        // Many requests should all succeed when storage limits are disabled
        for i in 0..10 {
            let request = StoreRequest {
                packet_id: test_id(100 + i),
                harbor_id,
                sender_id: sender_keypair.public_key,
                packet_data: packet_bytes.clone(),
                recipients: vec![test_id(40)],
                packet_type: HarborPacketType::Content,
                proof_of_work: None,
            };
            let result = service.handle_store(&mut conn, request);
            assert!(result.is_ok(), "Request {} should succeed with storage disabled", i);
        }
    }
}

