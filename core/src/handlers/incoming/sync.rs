//! Sync protocol incoming handler
//!
//! Sync transport:
//! - SyncUpdate (deltas): via Send protocol - small, needs offline delivery
//! - InitialSyncRequest: via Send protocol - announces need for full state
//! - InitialSyncResponse: via direct SYNC_ALPN connection - large snapshot, no size limit
//!
//! This handler processes:
//! - Direct SYNC_ALPN connections (InitialSyncResponse from other members)
//! - Sync messages via Send protocol (forwarded from send.rs via handle_sync_message)

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, trace, warn};

use crate::network::sync::protocol::{
    SyncMessage, InitialSyncResponse, SYNC_MESSAGE_PREFIX,
};
use crate::protocol::sync::SyncManager;
use crate::protocol::{Protocol, ProtocolEvent, SyncUpdatedEvent, SyncInitializedEvent, ProtocolError};

impl Protocol {
    /// Handle incoming direct sync connection (InitialSyncRequest or InitialSyncResponse)
    pub(crate) async fn handle_sync_connection(
        conn: iroh::endpoint::Connection,
        endpoint: iroh::Endpoint,
        event_tx: mpsc::Sender<ProtocolEvent>,
        sync_managers: Arc<RwLock<HashMap<[u8; 32], SyncManager>>>,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        use crate::network::sync::protocol::SyncMessage;
        
        // Maximum snapshot size (100MB - generous for large documents)
        const MAX_READ_SIZE: usize = 100 * 1024 * 1024;
        
        info!(sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: waiting for sync streams");
        
        loop {
            // Accept unidirectional stream
            info!(sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: calling accept_uni()");
            let mut recv = match conn.accept_uni().await {
                Ok(r) => {
                    info!(sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: stream accepted");
                    r
                }
                Err(e) => {
                    info!(error = %e, sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: accept_uni ended");
                    break;
                }
            };
            
            // Read message
            info!(sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: reading stream data");
            let buf = match recv.read_to_end(MAX_READ_SIZE).await {
                Ok(data) => {
                    info!(sender = %hex::encode(&sender_id[..8]), bytes = data.len(), "SYNC_HANDLER: read complete");
                    data
                }
                Err(e) => {
                    warn!(error = %e, sender = %hex::encode(&sender_id[..8]), "SYNC_HANDLER: failed to read stream");
                    continue;
                }
            };
            
            // Check minimum size: 32 bytes topic_id + at least 1 byte message
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
                first_byte = ?message_bytes.first(),
                "SYNC_HANDLER: parsing message"
            );
            
            // Try to decode as InitialSyncRequest first
            if let Ok(request) = SyncMessage::decode(message_bytes) {
                info!(
                    topic = %hex::encode(&topic_id[..8]),
                    sender = %hex::encode(&sender_id[..8]),
                    "SYNC_HANDLER: decoded as SyncMessage"
                );
                if let SyncMessage::InitialSyncRequest(req) = request {
                    info!(
                        topic = %hex::encode(&topic_id[..8]),
                        sender = %hex::encode(&sender_id[..8]),
                        request_id = %hex::encode(&req.request_id[..8]),
                        "SYNC_HANDLER: received InitialSyncRequest"
                    );
                    
                    // Generate response from our manager
                    let response = {
                        let managers = sync_managers.read().await;
                        if let Some(manager) = managers.get(&topic_id) {
                            Some(manager.handle_initial_sync_request(&req))
                        } else {
                            None
                        }
                    };
                    
                    if let Some(response) = response {
                        // Send response back via a new direct connection
                        if let Err(e) = Self::send_initial_sync_response(
                            &endpoint,
                            &topic_id,
                            &sender_id,
                            &response,
                        ).await {
                            warn!(
                                topic = %hex::encode(&topic_id[..8]),
                                error = %e,
                                "Failed to send initial sync response"
                            );
                        } else {
                            info!(
                                topic = %hex::encode(&topic_id[..8]),
                                sender = %hex::encode(&sender_id[..8]),
                                snapshot_size = response.snapshot.len(),
                                "Sent initial sync response"
                            );
                        }
                    } else {
                        debug!(
                            topic = %hex::encode(&topic_id[..8]),
                            "No sync manager for topic, cannot respond to initial sync request"
                        );
                    }
                    continue;
                }
            }
            
            // Try to decode as InitialSyncResponse
            let response = match InitialSyncResponse::decode(message_bytes) {
                Ok(r) => r,
                Err(e) => {
                    debug!(error = ?e, "failed to decode sync message");
                    continue;
                }
            };
            
            trace!(
                request_id = %hex::encode(&response.request_id[..8]),
                snapshot_size = response.snapshot.len(),
                "received initial sync response"
            );
            
            // Apply the response to the correct manager
            let mut managers = sync_managers.write().await;
            if let Some(manager) = managers.get_mut(&topic_id) {
                match manager.apply_initial_sync_response(&response) {
                    Ok(true) => {
                        info!(
                            topic = %hex::encode(&topic_id[..8]),
                            snapshot_size = response.snapshot.len(),
                            "Initial sync completed"
                        );
                        
                        // Emit event
                        let event = ProtocolEvent::SyncInitialized(SyncInitializedEvent {
                            topic_id,
                            snapshot_size: response.snapshot.len(),
                        });
                        
                        if event_tx.send(event).await.is_err() {
                            debug!("event receiver dropped");
                        }
                    }
                    Ok(false) => {
                        debug!(
                            topic = %hex::encode(&topic_id[..8]),
                            request_id = %hex::encode(&response.request_id[..8]),
                            "Received initial sync response for unknown request"
                        );
                    }
                    Err(e) => {
                        warn!(
                            topic = %hex::encode(&topic_id[..8]),
                            error = %e,
                            "Failed to apply initial sync response"
                        );
                    }
                }
            } else {
                debug!(
                    topic = %hex::encode(&topic_id[..8]),
                    "Received initial sync response but no manager exists"
                );
            }
        }
        
        Ok(())
    }
    
    /// Send an initial sync response to a requester
    async fn send_initial_sync_response(
        endpoint: &iroh::Endpoint,
        topic_id: &[u8; 32],
        recipient_id: &[u8; 32],
        response: &InitialSyncResponse,
    ) -> Result<(), String> {
        use crate::network::sync::SYNC_ALPN;
        use std::time::Duration;
        
        let node_id = iroh::NodeId::from_bytes(recipient_id)
            .map_err(|e| format!("invalid node id: {}", e))?;
        
        // Encode the response
        let encoded = response.encode();
        
        info!(
            topic = %hex::encode(&topic_id[..8]),
            recipient = %hex::encode(&recipient_id[..8]),
            encoded_len = encoded.len(),
            "SYNC_RESPONSE: connecting to requester"
        );
        
        // Connect via SYNC_ALPN (with timeout for offline peers)
        let conn = match tokio::time::timeout(
            Duration::from_secs(5),
            endpoint.connect(node_id, SYNC_ALPN)
        ).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return Err(format!("connection failed: {}", e)),
            Err(_) => return Err("connection timeout (peer may be offline)".to_string()),
        };
        
        info!(
            topic = %hex::encode(&topic_id[..8]),
            recipient = %hex::encode(&recipient_id[..8]),
            "SYNC_RESPONSE: connection established"
        );
        
        let mut send = conn
            .open_uni()
            .await
            .map_err(|e| format!("failed to open stream: {}", e))?;
        
        // Write topic_id first
        tokio::io::AsyncWriteExt::write_all(&mut send, topic_id)
            .await
            .map_err(|e| format!("failed to write topic_id: {}", e))?;
        
        // Write the response
        tokio::io::AsyncWriteExt::write_all(&mut send, &encoded)
            .await
            .map_err(|e| format!("failed to write response: {}", e))?;
        
        info!(
            topic = %hex::encode(&topic_id[..8]),
            recipient = %hex::encode(&recipient_id[..8]),
            total_bytes = 32 + encoded.len(),
            "SYNC_RESPONSE: data written, finishing stream"
        );
        
        send.finish()
            .map_err(|e| format!("failed to finish stream: {}", e))?;
        
        // Wait for acknowledgment with timeout
        let timeout = tokio::time::timeout(
            Duration::from_secs(10),
            send.stopped()
        ).await;
        
        match timeout {
            Ok(Ok(_)) => {
                info!(
                    topic = %hex::encode(&topic_id[..8]),
                    recipient = %hex::encode(&recipient_id[..8]),
                    "SYNC_RESPONSE: sent successfully"
                );
            }
            Ok(Err(e)) => {
                warn!(
                    topic = %hex::encode(&topic_id[..8]),
                    recipient = %hex::encode(&recipient_id[..8]),
                    error = %e,
                    "SYNC_RESPONSE: stream error (data likely sent)"
                );
            }
            Err(_) => {
                info!(
                    topic = %hex::encode(&topic_id[..8]),
                    recipient = %hex::encode(&recipient_id[..8]),
                    "SYNC_RESPONSE: sent (no ack within timeout)"
                );
            }
        }
        
        Ok(())
    }
    
    /// Handle incoming sync message from Send protocol
    ///
    /// Called by send.rs when a message with SYNC_MESSAGE_PREFIX is detected.
    #[allow(dead_code)]
    pub(crate) async fn handle_sync_message(
        event_tx: &mpsc::Sender<ProtocolEvent>,
        sync_managers: &Arc<RwLock<HashMap<[u8; 32], SyncManager>>>,
        topic_id: [u8; 32],
        sender_id: [u8; 32],
        payload: &[u8],
    ) -> Result<(), ProtocolError> {
        // Decode sync message
        let message = SyncMessage::decode(payload)
            .map_err(|e| ProtocolError::Sync(e.to_string()))?;
        
        match message {
            SyncMessage::Update(update) => {
                trace!(
                    topic = %hex::encode(&topic_id[..8]),
                    sender = %hex::encode(&sender_id[..8]),
                    update_size = update.data.len(),
                    "received sync update"
                );
                
                // Get sync manager for this topic
                let mut managers = sync_managers.write().await;
                
                if let Some(manager) = managers.get_mut(&topic_id) {
                    // Apply update
                    manager.apply_update(&update)
                        .map_err(|e| ProtocolError::Sync(e.to_string()))?;
                    
                    // Emit event
                    let event = ProtocolEvent::SyncUpdated(SyncUpdatedEvent {
                        topic_id,
                        sender_id,
                        update_size: update.data.len(),
                    });
                    
                    if event_tx.send(event).await.is_err() {
                        debug!("event receiver dropped");
                    }
                } else {
                    // No sync manager yet - this might be an update before initial sync
                    debug!(
                        topic = %hex::encode(&topic_id[..8]),
                        "received sync update but no manager - ignoring until initial sync"
                    );
                }
            }
            SyncMessage::InitialSyncRequest(request) => {
                trace!(
                    topic = %hex::encode(&topic_id[..8]),
                    sender = %hex::encode(&sender_id[..8]),
                    request_id = %hex::encode(&request.request_id[..8]),
                    "received initial sync request"
                );
                
                // Get sync manager for this topic
                let managers = sync_managers.read().await;
                
                if let Some(manager) = managers.get(&topic_id) {
                    // Generate response
                    let response = manager.handle_initial_sync_request(&request);
                    
                    // The response needs to be sent via direct connection
                    // This is handled by the outgoing sync handler
                    info!(
                        topic = %hex::encode(&topic_id[..8]),
                        request_id = %hex::encode(&request.request_id[..8]),
                        snapshot_size = response.snapshot.len(),
                        target = %hex::encode(&sender_id[..8]),
                        "generated initial sync response (direct send pending)"
                    );
                    
                    // Note: Sending response requires outgoing SYNC_ALPN connection.
                    // For v1, initial sync can be triggered manually by the app layer
                    // or via the SyncManager API. Full auto-response would need
                    // outgoing connection infrastructure similar to send.rs.
                } else {
                    debug!(
                        topic = %hex::encode(&topic_id[..8]),
                        "received initial sync request but no manager"
                    );
                }
            }
        }
        
        Ok(())
    }
}

/// Check if bytes are a sync message (starts with SYNC_MESSAGE_PREFIX)
#[allow(dead_code)]
pub fn is_sync_message(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes[0] == SYNC_MESSAGE_PREFIX
}

/// Decode a sync message from bytes
#[allow(dead_code)]
pub fn decode_sync_message(bytes: &[u8]) -> Result<SyncMessage, String> {
    SyncMessage::decode(bytes).map_err(|e| e.to_string())
}

/// Decode an initial sync response from bytes (direct connection)
#[allow(dead_code)]
pub fn decode_initial_sync_response(bytes: &[u8]) -> Result<InitialSyncResponse, String> {
    InitialSyncResponse::decode(bytes).map_err(|e| e.to_string())
}
