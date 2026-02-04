//! Send Service - The single entry point for all send operations
//!
//! Handles:
//! - Creating and encrypting packets
//! - Parallel delivery to multiple recipients via irpc RPC
//! - Inline receipt acknowledgement
//! - Connection management
//! - Incoming connection handling (via handle_send_connection)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use iroh::{Endpoint, EndpointId};
use iroh::endpoint::Connection;
use rusqlite::Connection as DbConnection;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, trace, warn};

use crate::data::dht::peer::{get_peer_relay_info, update_peer_relay_url, current_timestamp};
use crate::data::send::store_outgoing_packet;
use crate::data::send::acknowledge_receipt as db_acknowledge_receipt;
use crate::data::{
    get_topic_members, LocalIdentity, WILDCARD_RECIPIENT,
    add_topic_member, remove_topic_member, mark_pulled,
    get_blob, insert_blob, init_blob_sections, record_peer_can_seed, CHUNK_SIZE,
};
use crate::network::connect;
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::send::topic_messages::TopicMessage;
use crate::network::send::topic_messages::get_verification_mode_from_payload;
use crate::security::{
    SendPacket, PacketError, verify_and_decrypt_packet, verify_and_decrypt_packet_with_mode,
    VerificationMode, verify_and_decrypt_dm_packet,
};
use super::incoming::{ProcessError, ProcessResult, PacketSource};

/// Options controlling how a packet is sent and stored
#[derive(Debug, Clone)]
pub struct SendOptions {
    /// Packet type for harbor storage semantics
    pub packet_type: HarborPacketType,
    /// If true, skip storing in outgoing table (direct delivery only, no harbor replication)
    pub skip_harbor: bool,
    /// Override default harbor TTL. None = use default (PACKET_LIFETIME_SECS)
    pub ttl: Option<Duration>,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            packet_type: HarborPacketType::Content,
            skip_harbor: false,
            ttl: None,
        }
    }
}

impl SendOptions {
    /// Content packet with default harbor behavior
    pub fn content() -> Self {
        Self::default()
    }

    /// Direct-only delivery (no harbor storage)
    pub fn direct_only() -> Self {
        Self {
            packet_type: HarborPacketType::Content,
            skip_harbor: true,
            ttl: None,
        }
    }

    /// With a specific packet type
    pub fn with_packet_type(mut self, packet_type: HarborPacketType) -> Self {
        self.packet_type = packet_type;
        self
    }

    /// With a custom harbor TTL
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }
}
use crate::network::rpc::ExistingConnection;
use crate::protocol::{MemberInfo, ProtocolEvent};
use crate::security::harbor_id_from_topic;
use super::protocol::{SEND_ALPN, Receipt, DeliverTopic, DeliverDm, SendRpcProtocol};

/// Generate a random packet ID (32 bytes)
fn generate_packet_id() -> [u8; 32] {
    let mut id = [0u8; 32];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut id);
    id
}

/// Error during Send operations
#[derive(Debug)]
pub enum SendError {
    /// Failed to create packet
    PacketCreation(PacketError),
    /// Failed to connect to recipient
    Connection(String),
    /// Failed to send data
    Send(String),
    /// Database error
    Database(String),
    /// No recipients
    NoRecipients,
    /// Topic not found
    TopicNotFound,
    /// Not a member of the topic
    NotMember,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::PacketCreation(e) => write!(f, "packet creation failed: {}", e),
            SendError::Connection(e) => write!(f, "connection failed: {}", e),
            SendError::Send(e) => write!(f, "send failed: {}", e),
            SendError::Database(e) => write!(f, "database error: {}", e),
            SendError::NoRecipients => write!(f, "no recipients specified"),
            SendError::TopicNotFound => write!(f, "topic not found"),
            SendError::NotMember => write!(f, "not a member of this topic"),
        }
    }
}

impl std::error::Error for SendError {}

impl From<PacketError> for SendError {
    fn from(e: PacketError) -> Self {
        SendError::PacketCreation(e)
    }
}

/// Result of sending a packet
#[derive(Debug, Clone)]
pub struct SendResult {
    /// The packet ID
    pub packet_id: [u8; 32],
    /// Recipients that were successfully reached
    pub delivered_to: Vec<[u8; 32]>,
    /// Recipients that failed
    pub failed: Vec<([u8; 32], String)>,
}

/// Send service - single entry point for all send operations
///
/// Owns connection cache, identity, and provides both outgoing
/// (send_to_topic) and incoming (handle_send_connection) operations.
impl std::fmt::Debug for SendService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendService").finish_non_exhaustive()
    }
}

pub struct SendService {
    /// Iroh endpoint
    endpoint: Endpoint,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender (for incoming packet processing)
    event_tx: mpsc::Sender<ProtocolEvent>,
    /// Connection timeout
    send_timeout: Duration,
    /// Active connections cache
    connections: Arc<RwLock<HashMap<EndpointId, Connection>>>,
    /// Stream service for stream signaling routing (set after construction)
    stream_service: RwLock<Option<Arc<crate::network::stream::StreamService>>>,
}

impl SendService {
    /// Create a new Send service
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
    ) -> Self {
        Self {
            endpoint,
            identity,
            db,
            event_tx,
            send_timeout: Duration::from_secs(5),
            connections: Arc::new(RwLock::new(HashMap::new())),
            stream_service: RwLock::new(None),
        }
    }

    /// Set the StreamService reference (called after both services are constructed)
    pub async fn set_stream_service(&self, stream: Arc<crate::network::stream::StreamService>) {
        *self.stream_service.write().await = Some(stream);
    }

    /// Get the StreamService reference (if set)
    pub(crate) async fn stream_service(&self) -> Option<Arc<crate::network::stream::StreamService>> {
        self.stream_service.read().await.clone()
    }

    /// Get our EndpointID
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get the identity (for signing/encryption operations)
    pub fn identity(&self) -> &LocalIdentity {
        &self.identity
    }

    /// Get the event sender
    pub fn event_tx(&self) -> &mpsc::Sender<ProtocolEvent> {
        &self.event_tx
    }

    /// Get the database connection
    pub fn db(&self) -> &Arc<Mutex<DbConnection>> {
        &self.db
    }

    // ========== Outgoing ==========

    /// Send content to a topic, resolving recipients from the database
    ///
    /// Looks up topic members, validates membership, filters out self,
    /// encodes as Content message, and delivers to all recipients.
    pub async fn send_topic(
        &self,
        topic_id: &[u8; 32],
        payload: &[u8],
    ) -> Result<(), SendError> {
        let our_id = self.endpoint_id();

        // Get topic members
        let members = {
            let db = self.db.lock().await;
            get_topic_members(&db, topic_id)
                .map_err(|e| SendError::Database(e.to_string()))?
        };

        trace!(
            topic = %hex::encode(topic_id),
            member_count = members.len(),
            "sending message"
        );

        if members.is_empty() {
            return Err(SendError::TopicNotFound);
        }

        if !members.iter().any(|m| *m == our_id) {
            return Err(SendError::NotMember);
        }

        // Get recipients (all members except us)
        let recipients: Vec<MemberInfo> = members
            .into_iter()
            .filter(|m| *m != our_id)
            .map(|endpoint_id| MemberInfo::new(endpoint_id))
            .collect();

        if recipients.is_empty() {
            trace!("no recipients - only member is self");
            return Ok(());
        }

        // Encode as Content message and send
        let content_msg = super::topic_messages::TopicMessage::Content(payload.to_vec());
        let encoded_payload = content_msg.encode();

        self.send_to_topic(topic_id, &encoded_payload, &recipients, SendOptions::content())
            .await?;

        Ok(())
    }

    /// Send a direct message to a single peer (API layer)
    ///
    /// Wraps the payload as DmMessage::Content and delegates to send_to_dm.
    pub async fn send_dm(
        &self,
        recipient_id: &[u8; 32],
        payload: &[u8],
    ) -> Result<(), SendError> {
        use crate::network::send::dm_messages::DmMessage;

        let encoded = DmMessage::Content(payload.to_vec()).encode();
        self.send_to_dm(recipient_id, &encoded).await
    }

    /// Send pre-encoded DM bytes to a single peer (service layer)
    ///
    /// Used by other services to send arbitrary DM message types
    /// (e.g., FileAnnouncement, CanSeed, SyncUpdate).
    /// Callers encode the DmMessage themselves.
    ///
    /// # Flow:
    /// 1. Store RAW payload in outgoing_packets (no encryption)
    /// 2. Deliver raw payload directly via QUIC TLS (DeliverDm)
    /// 3. Harbor replication task will seal() undelivered packets later
    ///
    /// The packet is stored using the recipient's endpoint_id as harbor_id.
    pub async fn send_to_dm(
        &self,
        recipient_id: &[u8; 32],
        encoded_payload: &[u8],
    ) -> Result<(), SendError> {
        // Generate packet_id for tracking (seal() will use this when creating the sealed packet)
        let packet_id = generate_packet_id();
        let harbor_id = *recipient_id;

        // Store RAW payload in outgoing table (no encryption - seal() called during harbor replication)
        {
            let mut db = self.db.lock().await;
            store_outgoing_packet(
                &mut db,
                &packet_id,
                recipient_id, // use recipient_id as "topic_id" for DM tracking
                &harbor_id,
                encoded_payload,  // Raw payload, not encrypted
                &[*recipient_id],
                HarborPacketType::Content as u8,
            ).map_err(|e| SendError::Database(e.to_string()))?;
        }

        // Try direct delivery via DeliverDm (raw payload over QUIC TLS)
        let member = MemberInfo::new(*recipient_id);
        match self.deliver_dm_to_member(&member, encoded_payload).await {
            Ok(receipt) => {
                trace!(
                    recipient = hex::encode(recipient_id),
                    receipt_from = hex::encode(receipt.sender),
                    "DM delivered with receipt"
                );
            }
            Err(e) => {
                debug!(
                    recipient = hex::encode(recipient_id),
                    error = %e,
                    "DM direct delivery failed, will rely on harbor"
                );
            }
        }

        Ok(())
    }

    /// Send a message to topic members
    ///
    /// Lower-level entry point for all send operations.
    /// Takes pre-encoded TopicMessage bytes and delivers to all recipients.
    ///
    /// # Flow:
    /// 1. Store RAW payload in outgoing_packets (no encryption)
    /// 2. Deliver raw payload directly via QUIC TLS (DeliverTopic)
    /// 3. Harbor replication task will seal() undelivered packets later
    ///
    /// # Arguments
    /// * `topic_id` - The topic to send on
    /// * `encoded_payload` - Pre-encoded TopicMessage bytes (from TopicMessage::encode())
    /// * `recipients` - MemberInfo for topic members to send to
    /// * `options` - Send options (packet type, harbor storage, TTL)
    pub async fn send_to_topic(
        &self,
        topic_id: &[u8; 32],
        encoded_payload: &[u8],
        recipients: &[MemberInfo],
        options: SendOptions,
    ) -> Result<SendResult, SendError> {
        if recipients.is_empty() {
            return Err(SendError::NoRecipients);
        }

        // Extract endpoint IDs for storage
        let recipient_ids: Vec<[u8; 32]> = recipients.iter().map(|m| m.endpoint_id).collect();

        // Generate packet_id for tracking (seal() will use this when creating the sealed packet)
        let packet_id = generate_packet_id();
        let harbor_id = harbor_id_from_topic(topic_id);

        // Store RAW payload in outgoing table (no encryption - seal() called during harbor replication)
        if !options.skip_harbor {
            let mut db = self.db.lock().await;
            store_outgoing_packet(
                &mut db,
                &packet_id,
                topic_id,
                &harbor_id,
                encoded_payload,  // Raw payload, not encrypted
                &recipient_ids,
                options.packet_type as u8,
            ).map_err(|e| SendError::Database(e.to_string()))?;
        }

        // Filter out self and wildcard for actual delivery
        let our_id = self.identity.public_key;
        let actual_recipients: Vec<&MemberInfo> = recipients
            .iter()
            .filter(|m| m.endpoint_id != our_id && m.endpoint_id != WILDCARD_RECIPIENT)
            .collect();

        let mut delivered_to = Vec::new();
        let mut failed = Vec::new();

        // Mark self as delivered if in recipients
        if recipients.iter().any(|m| m.endpoint_id == our_id) {
            delivered_to.push(our_id);
        }

        if actual_recipients.is_empty() {
            debug!(packet_id = hex::encode(&packet_id[..8]), "no recipients (only self/wildcard)");
            return Ok(SendResult { packet_id, delivered_to, failed });
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            recipient_count = actual_recipients.len(),
            "sending to recipients in parallel (direct delivery)"
        );

        // Deliver RAW payload to all recipients in parallel via DeliverTopic
        let send_futures = actual_recipients.iter().map(|member| {
            let member = (*member).clone();
            let topic_id = *topic_id;
            let payload = encoded_payload.to_vec();
            async move {
                let result = self.deliver(&member, &topic_id, &payload).await;
                (member.endpoint_id, result)
            }
        });

        let results = join_all(send_futures).await;

        for (endpoint_id, result) in results {
            match result {
                Ok(receipt) => {
                    delivered_to.push(endpoint_id);
                    trace!(
                        recipient = hex::encode(endpoint_id),
                        receipt_from = hex::encode(receipt.sender),
                        "packet delivered with receipt"
                    );
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    failed.push((endpoint_id, err_msg.clone()));
                    debug!(
                        recipient = hex::encode(endpoint_id),
                        error = %err_msg,
                        "direct delivery failed, will rely on harbor"
                    );
                }
            }
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            delivered = delivered_to.len(),
            failed = failed.len(),
            "parallel send complete"
        );

        Ok(SendResult {
            packet_id,
            delivered_to,
            failed,
        })
    }

    // ========== Transport: irpc RPC delivery ==========

    /// Deliver raw topic payload directly to a member via DeliverTopic
    ///
    /// Uses QUIC TLS for encryption - no app-level crypto needed.
    /// This is the fast path for direct delivery to online peers.
    ///
    /// The message includes a membership proof (BLAKE3 hash) that proves
    /// we know the topic_id without revealing it on the wire.
    async fn deliver(
        &self,
        member: &MemberInfo,
        topic_id: &[u8; 32],
        payload: &[u8],
    ) -> Result<Receipt, SendError> {
        let node_id = EndpointId::from_bytes(&member.endpoint_id)
            .map_err(|e| SendError::Connection(e.to_string()))?;

        // Get or create connection (relay URL looked up from peers table)
        let conn = self.get_connection(node_id).await?;

        // Compute harbor_id and membership proof
        let harbor_id = harbor_id_from_topic(topic_id);
        let sender_id = self.endpoint_id();
        let membership_proof = super::protocol::create_membership_proof(topic_id, &harbor_id, &sender_id);

        // Send via irpc RPC - DeliverTopic request, Receipt response
        let client = irpc::Client::<SendRpcProtocol>::boxed(
            ExistingConnection::new(&conn)
        );
        let receipt = client
            .rpc(DeliverTopic {
                harbor_id,
                membership_proof,
                payload: payload.to_vec(),
            })
            .await
            .map_err(|e| SendError::Send(e.to_string()))?;

        Ok(receipt)
    }

    /// Deliver raw DM payload directly to a member via DeliverDm
    ///
    /// Uses QUIC TLS for encryption - no app-level crypto needed.
    /// This is the fast path for direct delivery to online peers.
    async fn deliver_dm_to_member(
        &self,
        member: &MemberInfo,
        payload: &[u8],
    ) -> Result<Receipt, SendError> {
        let node_id = EndpointId::from_bytes(&member.endpoint_id)
            .map_err(|e| SendError::Connection(e.to_string()))?;

        // Get or create connection (relay URL looked up from peers table)
        let conn = self.get_connection(node_id).await?;

        // Send via irpc RPC - DeliverDm request, Receipt response
        let client = irpc::Client::<SendRpcProtocol>::boxed(
            ExistingConnection::new(&conn)
        );
        let receipt = client
            .rpc(DeliverDm {
                payload: payload.to_vec(),
            })
            .await
            .map_err(|e| SendError::Send(e.to_string()))?;

        Ok(receipt)
    }

    /// Get or create a connection to a node
    async fn get_connection(&self, node_id: EndpointId) -> Result<Connection, SendError> {
        // Check cache
        {
            let connections = self.connections.read().await;
            if let Some(conn) = connections.get(&node_id) {
                if conn.close_reason().is_none() {
                    return Ok(conn.clone());
                }
            }
        }

        // Look up relay info from peers table
        let endpoint_id_bytes: [u8; 32] = *node_id.as_bytes();
        let relay_info = {
            let db = self.db.lock().await;
            get_peer_relay_info(&db, &endpoint_id_bytes).unwrap_or(None)
        };
        let (relay_url_str, relay_last_success) = match &relay_info {
            Some((url, ts)) => (Some(url.as_str()), *ts),
            None => (None, None),
        };
        let parsed_relay: Option<iroh::RelayUrl> = relay_url_str.and_then(|u| u.parse().ok());

        // Create new connection via smart connect
        let result = connect::connect(
            &self.endpoint,
            node_id,
            parsed_relay.as_ref(),
            relay_last_success,
            SEND_ALPN,
            self.send_timeout,
        )
        .await
        .map_err(|e| SendError::Connection(e.to_string()))?;

        let conn = result.connection;

        // Update relay URL timestamp if confirmed
        if result.relay_url_confirmed {
            if let Some(url) = &parsed_relay {
                let db = self.db.lock().await;
                let _ = update_peer_relay_url(&db, &endpoint_id_bytes, &url.to_string(), current_timestamp());
            }
        }

        // Cache it
        {
            let mut connections = self.connections.write().await;
            connections.insert(node_id, conn.clone());
        }

        Ok(conn)
    }

    /// Close connection to a node
    pub async fn close_connection(&self, node_id: EndpointId) {
        let mut connections = self.connections.write().await;
        if let Some(conn) = connections.remove(&node_id) {
            conn.close(0u32.into(), b"close");
        }
    }

    // ========== Incoming (Direct Delivery - Raw Payloads) ==========

    /// Process a raw topic message payload received via direct delivery (DeliverTopic)
    ///
    /// The payload is already plaintext (QUIC TLS provides encryption).
    /// No decryption or MAC/signature verification needed.
    pub async fn process_raw_topic_payload(
        &self,
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        payload: &[u8],
    ) -> Result<ProcessResult, ProcessError> {
        let db = &self.db;
        let event_tx = &self.event_tx;
        let stream = self.stream_service().await;

        info!(payload_len = payload.len(), "processing raw topic payload (direct delivery)");

        // Parse topic message (payload is already plaintext)
        let topic_msg = match TopicMessage::decode(payload) {
            Ok(msg) => {
                info!(msg_type = ?msg.message_type(), "decoded TopicMessage from raw payload");
                Some(msg)
            }
            Err(e) => {
                warn!(
                    topic = %hex::encode(&topic_id[..8]),
                    error = %e,
                    payload_len = payload.len(),
                    first_byte = ?payload.first(),
                    "failed to decode TopicMessage from raw payload"
                );
                None
            }
        };

        // Generate a packet_id for the receipt (we don't have one from a sealed packet)
        let packet_id = generate_packet_id();

        // Handle the decoded message (same logic as process_incoming_packet, but no packet)
        self.handle_topic_message(topic_id, sender_id, &topic_msg, &packet_id, db, event_tx, stream.as_ref()).await
    }

    /// Process a raw DM payload received via direct delivery (DeliverDm)
    ///
    /// The payload is already plaintext (QUIC TLS provides encryption).
    /// No decryption or signature verification needed.
    pub async fn process_raw_dm_payload(
        &self,
        sender_id: [u8; 32],
        payload: &[u8],
    ) -> Result<ProcessResult, ProcessError> {
        use crate::network::send::dm_messages::DmMessage;

        let our_id = self.endpoint_id();
        info!(payload_len = payload.len(), "processing raw DM payload (direct delivery)");

        // Decode DmMessage (payload is already plaintext)
        let dm_msg = DmMessage::decode(payload)
            .map_err(|e| ProcessError::VerificationFailed(format!("DM decode: {}", e)))?;

        // Generate a packet_id for the receipt
        let packet_id = generate_packet_id();

        // Handle the decoded DM message
        match dm_msg {
            DmMessage::Content(data) => {
                let event = crate::protocol::ProtocolEvent::DmReceived(
                    crate::protocol::DmReceivedEvent {
                        sender_id,
                        payload: data,
                        timestamp: crate::data::current_timestamp(),
                    },
                );
                let _ = self.event_tx.send(event).await;
            }
            DmMessage::SyncUpdate(data) => {
                let event = crate::protocol::ProtocolEvent::DmSyncUpdate(
                    crate::protocol::DmSyncUpdateEvent {
                        sender_id,
                        data,
                    },
                );
                let _ = self.event_tx.send(event).await;
            }
            DmMessage::SyncRequest => {
                let event = crate::protocol::ProtocolEvent::DmSyncRequest(
                    crate::protocol::DmSyncRequestEvent {
                        sender_id,
                    },
                );
                let _ = self.event_tx.send(event).await;
            }
            DmMessage::FileAnnouncement(msg) => {
                let event = crate::protocol::ProtocolEvent::DmFileAnnounced(
                    crate::protocol::DmFileAnnouncedEvent {
                        sender_id,
                        hash: msg.hash,
                        display_name: msg.display_name,
                        total_size: msg.total_size,
                        total_chunks: msg.total_chunks,
                        num_sections: msg.num_sections,
                        timestamp: crate::data::current_timestamp(),
                    },
                );
                let _ = self.event_tx.send(event).await;
            }
            // DM stream signaling — route to StreamService
            DmMessage::StreamAccept(_)
            | DmMessage::StreamReject(_)
            | DmMessage::StreamQuery(_)
            | DmMessage::StreamActive(_)
            | DmMessage::StreamEnded(_)
            | DmMessage::StreamRequest(_) => {
                if let Some(stream_svc) = self.stream_service().await {
                    stream_svc.handle_dm_signaling(&dm_msg, sender_id).await;
                }
            }
        }

        Ok(ProcessResult {
            receipt: super::protocol::Receipt::new(packet_id, our_id),
            content_payload: None,
        })
    }

    /// Helper: Handle a decoded TopicMessage
    ///
    /// Used by both `process_incoming_packet` (sealed) and `process_raw_topic_payload` (direct).
    async fn handle_topic_message(
        &self,
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        topic_msg: &Option<TopicMessage>,
        packet_id: &[u8; 32],
        db: &Arc<Mutex<DbConnection>>,
        event_tx: &mpsc::Sender<ProtocolEvent>,
        stream: Option<&Arc<crate::network::stream::StreamService>>,
    ) -> Result<ProcessResult, ProcessError> {
        let our_id = self.endpoint_id();

        // Handle SyncUpdate - emit event for app to handle
        if let Some(TopicMessage::SyncUpdate(sync_msg)) = topic_msg {
            info!(
                topic = %hex::encode(&topic_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                size = sync_msg.data.len(),
                "SYNC: received update from peer"
            );

            let event = crate::protocol::ProtocolEvent::SyncUpdate(crate::protocol::SyncUpdateEvent {
                topic_id: *topic_id,
                sender_id,
                data: sync_msg.data.clone(),
            });
            let _ = event_tx.send(event).await;

            {
                let db_lock = db.lock().await;
                let _ = mark_pulled(&db_lock, topic_id, packet_id);
            }

            return Ok(ProcessResult {
                receipt: super::protocol::Receipt::new(*packet_id, our_id),
                content_payload: None,
            });
        }

        // Handle SyncRequest - emit event for app to respond
        if let Some(TopicMessage::SyncRequest) = topic_msg {
            info!(
                topic = %hex::encode(&topic_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                "SYNC: received sync request from peer"
            );

            let event = crate::protocol::ProtocolEvent::SyncRequest(crate::protocol::SyncRequestEvent {
                topic_id: *topic_id,
                sender_id,
            });
            let _ = event_tx.send(event).await;

            {
                let db_lock = db.lock().await;
                let _ = mark_pulled(&db_lock, topic_id, packet_id);
            }

            return Ok(ProcessResult {
                receipt: super::protocol::Receipt::new(*packet_id, our_id),
                content_payload: None,
            });
        }

        // Handle control messages
        if let Some(msg) = topic_msg {
            let db_lock = db.lock().await;
            match msg {
                TopicMessage::Join(join) => {
                    if join.joiner != sender_id {
                        warn!(
                            joiner = %hex::encode(join.joiner),
                            sender = %hex::encode(sender_id),
                            "join message joiner doesn't match packet sender - ignoring"
                        );
                    } else {
                        trace!(
                            joiner = %hex::encode(join.joiner),
                            relay = ?join.relay_url,
                            topic = %hex::encode(topic_id),
                            "received join message"
                        );

                        if let Err(e) = add_topic_member(
                            &db_lock,
                            topic_id,
                            &join.joiner,
                        ) {
                            warn!(error = %e, "failed to add member");
                        }

                        if let Some(ref relay_url) = join.relay_url {
                            let _ = crate::data::update_peer_relay_url(
                                &db_lock,
                                &join.joiner,
                                relay_url,
                                crate::data::current_timestamp(),
                            );
                        }

                        debug!(
                            joiner = hex::encode(join.joiner),
                            relay = ?join.relay_url,
                            "member joined"
                        );
                    }
                }
                TopicMessage::Leave(leave) => {
                    if leave.leaver != sender_id {
                        warn!(
                            leaver = %hex::encode(leave.leaver),
                            sender = %hex::encode(sender_id),
                            "leave message leaver doesn't match packet sender - ignoring"
                        );
                    } else {
                        if let Err(e) = remove_topic_member(&db_lock, topic_id, &leave.leaver) {
                            warn!(error = %e, "failed to remove member");
                        }
                        debug!(leaver = hex::encode(leave.leaver), "member left");
                    }
                }
                TopicMessage::FileAnnouncement(ann) => {
                    if ann.source_id != sender_id {
                        warn!(
                            source = %hex::encode(ann.source_id),
                            sender = %hex::encode(sender_id),
                            "file announcement source doesn't match packet sender - ignoring"
                        );
                    } else {
                        info!(
                            hash = hex::encode(&ann.hash[..8]),
                            source = hex::encode(&ann.source_id[..8]),
                            name = ann.display_name,
                            size = ann.total_size,
                            chunks = ann.total_chunks,
                            sections = ann.num_sections,
                            "SHARE: Received file announcement"
                        );

                        let existing = get_blob(&db_lock, &ann.hash);
                        if existing.is_ok() && existing.as_ref().unwrap().is_some() {
                            debug!(
                                hash = hex::encode(&ann.hash[..8]),
                                "blob already known, skipping insert"
                            );
                        } else {
                            if let Err(e) = insert_blob(
                                &db_lock,
                                &ann.hash,
                                topic_id, // scope_id: topic scopes the blob
                                &ann.source_id,
                                &ann.display_name,
                                ann.total_size,
                                ann.num_sections,
                            ) {
                                warn!(error = %e, "failed to store blob metadata");
                            } else {
                                let total_chunks = ((ann.total_size + CHUNK_SIZE - 1)
                                    / CHUNK_SIZE) as u32;
                                if let Err(e) = init_blob_sections(
                                    &db_lock,
                                    &ann.hash,
                                    ann.num_sections,
                                    total_chunks,
                                ) {
                                    warn!(error = %e, "failed to init blob sections");
                                }
                                debug!(
                                    hash = hex::encode(&ann.hash[..8]),
                                    "blob stored, will pull via background task"
                                );

                                // Drop db_lock before sending event to avoid deadlock
                                drop(db_lock);
                                let file_event = crate::protocol::ProtocolEvent::FileAnnounced(crate::protocol::FileAnnouncedEvent {
                                    topic_id: *topic_id,
                                    source_id: ann.source_id,
                                    hash: ann.hash,
                                    display_name: ann.display_name.clone(),
                                    total_size: ann.total_size,
                                    total_chunks,
                                    timestamp: crate::data::current_timestamp(),
                                });
                                if event_tx.send(file_event).await.is_err() {
                                    debug!("event receiver dropped");
                                }

                                {
                                    let db_lock = db.lock().await;
                                    let _ = mark_pulled(&db_lock, topic_id, packet_id);
                                }
                                return Ok(ProcessResult {
                                    receipt: super::protocol::Receipt::new(*packet_id, our_id),
                                    content_payload: None,
                                });
                            }
                        }
                    }
                }
                TopicMessage::CanSeed(can_seed) => {
                    if can_seed.seeder_id != sender_id {
                        warn!(
                            seeder = %hex::encode(can_seed.seeder_id),
                            sender = %hex::encode(sender_id),
                            "can seed message seeder doesn't match packet sender - ignoring"
                        );
                    } else {
                        info!(
                            hash = hex::encode(&can_seed.hash[..8]),
                            peer = hex::encode(&can_seed.seeder_id[..8]),
                            "SHARE: Peer can seed file"
                        );

                        if let Err(e) = record_peer_can_seed(
                            &db_lock,
                            &can_seed.hash,
                            &can_seed.seeder_id,
                        ) {
                            warn!(error = %e, "failed to record peer can seed");
                        }
                    }
                }
                TopicMessage::Content(_) => {}
                TopicMessage::SyncUpdate(_) => {}
                TopicMessage::SyncRequest => {}
                // Stream signaling — route to StreamService
                TopicMessage::StreamRequest(_) => {
                    drop(db_lock);
                    if let Some(ref stream_svc) = stream {
                        stream_svc.handle_signaling(msg, topic_id, sender_id, PacketSource::Direct).await;
                    }

                    {
                        let db_lock = db.lock().await;
                        let _ = mark_pulled(&db_lock, topic_id, packet_id);
                    }
                    return Ok(ProcessResult {
                        receipt: super::protocol::Receipt::new(*packet_id, our_id),
                        content_payload: None,
                    });
                }
            }
        }

        // Extract content payload if this is a Content message
        let content_payload = if let Some(TopicMessage::Content(data)) = topic_msg.clone() {
            let event = crate::protocol::ProtocolEvent::Message(crate::protocol::IncomingMessage {
                topic_id: *topic_id,
                sender_id,
                payload: data.clone(),
                timestamp: crate::data::current_timestamp(),
            });

            if event_tx.send(event).await.is_err() {
                debug!("event receiver dropped");
            }
            Some(data)
        } else {
            None
        };

        // Mark packet as seen (dedup)
        {
            let db_lock = db.lock().await;
            let _ = mark_pulled(&db_lock, topic_id, packet_id);
        }

        Ok(ProcessResult {
            receipt: super::protocol::Receipt::new(*packet_id, our_id),
            content_payload,
        })
    }

    // ========== Incoming (Sealed Packets) ==========

    /// Process an incoming packet using service-owned db, event_tx, identity, and stream_service.
    pub async fn process_incoming_packet(
        &self,
        packet: &SendPacket,
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        source: PacketSource,
    ) -> Result<ProcessResult, ProcessError> {
        let our_id = self.endpoint_id();
        let db = &self.db;
        let event_tx = &self.event_tx;
        let stream = self.stream_service().await;

        // Determine verification mode from payload prefix
        let mode = get_verification_mode_from_payload(&packet.ciphertext)
            .unwrap_or(VerificationMode::Full);

        // Verify and decrypt
        let plaintext = verify_and_decrypt_packet_with_mode(packet, topic_id, mode)
            .map_err(|e| ProcessError::VerificationFailed(e.to_string()))?;

        info!(plaintext_len = plaintext.len(), "packet decrypted successfully");

        // Parse topic message
        let topic_msg = match TopicMessage::decode(&plaintext) {
            Ok(msg) => {
                info!(msg_type = ?msg.message_type(), "decoded TopicMessage");
                Some(msg)
            }
            Err(e) => {
                warn!(
                    topic = %hex::encode(&topic_id[..8]),
                    error = %e,
                    plaintext_len = plaintext.len(),
                    first_byte = ?plaintext.first(),
                    "failed to decode TopicMessage"
                );
                None
            }
        };

        // Handle SyncUpdate - emit event for app to handle
        if let Some(TopicMessage::SyncUpdate(ref sync_msg)) = topic_msg {
            info!(
                topic = %hex::encode(&topic_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                size = sync_msg.data.len(),
                "SYNC: received update from peer"
            );

            let event = crate::protocol::ProtocolEvent::SyncUpdate(crate::protocol::SyncUpdateEvent {
                topic_id: *topic_id,
                sender_id,
                data: sync_msg.data.clone(),
            });
            let _ = event_tx.send(event).await;

            {
                let db_lock = db.lock().await;
                let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
            }

            return Ok(ProcessResult {
                receipt: super::protocol::Receipt::new(packet.packet_id, our_id),
                content_payload: None,
            });
        }

        // Handle SyncRequest - emit event for app to respond
        if let Some(TopicMessage::SyncRequest) = topic_msg {
            info!(
                topic = %hex::encode(&topic_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                "SYNC: received sync request from peer"
            );

            let event = crate::protocol::ProtocolEvent::SyncRequest(crate::protocol::SyncRequestEvent {
                topic_id: *topic_id,
                sender_id,
            });
            let _ = event_tx.send(event).await;

            {
                let db_lock = db.lock().await;
                let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
            }

            return Ok(ProcessResult {
                receipt: super::protocol::Receipt::new(packet.packet_id, our_id),
                content_payload: None,
            });
        }

        // Handle control messages
        if let Some(ref msg) = topic_msg {
            let db_lock = db.lock().await;
            match msg {
                TopicMessage::Join(join) => {
                    if join.joiner != sender_id {
                        warn!(
                            joiner = %hex::encode(join.joiner),
                            sender = %hex::encode(sender_id),
                            "join message joiner doesn't match packet sender - ignoring"
                        );
                    } else {
                        trace!(
                            joiner = %hex::encode(join.joiner),
                            relay = ?join.relay_url,
                            topic = %hex::encode(topic_id),
                            "received join message"
                        );

                        if let Err(e) = add_topic_member(
                            &db_lock,
                            topic_id,
                            &join.joiner,
                        ) {
                            warn!(error = %e, "failed to add member");
                        }

                        if let Some(ref relay_url) = join.relay_url {
                            let _ = crate::data::update_peer_relay_url(
                                &db_lock,
                                &join.joiner,
                                relay_url,
                                crate::data::current_timestamp(),
                            );
                        }

                        debug!(
                            joiner = hex::encode(join.joiner),
                            relay = ?join.relay_url,
                            "member joined"
                        );
                    }
                }
                TopicMessage::Leave(leave) => {
                    if leave.leaver != sender_id {
                        warn!(
                            leaver = %hex::encode(leave.leaver),
                            sender = %hex::encode(sender_id),
                            "leave message leaver doesn't match packet sender - ignoring"
                        );
                    } else {
                        if let Err(e) = remove_topic_member(&db_lock, topic_id, &leave.leaver) {
                            warn!(error = %e, "failed to remove member");
                        }
                        debug!(leaver = hex::encode(leave.leaver), "member left");
                    }
                }
                TopicMessage::FileAnnouncement(ann) => {
                    if ann.source_id != sender_id {
                        warn!(
                            source = %hex::encode(ann.source_id),
                            sender = %hex::encode(sender_id),
                            "file announcement source doesn't match packet sender - ignoring"
                        );
                    } else {
                        info!(
                            hash = hex::encode(&ann.hash[..8]),
                            source = hex::encode(&ann.source_id[..8]),
                            name = ann.display_name,
                            size = ann.total_size,
                            chunks = ann.total_chunks,
                            sections = ann.num_sections,
                            "SHARE: Received file announcement"
                        );

                        let existing = get_blob(&db_lock, &ann.hash);
                        if existing.is_ok() && existing.as_ref().unwrap().is_some() {
                            debug!(
                                hash = hex::encode(&ann.hash[..8]),
                                "blob already known, skipping insert"
                            );
                        } else {
                            if let Err(e) = insert_blob(
                                &db_lock,
                                &ann.hash,
                                topic_id, // scope_id: topic scopes the blob
                                &ann.source_id,
                                &ann.display_name,
                                ann.total_size,
                                ann.num_sections,
                            ) {
                                warn!(error = %e, "failed to store blob metadata");
                            } else {
                                let total_chunks = ((ann.total_size + CHUNK_SIZE - 1)
                                    / CHUNK_SIZE) as u32;
                                if let Err(e) = init_blob_sections(
                                    &db_lock,
                                    &ann.hash,
                                    ann.num_sections,
                                    total_chunks,
                                ) {
                                    warn!(error = %e, "failed to init blob sections");
                                }
                                debug!(
                                    hash = hex::encode(&ann.hash[..8]),
                                    "blob stored, will pull via background task"
                                );

                                // Drop db_lock before sending event to avoid deadlock
                                drop(db_lock);
                                let file_event = crate::protocol::ProtocolEvent::FileAnnounced(crate::protocol::FileAnnouncedEvent {
                                    topic_id: *topic_id,
                                    source_id: ann.source_id,
                                    hash: ann.hash,
                                    display_name: ann.display_name.clone(),
                                    total_size: ann.total_size,
                                    total_chunks,
                                    timestamp: crate::data::current_timestamp(),
                                });
                                if event_tx.send(file_event).await.is_err() {
                                    debug!("event receiver dropped");
                                }

                                {
                                    let db_lock = db.lock().await;
                                    let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
                                }
                                return Ok(ProcessResult {
                                    receipt: super::protocol::Receipt::new(packet.packet_id, our_id),
                                    content_payload: None,
                                });
                            }
                        }
                    }
                }
                TopicMessage::CanSeed(can_seed) => {
                    if can_seed.seeder_id != sender_id {
                        warn!(
                            seeder = %hex::encode(can_seed.seeder_id),
                            sender = %hex::encode(sender_id),
                            "can seed message seeder doesn't match packet sender - ignoring"
                        );
                    } else {
                        info!(
                            hash = hex::encode(&can_seed.hash[..8]),
                            peer = hex::encode(&can_seed.seeder_id[..8]),
                            "SHARE: Peer can seed file"
                        );

                        if let Err(e) = record_peer_can_seed(
                            &db_lock,
                            &can_seed.hash,
                            &can_seed.seeder_id,
                        ) {
                            warn!(error = %e, "failed to record peer can seed");
                        }
                    }
                }
                TopicMessage::Content(_) => {}
                TopicMessage::SyncUpdate(_) => {}
                TopicMessage::SyncRequest => {}
                // Stream signaling — route to StreamService
                TopicMessage::StreamRequest(_) => {
                    drop(db_lock);
                    if let Some(ref stream) = stream {
                        stream.handle_signaling(msg, topic_id, sender_id, source).await;
                    }

                    {
                        let db_lock = db.lock().await;
                        let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
                    }
                    return Ok(ProcessResult {
                        receipt: super::protocol::Receipt::new(packet.packet_id, our_id),
                        content_payload: None,
                    });
                }
            }
        }

        // Extract content payload if this is a Content message
        let content_payload = if let Some(TopicMessage::Content(data)) = topic_msg {
            let event = crate::protocol::ProtocolEvent::Message(crate::protocol::IncomingMessage {
                topic_id: *topic_id,
                sender_id,
                payload: data.clone(),
                timestamp: crate::data::current_timestamp(),
            });

            if event_tx.send(event).await.is_err() {
                debug!("event receiver dropped");
            }
            Some(data)
        } else {
            None
        };

        // Mark packet as seen (dedup)
        {
            let db_lock = db.lock().await;
            let _ = mark_pulled(&db_lock, topic_id, &packet.packet_id);
        }

        Ok(ProcessResult {
            receipt: super::protocol::Receipt::new(packet.packet_id, our_id),
            content_payload,
        })
    }

    /// Process an incoming DM packet
    pub async fn process_incoming_dm_packet(
        &self,
        packet: &SendPacket,
    ) -> Result<ProcessResult, ProcessError> {
        use crate::network::send::dm_messages::DmMessage;

        let our_id = self.endpoint_id();

        // Verify signature + ECDH decrypt
        let plaintext = verify_and_decrypt_dm_packet(packet, &self.identity.private_key)
            .map_err(|e| ProcessError::VerificationFailed(e.to_string()))?;

        info!(plaintext_len = plaintext.len(), "DM packet decrypted");

        // Decode DmMessage
        let dm_msg = DmMessage::decode(&plaintext)
            .map_err(|e| ProcessError::VerificationFailed(format!("DM decode: {}", e)))?;

        match dm_msg {
            DmMessage::Content(data) => {
                let event = crate::protocol::ProtocolEvent::DmReceived(
                    crate::protocol::DmReceivedEvent {
                        sender_id: packet.endpoint_id,
                        payload: data,
                        timestamp: crate::data::current_timestamp(),
                    },
                );
                let _ = self.event_tx.send(event).await;
            }
            DmMessage::SyncUpdate(data) => {
                let event = crate::protocol::ProtocolEvent::DmSyncUpdate(
                    crate::protocol::DmSyncUpdateEvent {
                        sender_id: packet.endpoint_id,
                        data,
                    },
                );
                let _ = self.event_tx.send(event).await;
            }
            DmMessage::SyncRequest => {
                let event = crate::protocol::ProtocolEvent::DmSyncRequest(
                    crate::protocol::DmSyncRequestEvent {
                        sender_id: packet.endpoint_id,
                    },
                );
                let _ = self.event_tx.send(event).await;
            }
            DmMessage::FileAnnouncement(msg) => {
                let event = crate::protocol::ProtocolEvent::DmFileAnnounced(
                    crate::protocol::DmFileAnnouncedEvent {
                        sender_id: packet.endpoint_id,
                        hash: msg.hash,
                        display_name: msg.display_name,
                        total_size: msg.total_size,
                        total_chunks: msg.total_chunks,
                        num_sections: msg.num_sections,
                        timestamp: crate::data::current_timestamp(),
                    },
                );
                let _ = self.event_tx.send(event).await;
            }
            // DM stream signaling — route to StreamService
            DmMessage::StreamAccept(_)
            | DmMessage::StreamReject(_)
            | DmMessage::StreamQuery(_)
            | DmMessage::StreamActive(_)
            | DmMessage::StreamEnded(_)
            | DmMessage::StreamRequest(_) => {
                if let Some(stream_svc) = self.stream_service().await {
                    stream_svc.handle_dm_signaling(&dm_msg, packet.endpoint_id).await;
                }
            }
        }

        Ok(ProcessResult {
            receipt: super::protocol::Receipt::new(packet.packet_id, our_id),
            content_payload: None,
        })
    }

    /// Verify and decrypt a packet
    pub fn receive_packet(
        packet: &SendPacket,
        topic_id: &[u8; 32],
        our_id: [u8; 32],
    ) -> Result<(Vec<u8>, super::protocol::Receipt), PacketError> {
        let plaintext = verify_and_decrypt_packet(packet, topic_id)?;
        let receipt = super::protocol::Receipt::new(packet.packet_id, our_id);
        Ok((plaintext, receipt))
    }

    /// Process an incoming receipt - update tracking in database
    pub fn process_receipt(
        receipt: &super::protocol::Receipt,
        conn: &rusqlite::Connection,
    ) -> Result<bool, rusqlite::Error> {
        db_acknowledge_receipt(conn, &receipt.packet_id, &receipt.sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_all_tables;
    use crate::data::send::get_packets_needing_replication;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_all_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_send_error_display() {
        let err = SendError::NoRecipients;
        assert_eq!(err.to_string(), "no recipients specified");

        let err = SendError::Connection("test".to_string());
        assert_eq!(err.to_string(), "connection failed: test");

        let err = SendError::Send("stream closed".to_string());
        assert_eq!(err.to_string(), "send failed: stream closed");

        let err = SendError::Database("constraint failed".to_string());
        assert_eq!(err.to_string(), "database error: constraint failed");
    }

    #[test]
    fn test_send_error_from_packet_error() {
        let packet_err = PacketError::InvalidMac;
        let send_err: SendError = packet_err.into();
        assert!(matches!(send_err, SendError::PacketCreation(_)));
    }

    #[test]
    fn test_send_result_structure() {
        let result = SendResult {
            packet_id: [1u8; 32],
            delivered_to: vec![test_id(1), test_id(2)],
            failed: vec![(test_id(3), "timeout".to_string())],
        };

        assert_eq!(result.packet_id, [1u8; 32]);
        assert_eq!(result.delivered_to.len(), 2);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].1, "timeout");
    }

    #[test]
    fn test_outgoing_packet_tracking() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [1u8; 32];
        let recipients = vec![test_id(10), test_id(11), test_id(12)];

        // Store packet
        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"data",
            &recipients,
            0, // Content
        ).unwrap();

        // Get packets needing replication (none acked yet)
        let needs_replication = get_packets_needing_replication(&db_conn).unwrap();
        assert_eq!(needs_replication.len(), 1);
    }
}
