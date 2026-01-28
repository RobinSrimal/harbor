//! Live Service - Real-time streaming over MOQ
//!
//! Manages stream signaling (via SendService) and media transport (via moq-lite).
//! Streams are topic-scoped and require explicit acceptance before media flows.

use std::collections::HashMap;
use std::sync::Arc;

use iroh::Endpoint;
use rusqlite::Connection as DbConnection;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::data::{get_topic_members, LocalIdentity};
use crate::network::send::{SendService, SendOptions, TopicMessage};
use crate::network::send::topic_messages::{
    StreamRequestMessage, StreamAcceptMessage, StreamRejectMessage,
    StreamQueryMessage, StreamActiveMessage, StreamEndedMessage,
};
use crate::protocol::{MemberInfo, ProtocolEvent};
use super::protocol::{STREAM_SIGNAL_TTL, LiveError};
use super::session::LiveSession;

/// A pending stream request awaiting accept/reject
#[derive(Debug, Clone)]
pub struct PendingStream {
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
    /// Unique request identifier
    pub request_id: [u8; 32],
    /// Topic this stream is scoped to
    pub topic_id: [u8; 32],
    /// The destination peer
    pub peer_id: [u8; 32],
    /// Stream name
    pub name: String,
}

/// Messages sent to the LiveService actor
enum LiveActorMessage {
    /// A stream request was accepted by the destination
    StreamAccepted {
        request_id: [u8; 32],
    },
    /// A stream request was rejected by the destination
    StreamRejected {
        request_id: [u8; 32],
        reason: Option<String>,
    },
    /// A stream has ended
    StreamEnded {
        request_id: [u8; 32],
    },
    /// A liveness query from a peer that pulled our StreamRequest from harbor
    StreamQuery {
        request_id: [u8; 32],
        querier_id: [u8; 32],
        topic_id: [u8; 32],
    },
}

/// Live streaming service
///
/// Manages the lifecycle of real-time streams:
/// 1. Source requests a stream via signaling (SendService)
/// 2. Destination accepts/rejects
/// 3. On accept, MOQ connection is established for media transport
pub struct LiveService {
    /// Iroh endpoint for QUIC connections
    endpoint: Endpoint,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender for protocol events
    event_tx: mpsc::Sender<ProtocolEvent>,
    /// Send service for signaling messages
    send_service: Arc<SendService>,
    /// Actor message channel
    actor_tx: mpsc::Sender<LiveActorMessage>,
    /// Pending outgoing requests (source side, awaiting accept/reject)
    pending_outgoing: Arc<RwLock<HashMap<[u8; 32], PendingStream>>>,
    /// Pending incoming requests (destination side, awaiting app decision)
    pending_incoming: Arc<RwLock<HashMap<[u8; 32], PendingStream>>>,
    /// Currently active outgoing streams (source side, for liveness queries)
    active_streams: Arc<RwLock<HashMap<[u8; 32], ActiveStream>>>,
    /// Active MOQ sessions keyed by request_id
    sessions: Arc<RwLock<HashMap<[u8; 32], Arc<LiveSession>>>>,
}

impl std::fmt::Debug for LiveService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveService").finish_non_exhaustive()
    }
}

impl LiveService {
    /// Create a new LiveService
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
        send_service: Arc<SendService>,
    ) -> Arc<Self> {
        let (actor_tx, actor_rx) = mpsc::channel(256);
        let pending_outgoing = Arc::new(RwLock::new(HashMap::new()));
        let pending_incoming = Arc::new(RwLock::new(HashMap::new()));
        let active_streams = Arc::new(RwLock::new(HashMap::new()));
        let sessions = Arc::new(RwLock::new(HashMap::new()));

        let service = Arc::new(Self {
            endpoint,
            identity,
            db,
            event_tx,
            send_service,
            actor_tx,
            pending_outgoing,
            pending_incoming,
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
    async fn get_recipients(&self, topic_id: &[u8; 32]) -> Result<Vec<MemberInfo>, LiveError> {
        let our_id = self.identity.public_key;
        let db = self.db.lock().await;
        let members = get_topic_members(&db, topic_id)
            .map_err(|e| LiveError::Database(e.to_string()))?;
        Ok(members
            .into_iter()
            .filter(|m| *m != our_id)
            .map(MemberInfo::new)
            .collect())
    }

    /// Send a signaling message to topic members
    async fn send_signaling(
        &self,
        topic_id: &[u8; 32],
        msg: &TopicMessage,
        options: SendOptions,
    ) -> Result<(), LiveError> {
        let recipients = self.get_recipients(topic_id).await?;
        if recipients.is_empty() {
            return Ok(());
        }
        let payload = msg.encode();
        self.send_service
            .send_to_topic(topic_id, &payload, &recipients, options)
            .await
            .map_err(|e| LiveError::Signaling(e.to_string()))?;
        Ok(())
    }

    /// Request to stream to a peer (source side)
    ///
    /// Sends a StreamRequest via SendService. The request is stored on harbor
    /// with a 6-hour TTL so offline peers can discover it.
    pub async fn request_stream(
        &self,
        topic_id: &[u8; 32],
        peer_id: &[u8; 32],
        name: &str,
        catalog: Vec<u8>,
    ) -> Result<[u8; 32], LiveError> {
        let request_id = generate_request_id();

        let pending = PendingStream {
            request_id,
            topic_id: *topic_id,
            peer_id: *peer_id,
            name: name.to_string(),
            catalog: catalog.clone(),
        };
        self.pending_outgoing.write().await.insert(request_id, pending);

        let msg = TopicMessage::StreamRequest(StreamRequestMessage {
            request_id,
            name: name.to_string(),
            catalog,
        });
        let options = SendOptions::content().with_ttl(STREAM_SIGNAL_TTL);
        self.send_signaling(topic_id, &msg, options).await?;

        debug!(
            request_id = %hex::encode(&request_id[..8]),
            topic = %hex::encode(&topic_id[..8]),
            peer = %hex::encode(&peer_id[..8]),
            name = %name,
            "stream request sent"
        );

        Ok(request_id)
    }

    /// Accept an incoming stream request (destination side)
    pub async fn accept_stream(
        &self,
        request_id: &[u8; 32],
    ) -> Result<(), LiveError> {
        let pending = self.pending_incoming.write().await.remove(request_id)
            .ok_or(LiveError::RequestNotFound)?;

        let msg = TopicMessage::StreamAccept(StreamAcceptMessage {
            request_id: *request_id,
        });
        self.send_signaling(&pending.topic_id, &msg, SendOptions::direct_only()).await?;

        info!(request_id = %hex::encode(&request_id[..8]), "stream request accepted");
        Ok(())
    }

    /// Reject an incoming stream request (destination side)
    pub async fn reject_stream(
        &self,
        request_id: &[u8; 32],
        reason: Option<String>,
    ) -> Result<(), LiveError> {
        let pending = self.pending_incoming.write().await.remove(request_id)
            .ok_or(LiveError::RequestNotFound)?;

        let msg = TopicMessage::StreamReject(StreamRejectMessage {
            request_id: *request_id,
            reason: reason.clone(),
        });
        self.send_signaling(&pending.topic_id, &msg, SendOptions::direct_only()).await?;

        info!(request_id = %hex::encode(&request_id[..8]), reason = ?reason, "stream request rejected");
        Ok(())
    }

    /// End an active stream (source side)
    pub async fn end_stream(
        &self,
        request_id: &[u8; 32],
    ) -> Result<(), LiveError> {
        let active = self.active_streams.write().await.remove(request_id)
            .ok_or(LiveError::RequestNotFound)?;

        let msg = TopicMessage::StreamEnded(StreamEndedMessage {
            request_id: *request_id,
        });
        self.send_signaling(&active.topic_id, &msg, SendOptions::direct_only()).await?;

        info!(request_id = %hex::encode(&request_id[..8]), "stream ended");
        Ok(())
    }

    /// Check if a stream is still active (for liveness queries)
    pub async fn is_stream_active(&self, request_id: &[u8; 32]) -> bool {
        self.active_streams.read().await.contains_key(request_id)
    }

    /// Find a pending incoming request from a specific peer
    pub(crate) async fn find_pending_incoming_for_peer(&self, peer_id: &[u8; 32]) -> Option<PendingStream> {
        self.pending_incoming.read().await
            .values()
            .find(|p| p.peer_id == *peer_id)
            .cloned()
    }

    /// Store an active MOQ session
    pub(crate) async fn store_session(&self, request_id: [u8; 32], session: LiveSession) {
        self.sessions.write().await.insert(request_id, Arc::new(session));
    }

    /// Get an active MOQ session by request_id
    pub(crate) async fn get_session(&self, request_id: &[u8; 32]) -> Option<Arc<LiveSession>> {
        self.sessions.read().await.get(request_id).cloned()
    }

    /// Remove an active MOQ session
    pub(crate) async fn remove_session(&self, request_id: &[u8; 32]) {
        self.sessions.write().await.remove(request_id);
    }

    /// Handle an incoming stream signaling message from the Send path
    pub async fn handle_signaling(
        &self,
        msg: &TopicMessage,
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        source: crate::network::send::PacketSource,
    ) {
        match msg {
            TopicMessage::StreamRequest(req) => {
                self.handle_incoming_stream_request(
                    &req.request_id, topic_id, sender_id,
                    &req.name, &req.catalog, source,
                ).await;
            }
            TopicMessage::StreamAccept(accept) => {
                let _ = self.actor_tx.send(LiveActorMessage::StreamAccepted {
                    request_id: accept.request_id,
                }).await;
            }
            TopicMessage::StreamReject(reject) => {
                let _ = self.actor_tx.send(LiveActorMessage::StreamRejected {
                    request_id: reject.request_id,
                    reason: reject.reason.clone(),
                }).await;
            }
            TopicMessage::StreamQuery(query) => {
                let _ = self.actor_tx.send(LiveActorMessage::StreamQuery {
                    request_id: query.request_id,
                    querier_id: sender_id,
                    topic_id: *topic_id,
                }).await;
            }
            TopicMessage::StreamActive(active) => {
                self.emit_deferred_stream_request(&active.request_id).await;
            }
            TopicMessage::StreamEnded(ended) => {
                let _ = self.actor_tx.send(LiveActorMessage::StreamEnded {
                    request_id: ended.request_id,
                }).await;
            }
            _ => {}
        }
    }

    /// Handle an incoming StreamRequest
    async fn handle_incoming_stream_request(
        &self,
        request_id: &[u8; 32],
        topic_id: &[u8; 32],
        sender_id: [u8; 32],
        name: &str,
        catalog: &[u8],
        source: crate::network::send::PacketSource,
    ) {
        let pending = PendingStream {
            request_id: *request_id,
            topic_id: *topic_id,
            peer_id: sender_id,
            name: name.to_string(),
            catalog: catalog.to_vec(),
        };

        match source {
            crate::network::send::PacketSource::Direct => {
                self.pending_incoming.write().await.insert(*request_id, pending.clone());
                self.emit_stream_request_event(&pending).await;
            }
            crate::network::send::PacketSource::HarborPull => {
                self.pending_incoming.write().await.insert(*request_id, pending);

                let msg = TopicMessage::StreamQuery(StreamQueryMessage {
                    request_id: *request_id,
                });
                if let Err(e) = self.send_signaling(topic_id, &msg, SendOptions::direct_only()).await {
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
            topic_id: pending.topic_id,
            peer_id: pending.peer_id,
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

    async fn run_actor(self: Arc<Self>, mut rx: mpsc::Receiver<LiveActorMessage>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                LiveActorMessage::StreamAccepted { request_id } => {
                    if let Some(pending) = self.pending_outgoing.write().await.remove(&request_id) {
                        let active = ActiveStream {
                            request_id,
                            topic_id: pending.topic_id,
                            peer_id: pending.peer_id,
                            name: pending.name,
                        };
                        self.active_streams.write().await.insert(request_id, active);

                        let event = ProtocolEvent::StreamAccepted(
                            crate::protocol::StreamAcceptedEvent { request_id },
                        );
                        let _ = self.event_tx.send(event).await;
                        info!(request_id = %hex::encode(&request_id[..8]), "stream accepted — ready for MOQ connection");
                    }
                }
                LiveActorMessage::StreamRejected { request_id, reason } => {
                    if self.pending_outgoing.write().await.remove(&request_id).is_some() {
                        let event = ProtocolEvent::StreamRejected(
                            crate::protocol::StreamRejectedEvent { request_id, reason: reason.clone() },
                        );
                        let _ = self.event_tx.send(event).await;
                        info!(request_id = %hex::encode(&request_id[..8]), reason = ?reason, "stream rejected");
                    }
                }
                LiveActorMessage::StreamEnded { request_id } => {
                    let was_active = self.active_streams.write().await.remove(&request_id);
                    let was_incoming = self.pending_incoming.write().await.remove(&request_id);

                    if was_active.is_some() || was_incoming.is_some() {
                        let peer_id = was_active
                            .map(|a| a.peer_id)
                            .or(was_incoming.map(|p| p.peer_id))
                            .unwrap_or([0u8; 32]);

                        let event = ProtocolEvent::StreamEnded(
                            crate::protocol::StreamEndedEvent { request_id, peer_id },
                        );
                        let _ = self.event_tx.send(event).await;
                    }
                }
                LiveActorMessage::StreamQuery { request_id, querier_id: _, topic_id } => {
                    self.handle_stream_query(&request_id, &topic_id).await;
                }
            }
        }
    }

    async fn handle_stream_query(&self, request_id: &[u8; 32], topic_id: &[u8; 32]) {
        let is_active = self.active_streams.read().await.contains_key(request_id)
            || self.pending_outgoing.read().await.contains_key(request_id);

        let msg = if is_active {
            TopicMessage::StreamActive(StreamActiveMessage { request_id: *request_id })
        } else {
            TopicMessage::StreamEnded(StreamEndedMessage { request_id: *request_id })
        };

        if let Err(e) = self.send_signaling(topic_id, &msg, SendOptions::direct_only()).await {
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
