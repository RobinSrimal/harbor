//! Stream Service - Real-time streaming over MOQ
//!
//! Manages stream signaling (via SendService) and media transport (via moq-lite).
//! Streams are topic-scoped and require explicit acceptance before media flows.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{debug, info, warn};

use super::protocol::{STREAM_SIGNAL_TTL, StreamError};
use super::session::StreamSession;
use super::{ZERO_TOPIC_ID, optional_topic_id};
use crate::data::{LocalIdentity, get_topic_members};
use crate::network::connect::Connector;
use crate::network::gate::ConnectionGate;
use crate::network::packet::{
    DmMessage, DmStreamRequestMessage, StreamAcceptMessage, StreamActiveMessage,
    StreamEndedMessage, StreamQueryMessage, StreamRejectMessage, StreamRequestMessage,
    StreamSignalingMessage, TopicMessage,
};
use crate::network::send::{SendOptions, SendService};
use crate::protocol::{MemberInfo, ProtocolEvent, StreamConnectedEvent};

/// A pending stream request awaiting accept/reject
#[derive(Debug, Clone)]
pub struct PendingStream {
    /// Logical broadcast grouping ID (shared across recipients)
    pub broadcast_id: [u8; 32],
    /// Unique request identifier
    pub request_id: [u8; 32],
    /// Topic this stream is scoped to
    pub topic_id: [u8; 32],
    /// The peer involved (source or destination depending on perspective)
    pub peer_id: [u8; 32],
    /// Human-readable stream name
    pub name: String,
    /// Catalog metadata bytes
    pub catalog: Vec<u8>,
}

/// An active outgoing stream (source side)
#[derive(Debug)]
pub struct ActiveStream {
    /// Logical broadcast grouping ID (shared across recipients)
    pub broadcast_id: [u8; 32],
    /// Unique request identifier
    pub request_id: [u8; 32],
    /// Topic this stream is scoped to
    pub topic_id: [u8; 32],
    /// The destination peer
    pub peer_id: [u8; 32],
    /// Stream name
    pub name: String,
}

/// Messages sent to the StreamService actor
enum StreamActorMessage {
    /// A stream request was accepted by the destination
    StreamAccepted {
        request_id: [u8; 32],
        sender_id: [u8; 32],
    },
    /// A stream request was rejected by the destination
    StreamRejected {
        request_id: [u8; 32],
        reason: Option<String>,
        sender_id: [u8; 32],
    },
    /// A stream has ended
    StreamEnded {
        request_id: [u8; 32],
        sender_id: [u8; 32],
    },
    /// A liveness query from a peer that pulled our StreamRequest from harbor
    StreamQuery {
        request_id: [u8; 32],
        topic_id: [u8; 32],
        querier_id: [u8; 32],
    },
}

/// Live streaming service
///
/// Manages the lifecycle of real-time streams:
/// 1. Source requests a stream via signaling (SendService)
/// 2. Destination accepts/rejects
/// 3. On accept, MOQ connection is established for media transport
pub struct StreamService {
    /// Connector for outgoing QUIC connections (used for MOQ transport)
    connector: Arc<Connector>,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender for protocol events
    event_tx: mpsc::Sender<ProtocolEvent>,
    /// Send service for signaling messages
    send_service: Arc<SendService>,
    /// Connection gate for peer authorization
    connection_gate: Option<Arc<ConnectionGate>>,
    /// Actor message channel
    actor_tx: mpsc::Sender<StreamActorMessage>,
    /// Pending outgoing requests (source side, awaiting accept/reject)
    pending_outgoing: Arc<RwLock<HashMap<[u8; 32], PendingStream>>>,
    /// Pending incoming requests (destination side, awaiting app decision)
    pending_incoming: Arc<RwLock<HashMap<[u8; 32], PendingStream>>>,
    /// Accepted incoming requests (destination side, awaiting MOQ connection from source)
    accepted_incoming: Arc<RwLock<HashMap<[u8; 32], PendingStream>>>,
    /// Currently active outgoing streams (source side, for liveness queries)
    active_streams: Arc<RwLock<HashMap<[u8; 32], ActiveStream>>>,
    /// Active MOQ sessions keyed by request_id
    sessions: Arc<RwLock<HashMap<[u8; 32], Arc<StreamSession>>>>,
}

impl std::fmt::Debug for StreamService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamService").finish_non_exhaustive()
    }
}

impl StreamService {
    /// Create a new StreamService
    pub fn new(
        connector: Arc<Connector>,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        send_service: Arc<SendService>,
        connection_gate: Option<Arc<ConnectionGate>>,
    ) -> Arc<Self> {
        let (actor_tx, actor_rx) = mpsc::channel(256);
        let pending_outgoing = Arc::new(RwLock::new(HashMap::new()));
        let pending_incoming = Arc::new(RwLock::new(HashMap::new()));
        let accepted_incoming = Arc::new(RwLock::new(HashMap::new()));
        let active_streams = Arc::new(RwLock::new(HashMap::new()));
        let sessions = Arc::new(RwLock::new(HashMap::new()));

        let service = Arc::new(Self {
            connector,
            identity,
            db,
            event_tx,
            send_service,
            connection_gate,
            actor_tx,
            pending_outgoing,
            pending_incoming,
            accepted_incoming,
            active_streams,
            sessions,
        });

        // Spawn the actor loop
        let service_clone = service.clone();
        tokio::spawn(async move {
            service_clone.run_actor(actor_rx).await;
        });

        service
    }

    /// Get topic members for signaling, excluding ourselves
    async fn get_recipients(&self, topic_id: &[u8; 32]) -> Result<Vec<MemberInfo>, StreamError> {
        let our_id = self.identity.public_key;
        let db = self.db.lock().await;
        let members =
            get_topic_members(&db, topic_id).map_err(|e| StreamError::Database(e.to_string()))?;
        Ok(members
            .into_iter()
            .filter(|m| *m != our_id)
            .map(MemberInfo::new)
            .collect())
    }

    /// Send a signaling message to a specific recipient set
    async fn send_signaling_to_recipients(
        &self,
        topic_id: &[u8; 32],
        recipients: &[MemberInfo],
        msg: &TopicMessage,
        options: SendOptions,
    ) -> Result<(), StreamError> {
        if recipients.is_empty() {
            return Ok(());
        }
        let payload = msg.encode();
        self.send_service
            .send_to_topic(topic_id, &payload, recipients, options)
            .await
            .map_err(|e| StreamError::Signaling(e.to_string()))?;
        Ok(())
    }

    /// Send a DM signaling message to a specific peer
    async fn send_dm_signaling(
        &self,
        target_peer: &[u8; 32],
        dm_msg: DmMessage,
    ) -> Result<(), StreamError> {
        let encoded = dm_msg.encode();
        self.send_service
            .send_to_dm(target_peer, &encoded)
            .await
            .map_err(|e| StreamError::Signaling(e.to_string()))?;
        Ok(())
    }

    /// Send a stream signaling message to a specific peer
    async fn send_stream_signaling(
        &self,
        target_peer: &[u8; 32],
        msg: StreamSignalingMessage,
    ) -> Result<(), StreamError> {
        let encoded = msg.encode();
        self.send_service
            .send_to_dm(target_peer, &encoded)
            .await
            .map_err(|e| StreamError::Signaling(e.to_string()))?;
        Ok(())
    }

    /// Request to stream to a peer (source side)
    ///
    /// Sends StreamRequest messages via SendService.
    /// For fanout, creates one unique `request_id` per recipient and groups them
    /// under a shared `broadcast_id`.
    pub async fn request_stream(
        &self,
        topic_id: &[u8; 32],
        peer_id: &[u8; 32],
        name: &str,
        catalog: Vec<u8>,
    ) -> Result<[u8; 32], StreamError> {
        let recipients = self.get_recipients(topic_id).await?;
        if recipients.is_empty() {
            return Err(StreamError::Signaling(
                "no topic recipients available".to_string(),
            ));
        }

        // Topic stream requests fan out to every current topic member.
        // `peer_id` remains as a compatibility hint for callers that still pass it.
        let targets = recipients;

        let broadcast_id = generate_request_id();
        let options = SendOptions::content().with_ttl(STREAM_SIGNAL_TTL);
        let mut sent = 0usize;
        let mut first_request_id: Option<[u8; 32]> = None;

        for target in targets {
            let request_id = generate_request_id();
            let pending = PendingStream {
                broadcast_id,
                request_id,
                topic_id: *topic_id,
                peer_id: target.endpoint_id,
                name: name.to_string(),
                catalog: catalog.clone(),
            };
            self.pending_outgoing
                .write()
                .await
                .insert(request_id, pending);

            let msg = TopicMessage::StreamRequest(StreamRequestMessage {
                broadcast_id,
                request_id,
                name: name.to_string(),
                catalog: catalog.clone(),
            });
            let recipient = [target];

            match self
                .send_signaling_to_recipients(topic_id, &recipient, &msg, options.clone())
                .await
            {
                Ok(()) => {
                    sent += 1;
                    if first_request_id.is_none() {
                        first_request_id = Some(request_id);
                    }
                }
                Err(e) => {
                    self.pending_outgoing.write().await.remove(&request_id);
                    warn!(
                        request_id = %hex::encode(&request_id[..8]),
                        peer = %hex::encode(&recipient[0].endpoint_id[..8]),
                        error = %e,
                        "failed to send stream request to recipient"
                    );
                }
            }
        }

        if sent == 0 {
            return Err(StreamError::Signaling(
                "failed to send stream request to any recipients".to_string(),
            ));
        }

        debug!(
            broadcast_id = %hex::encode(&broadcast_id[..8]),
            topic = %hex::encode(&topic_id[..8]),
            requested_peer = %hex::encode(&peer_id[..8]),
            sent_recipients = sent,
            name = %name,
            "stream request(s) sent"
        );

        if sent == 1 {
            Ok(first_request_id.expect("request id must exist when sent == 1"))
        } else {
            Ok(broadcast_id)
        }
    }

    /// Request a DM stream to a peer (source side, peer-to-peer, no topic)
    ///
    /// Sends a DmMessage::StreamRequest directly to the peer.
    pub async fn dm_request_stream(
        &self,
        peer_id: &[u8; 32],
        name: &str,
    ) -> Result<[u8; 32], StreamError> {
        let request_id = generate_request_id();
        let broadcast_id = request_id;

        let pending = PendingStream {
            broadcast_id,
            request_id,
            topic_id: ZERO_TOPIC_ID,
            peer_id: *peer_id,
            name: name.to_string(),
            catalog: vec![],
        };
        self.pending_outgoing
            .write()
            .await
            .insert(request_id, pending);

        let dm_msg = DmMessage::StreamRequest(DmStreamRequestMessage {
            broadcast_id,
            request_id,
            stream_name: name.to_string(),
        });
        self.send_dm_signaling(peer_id, dm_msg).await?;

        debug!(
            broadcast_id = %hex::encode(&broadcast_id[..8]),
            request_id = %hex::encode(&request_id[..8]),
            peer = %hex::encode(&peer_id[..8]),
            name = %name,
            "DM stream request sent"
        );

        Ok(request_id)
    }

    /// Accept an incoming stream request (destination side)
    pub async fn accept_stream(self: &Arc<Self>, request_id: &[u8; 32]) -> Result<(), StreamError> {
        let pending = self
            .pending_incoming
            .write()
            .await
            .remove(request_id)
            .ok_or(StreamError::RequestNotFound)?;

        // Move to accepted_incoming so the MOQ handler can find it
        self.accepted_incoming
            .write()
            .await
            .insert(*request_id, pending.clone());

        let msg = StreamSignalingMessage::Accept(StreamAcceptMessage {
            request_id: *request_id,
        });
        self.send_stream_signaling(&pending.peer_id, msg).await?;

        // Spawn timeout cleanup — if source never connects within 30s, remove stale entry
        let svc = self.clone();
        let rid = *request_id;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            if svc.accepted_incoming.write().await.remove(&rid).is_some() {
                warn!(request_id = %hex::encode(&rid[..8]), "accepted stream timed out — source never connected");
            }
        });

        info!(request_id = %hex::encode(&request_id[..8]), "stream request accepted");
        Ok(())
    }

    /// Reject an incoming stream request (destination side)
    pub async fn reject_stream(
        &self,
        request_id: &[u8; 32],
        reason: Option<String>,
    ) -> Result<(), StreamError> {
        let pending = self
            .pending_incoming
            .write()
            .await
            .remove(request_id)
            .ok_or(StreamError::RequestNotFound)?;

        let msg = StreamSignalingMessage::Reject(StreamRejectMessage {
            request_id: *request_id,
            reason: reason.clone(),
        });
        self.send_stream_signaling(&pending.peer_id, msg).await?;

        info!(request_id = %hex::encode(&request_id[..8]), reason = ?reason, "stream request rejected");
        Ok(())
    }

    /// End an active stream (source side)
    pub async fn end_stream(&self, request_id: &[u8; 32]) -> Result<(), StreamError> {
        let active = self
            .active_streams
            .write()
            .await
            .remove(request_id)
            .ok_or(StreamError::RequestNotFound)?;

        let msg = StreamSignalingMessage::Ended(StreamEndedMessage {
            request_id: *request_id,
        });
        self.send_stream_signaling(&active.peer_id, msg).await?;

        info!(request_id = %hex::encode(&request_id[..8]), "stream ended");
        Ok(())
    }

    /// Check if a stream is still active (for liveness queries)
    pub async fn is_stream_active(&self, request_id: &[u8; 32]) -> bool {
        self.active_streams.read().await.contains_key(request_id)
    }

    /// Find an accepted incoming request from a specific peer
    pub(crate) async fn find_accepted_for_peer(&self, peer_id: &[u8; 32]) -> Option<PendingStream> {
        let accepted = self.accepted_incoming.read().await;
        let mut matches = accepted.values().filter(|p| p.peer_id == *peer_id);
        let first = matches.next().cloned();
        if first.is_some() && matches.next().is_some() {
            warn!(
                peer = %hex::encode(&peer_id[..8]),
                "multiple accepted stream requests from same peer; selecting first"
            );
        }
        first
    }

    /// Remove an accepted incoming request after successful MOQ handshake
    pub(crate) async fn remove_accepted(&self, request_id: &[u8; 32]) {
        self.accepted_incoming.write().await.remove(request_id);
    }

    /// Store an active MOQ session
    pub(crate) async fn store_session(&self, request_id: [u8; 32], session: StreamSession) {
        self.sessions
            .write()
            .await
            .insert(request_id, Arc::new(session));
    }

    /// Get an active MOQ session by request_id
    pub(crate) async fn get_session(&self, request_id: &[u8; 32]) -> Option<Arc<StreamSession>> {
        self.sessions.read().await.get(request_id).cloned()
    }

    /// Remove an active MOQ session
    pub(crate) async fn remove_session(&self, request_id: &[u8; 32]) {
        self.sessions.write().await.remove(request_id);
    }

    /// Get the event sender
    pub(crate) fn event_tx(&self) -> &mpsc::Sender<ProtocolEvent> {
        &self.event_tx
    }

    /// Get the connection gate (for handler to check peer authorization)
    pub fn connection_gate(&self) -> Option<&Arc<ConnectionGate>> {
        self.connection_gate.as_ref()
    }

    /// Establish an outgoing MOQ connection to the destination peer (source side).
    ///
    /// Called after the destination accepts our stream request. Dials the peer
    /// with STREAM_ALPN, performs the MOQ Client handshake, and stores the session.
    pub(crate) async fn connect_moq(&self, request_id: &[u8; 32]) -> Result<(), StreamError> {
        // 1. Look up the active stream to get peer_id/topic_id/broadcast_id
        let active = self
            .active_streams
            .read()
            .await
            .get(request_id)
            .map(|a| (a.peer_id, a.topic_id, a.broadcast_id));
        let (peer_id, topic_id, broadcast_id) = active.ok_or(StreamError::RequestNotFound)?;

        // 2. Dial the peer with STREAM_ALPN
        let conn = self
            .connector
            .connect_with_timeout(&peer_id, Duration::from_secs(10))
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))?;

        info!(
            peer = %hex::encode(&peer_id[..8]),
            request_id = %hex::encode(&request_id[..8]),
            "MOQ connection established to destination"
        );

        // 4. Wrap as WebTransport (raw QUIC, no HTTP/3)
        let wt_session = web_transport_iroh::Session::raw(conn);

        // 5. Create Origin and perform MOQ Client handshake
        //    Source only publishes — no .with_consume() to avoid bidirectional subscribe loops
        let origin = moq_lite::Origin::produce();
        let moq_client = moq_lite::Client::new().with_publish(origin.consumer.clone());

        let moq_session = moq_client
            .connect(wt_session)
            .await
            .map_err(|e| StreamError::Moq(e.to_string()))?;

        info!(
            peer = %hex::encode(&peer_id[..8]),
            request_id = %hex::encode(&request_id[..8]),
            "MOQ session established (source side)"
        );

        // 6. Create and store StreamSession
        let live_session = StreamSession::from_parts(
            moq_session,
            origin.producer,
            origin.consumer,
            topic_id,
            *request_id,
        );
        self.store_session(*request_id, live_session).await;

        // 7. Emit StreamConnected event
        let event = ProtocolEvent::StreamConnected(StreamConnectedEvent {
            broadcast_id,
            request_id: *request_id,
            topic_id: optional_topic_id(&topic_id),
            peer_id,
            is_source: true,
        });
        let _ = self.event_tx.send(event).await;

        Ok(())
    }

    /// Handle an incoming stream signaling message from the Send path (topic-based)
    ///
    /// Only handles StreamRequest (the only stream type still sent via topic broadcast).
    pub async fn handle_signaling(
        &self,
        msg: &TopicMessage,
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        source: crate::network::send::PacketSource,
    ) {
        if let TopicMessage::StreamRequest(req) = msg {
            self.handle_incoming_stream_request(
                &req.broadcast_id,
                &req.request_id,
                topic_id,
                sender_id,
                &req.name,
                &req.catalog,
                source,
            )
            .await;
        }
    }

    /// Handle an incoming DM stream signaling message
    pub async fn handle_dm_signaling(&self, dm_msg: &DmMessage, sender_id: [u8; 32]) {
        match dm_msg {
            DmMessage::StreamRequest(req) => {
                self.handle_incoming_stream_request(
                    &req.broadcast_id,
                    &req.request_id,
                    &ZERO_TOPIC_ID,
                    sender_id,
                    &req.stream_name,
                    &[],
                    crate::network::send::PacketSource::Direct,
                )
                .await;
            }
            _ => {}
        }
    }

    /// Handle an incoming stream signaling message (Accept/Reject/Query/Active/Ended)
    pub async fn handle_stream_signaling_msg(
        &self,
        msg: &StreamSignalingMessage,
        sender_id: [u8; 32],
    ) {
        match msg {
            StreamSignalingMessage::Accept(accept) => {
                let _ = self
                    .actor_tx
                    .send(StreamActorMessage::StreamAccepted {
                        request_id: accept.request_id,
                        sender_id,
                    })
                    .await;
            }
            StreamSignalingMessage::Reject(reject) => {
                let _ = self
                    .actor_tx
                    .send(StreamActorMessage::StreamRejected {
                        request_id: reject.request_id,
                        reason: reject.reason.clone(),
                        sender_id,
                    })
                    .await;
            }
            StreamSignalingMessage::Query(query) => {
                let _ = self
                    .actor_tx
                    .send(StreamActorMessage::StreamQuery {
                        request_id: query.request_id,
                        topic_id: [0u8; 32], // Stream signaling is peer-to-peer, no topic context
                        querier_id: sender_id,
                    })
                    .await;
            }
            StreamSignalingMessage::Active(active) => {
                self.emit_deferred_stream_request(&active.request_id).await;
            }
            StreamSignalingMessage::Ended(ended) => {
                let _ = self
                    .actor_tx
                    .send(StreamActorMessage::StreamEnded {
                        request_id: ended.request_id,
                        sender_id,
                    })
                    .await;
            }
        }
    }

    /// Handle an incoming StreamRequest
    async fn handle_incoming_stream_request(
        &self,
        broadcast_id: &[u8; 32],
        request_id: &[u8; 32],
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        name: &str,
        catalog: &[u8],
        source: crate::network::send::PacketSource,
    ) {
        let pending = PendingStream {
            broadcast_id: *broadcast_id,
            request_id: *request_id,
            topic_id: *topic_id,
            peer_id: sender_id,
            name: name.to_string(),
            catalog: catalog.to_vec(),
        };

        match source {
            crate::network::send::PacketSource::Direct => {
                self.pending_incoming
                    .write()
                    .await
                    .insert(*request_id, pending.clone());
                self.emit_stream_request_event(&pending).await;
            }
            crate::network::send::PacketSource::HarborPull => {
                self.pending_incoming
                    .write()
                    .await
                    .insert(*request_id, pending);

                let msg = StreamSignalingMessage::Query(StreamQueryMessage {
                    request_id: *request_id,
                });
                if let Err(e) = self.send_stream_signaling(&sender_id, msg).await {
                    warn!(
                        request_id = %hex::encode(&request_id[..8]),
                        error = %e,
                        "failed to send stream liveness query — discarding request"
                    );
                    self.pending_incoming.write().await.remove(request_id);
                }
            }
        }
    }

    async fn emit_stream_request_event(&self, pending: &PendingStream) {
        let event = ProtocolEvent::StreamRequest(crate::protocol::StreamRequestEvent {
            topic_id: optional_topic_id(&pending.topic_id),
            peer_id: pending.peer_id,
            broadcast_id: pending.broadcast_id,
            request_id: pending.request_id,
            name: pending.name.clone(),
            catalog: pending.catalog.clone(),
        });
        let _ = self.event_tx.send(event).await;
    }

    async fn emit_deferred_stream_request(&self, request_id: &[u8; 32]) {
        let pending = self.pending_incoming.read().await.get(request_id).cloned();
        if let Some(pending) = pending {
            self.emit_stream_request_event(&pending).await;
        }
    }

    async fn run_actor(self: Arc<Self>, mut rx: mpsc::Receiver<StreamActorMessage>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                StreamActorMessage::StreamAccepted {
                    request_id,
                    sender_id,
                } => {
                    let pending = self.pending_outgoing.write().await.remove(&request_id);
                    if let Some(pending) = pending {
                        if pending.peer_id != sender_id {
                            warn!(
                                request_id = %hex::encode(&request_id[..8]),
                                expected = %hex::encode(&pending.peer_id[..8]),
                                sender = %hex::encode(&sender_id[..8]),
                                "ignoring stream accept from unexpected peer"
                            );
                            self.pending_outgoing
                                .write()
                                .await
                                .insert(request_id, pending);
                            continue;
                        }

                        let active = ActiveStream {
                            broadcast_id: pending.broadcast_id,
                            request_id,
                            topic_id: pending.topic_id,
                            peer_id: pending.peer_id,
                            name: pending.name,
                        };
                        self.active_streams.write().await.insert(request_id, active);

                        let event =
                            ProtocolEvent::StreamAccepted(crate::protocol::StreamAcceptedEvent {
                                broadcast_id: pending.broadcast_id,
                                request_id,
                            });
                        let _ = self.event_tx.send(event).await;
                        info!(
                            request_id = %hex::encode(&request_id[..8]),
                            broadcast_id = %hex::encode(&pending.broadcast_id[..8]),
                            "stream accepted — ready for MOQ connection"
                        );

                        // Spawn outgoing MOQ connection to destination
                        let svc = self.clone();
                        tokio::spawn(async move {
                            if let Err(e) = svc.connect_moq(&request_id).await {
                                warn!(
                                    request_id = %hex::encode(&request_id[..8]),
                                    error = %e,
                                    "failed to establish outgoing MOQ connection"
                                );
                            }
                        });
                    }
                }
                StreamActorMessage::StreamRejected {
                    request_id,
                    reason,
                    sender_id,
                } => {
                    let pending = self.pending_outgoing.write().await.remove(&request_id);
                    if let Some(pending) = pending {
                        if pending.peer_id != sender_id {
                            warn!(
                                request_id = %hex::encode(&request_id[..8]),
                                expected = %hex::encode(&pending.peer_id[..8]),
                                sender = %hex::encode(&sender_id[..8]),
                                "ignoring stream reject from unexpected peer"
                            );
                            self.pending_outgoing
                                .write()
                                .await
                                .insert(request_id, pending);
                            continue;
                        }

                        let event =
                            ProtocolEvent::StreamRejected(crate::protocol::StreamRejectedEvent {
                                broadcast_id: pending.broadcast_id,
                                request_id,
                                reason: reason.clone(),
                            });
                        let _ = self.event_tx.send(event).await;
                        info!(request_id = %hex::encode(&request_id[..8]), reason = ?reason, "stream rejected");
                    }
                }
                StreamActorMessage::StreamEnded {
                    request_id,
                    sender_id,
                } => {
                    let was_active = self.active_streams.write().await.remove(&request_id);
                    let was_incoming = self.pending_incoming.write().await.remove(&request_id);
                    let accepted = self.accepted_incoming.write().await.remove(&request_id);
                    let had_session = self.sessions.read().await.contains_key(&request_id);

                    let expected_peer = was_active
                        .as_ref()
                        .map(|a| a.peer_id)
                        .or(was_incoming.as_ref().map(|p| p.peer_id))
                        .or(accepted.as_ref().map(|p| p.peer_id));
                    if let Some(expected_peer) = expected_peer {
                        if expected_peer != sender_id {
                            warn!(
                                request_id = %hex::encode(&request_id[..8]),
                                expected = %hex::encode(&expected_peer[..8]),
                                sender = %hex::encode(&sender_id[..8]),
                                "ignoring stream ended from unexpected peer"
                            );
                            if let Some(active) = was_active {
                                self.active_streams.write().await.insert(request_id, active);
                            }
                            if let Some(incoming) = was_incoming {
                                self.pending_incoming
                                    .write()
                                    .await
                                    .insert(request_id, incoming);
                            }
                            if let Some(accepted) = accepted {
                                self.accepted_incoming
                                    .write()
                                    .await
                                    .insert(request_id, accepted);
                            }
                            continue;
                        }
                    }

                    if was_active.is_some()
                        || was_incoming.is_some()
                        || accepted.is_some()
                        || had_session
                    {
                        let peer_id = expected_peer.unwrap_or(sender_id);
                        let broadcast_id = was_active
                            .as_ref()
                            .map(|a| a.broadcast_id)
                            .or(was_incoming.as_ref().map(|p| p.broadcast_id))
                            .or(accepted.as_ref().map(|p| p.broadcast_id));

                        let event = ProtocolEvent::StreamEnded(crate::protocol::StreamEndedEvent {
                            broadcast_id,
                            request_id,
                            peer_id,
                        });
                        let _ = self.event_tx.send(event).await;
                    }
                }
                StreamActorMessage::StreamQuery {
                    request_id,
                    topic_id,
                    querier_id,
                } => {
                    self.handle_stream_query(&request_id, &topic_id, querier_id)
                        .await;
                }
            }
        }
    }

    async fn handle_stream_query(
        &self,
        request_id: &[u8; 32],
        _topic_id: &[u8; 32],
        querier_id: [u8; 32],
    ) {
        let is_active = self.active_streams.read().await.contains_key(request_id)
            || self.pending_outgoing.read().await.contains_key(request_id);

        let msg = if is_active {
            StreamSignalingMessage::Active(StreamActiveMessage {
                request_id: *request_id,
            })
        } else {
            StreamSignalingMessage::Ended(StreamEndedMessage {
                request_id: *request_id,
            })
        };

        if let Err(e) = self.send_stream_signaling(&querier_id, msg).await {
            warn!(
                request_id = %hex::encode(&request_id[..8]),
                error = %e,
                "failed to respond to stream query"
            );
        }
    }
}

/// Generate a random 32-byte request ID
fn generate_request_id() -> [u8; 32] {
    use rand::RngCore;
    let mut id = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut id);
    id
}
