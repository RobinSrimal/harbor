//! Sync protocol incoming handler
//!
//! Handles direct SYNC_ALPN connections for sync transport:
//! - SyncRequest: peer requesting full sync state
//! - SyncResponse: peer providing full sync state
//!
//! This is a pure transport handler - it emits events for the app layer
//! to process. The app layer is responsible for:
//! - Maintaining its own CRDT state
//! - Responding to SyncRequest events via Protocol::respond_sync()

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::network::sync::protocol::InitialSyncResponse;
use crate::protocol::{Protocol, ProtocolEvent, SyncRequestEvent, SyncResponseEvent, ProtocolError};

impl Protocol {
    /// Handle incoming direct sync connection (SyncRequest or SyncResponse)
    ///
    /// Emits events for the app layer to handle:
    /// - SyncRequestEvent: app should call respond_sync() with its CRDT state
    /// - SyncResponseEvent: app should merge the received CRDT state
    pub(crate) async fn handle_sync_connection(
        conn: iroh::endpoint::Connection,
        _endpoint: iroh::Endpoint,
        event_tx: mpsc::Sender<ProtocolEvent>,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        // Maximum snapshot size (100MB - generous for large documents)
        const MAX_READ_SIZE: usize = 100 * 1024 * 1024;
        
        info!(sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: waiting for sync streams");
        
        loop {
            // Accept unidirectional stream
            let mut recv = match conn.accept_uni().await {
                Ok(r) => r,
                Err(e) => {
                    debug!(error = %e, sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: accept_uni ended");
                    break;
                }
            };
            
            // Read message
            let buf = match recv.read_to_end(MAX_READ_SIZE).await {
                Ok(data) => data,
                Err(e) => {
                    warn!(error = %e, sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: failed to read stream");
                    continue;
                }
            };
            
            // Check minimum size: 32 bytes topic_id + at least 1 byte message type
            if buf.len() < 33 {
                warn!(len = buf.len(), sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: message too short");
                continue;
            }
            
            // First 32 bytes are the topic_id
            let mut topic_id = [0u8; 32];
            topic_id.copy_from_slice(&buf[..32]);
            let message_bytes = &buf[32..];
            
            info!(
                topic = %hex::encode(&topic_id[..8]),
                sender = %hex::encode(&sender_id[..8]),
                message_len = message_bytes.len(),
                "SYNC_HANDLER: received message"
            );
            
            // Message type byte determines what kind of message this is
            // 0x01 = SyncRequest (empty payload, just requesting state)
            // 0x02 = SyncResponse (contains CRDT snapshot)
            if message_bytes.is_empty() {
                warn!(
                    topic = %hex::encode(&topic_id[..8]),
                    sender = %hex::encode(&sender_id[..8]),
                    "SYNC_HANDLER: empty message payload"
                );
                continue;
            }
            
            match message_bytes[0] {
                0x01 => {
                    // SyncRequest - peer is requesting our state
                    info!(
                        topic = %hex::encode(&topic_id[..8]),
                        sender = %hex::encode(&sender_id[..8]),
                        "SYNC_HANDLER: received SyncRequest"
                    );
                    
                    // Emit event for app to handle
                    let event = ProtocolEvent::SyncRequest(SyncRequestEvent {
                        topic_id,
                        sender_id,
                    });
                    
                    if event_tx.send(event).await.is_err() {
                        debug!("event receiver dropped");
                    }
                }
                0x02 => {
                    // SyncResponse - peer is providing their state
                    let data = message_bytes[1..].to_vec();
                    
                    info!(
                        topic = %hex::encode(&topic_id[..8]),
                        sender = %hex::encode(&sender_id[..8]),
                        data_len = data.len(),
                        "SYNC_HANDLER: received SyncResponse"
                    );
                    
                    // Emit event for app to handle
                    let event = ProtocolEvent::SyncResponse(SyncResponseEvent {
                        topic_id,
                        data,
                    });
                    
                    if event_tx.send(event).await.is_err() {
                        debug!("event receiver dropped");
                    }
                }
                _ => {
                    // Try legacy format (InitialSyncResponse)
                    if let Ok(response) = InitialSyncResponse::decode(message_bytes) {
                        info!(
                            topic = %hex::encode(&topic_id[..8]),
                            sender = %hex::encode(&sender_id[..8]),
                            snapshot_len = response.snapshot.len(),
                            "SYNC_HANDLER: received legacy InitialSyncResponse"
                        );
                        
                        let event = ProtocolEvent::SyncResponse(SyncResponseEvent {
                            topic_id,
                            data: response.snapshot,
                        });
                        
                        if event_tx.send(event).await.is_err() {
                            debug!("event receiver dropped");
                        }
                    } else {
                        debug!(
                            topic = %hex::encode(&topic_id[..8]),
                            first_byte = message_bytes[0],
                            "SYNC_HANDLER: unknown message type"
                        );
                    }
                }
            }
        }
        
        Ok(())
    }
}
