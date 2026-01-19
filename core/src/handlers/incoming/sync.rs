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
    /// Handle incoming direct sync connection (InitialSyncResponse)
    pub(crate) async fn handle_sync_connection(
        conn: iroh::endpoint::Connection,
        event_tx: mpsc::Sender<ProtocolEvent>,
        sync_managers: Arc<RwLock<HashMap<[u8; 32], SyncManager>>>,
        sender_id: [u8; 32],
    ) -> Result<(), ProtocolError> {
        // Maximum snapshot size (100MB - generous for large documents)
        const MAX_READ_SIZE: usize = 100 * 1024 * 1024;
        
        trace!(sender = %hex::encode(sender_id), "waiting for sync streams");
        
        loop {
            // Accept unidirectional stream
            let mut recv = match conn.accept_uni().await {
                Ok(r) => r,
                Err(e) => {
                    trace!(error = %e, sender = %hex::encode(sender_id), "sync stream accept ended");
                    break;
                }
            };
            
            // Read message
            let buf = match recv.read_to_end(MAX_READ_SIZE).await {
                Ok(data) => data,
                Err(e) => {
                    debug!(error = %e, "failed to read sync stream");
                    continue;
                }
            };
            
            trace!(bytes = buf.len(), "read from sync stream");
            
            // Decode InitialSyncResponse
            let response = match InitialSyncResponse::decode(&buf) {
                Ok(r) => r,
                Err(e) => {
                    debug!(error = ?e, "failed to decode sync response");
                    continue;
                }
            };
            
            trace!(
                request_id = %hex::encode(&response.request_id[..8]),
                snapshot_size = response.snapshot.len(),
                "received initial sync response"
            );
            
            // Find the manager that requested this
            let mut managers = sync_managers.write().await;
            let mut found_topic = None;
            
            for (topic_id, manager) in managers.iter_mut() {
                match manager.apply_initial_sync_response(&response) {
                    Ok(true) => {
                        found_topic = Some(*topic_id);
                        break;
                    }
                    Ok(false) => continue, // Not for this manager
                    Err(e) => {
                        warn!(
                            topic = %hex::encode(&topic_id[..8]),
                            error = %e,
                            "failed to apply initial sync response"
                        );
                    }
                }
            }
            
            if let Some(topic_id) = found_topic {
                info!(
                    topic = %hex::encode(&topic_id[..8]),
                    snapshot_size = response.snapshot.len(),
                    "initial sync completed"
                );
                
                // Emit event
                let event = ProtocolEvent::SyncInitialized(SyncInitializedEvent {
                    topic_id,
                    snapshot_size: response.snapshot.len(),
                });
                
                if event_tx.send(event).await.is_err() {
                    debug!("event receiver dropped");
                }
            } else {
                debug!(
                    request_id = %hex::encode(&response.request_id[..8]),
                    "received initial sync response for unknown request"
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
