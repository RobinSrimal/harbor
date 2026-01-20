//! Sync flush task
//!
//! Periodically flushes batched sync updates to the network and saves snapshots.
//!
//! Design:
//! - Runs every ~1 second (configurable)
//! - For each topic with pending changes in SyncManager:
//!   - Generates batched update message
//!   - Sends via Protocol.send() to topic members
//! - Periodically saves snapshots (every N minutes)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, trace, warn};

use crate::protocol::sync::SyncManager;

/// Default flush interval (1 second)
#[allow(dead_code)]
pub const DEFAULT_SYNC_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Default snapshot interval (5 minutes)
#[allow(dead_code)]
pub const DEFAULT_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Run the sync flush task
///
/// This task:
/// 1. Periodically checks for sync managers with pending local changes
/// 2. Generates batched updates and broadcasts them to topic members
/// 3. Periodically saves snapshots for compaction
pub async fn run_sync_flush_task(
    db: Arc<Mutex<Connection>>,
    endpoint: Endpoint,
    sync_managers: Arc<RwLock<HashMap<[u8; 32], SyncManager>>>,
    our_private_key: [u8; 32],
    our_public_key: [u8; 32],
    running: Arc<RwLock<bool>>,
    flush_interval: Duration,
    snapshot_interval: Duration,
) {
    info!(
        flush_interval_ms = flush_interval.as_millis(),
        snapshot_interval_secs = snapshot_interval.as_secs(),
        "Sync flush task started"
    );
    
    let mut flush_timer = tokio::time::interval(flush_interval);
    let mut snapshot_timer = tokio::time::interval(snapshot_interval);
    
    // Skip immediate ticks
    flush_timer.tick().await;
    snapshot_timer.tick().await;
    
    let mut snapshots_since_last = 0u64;
    
    loop {
        tokio::select! {
            _ = flush_timer.tick() => {
                // Check if still running
                if !*running.read().await {
                    break;
                }
                
                // Flush managers with pending changes
                let updates = {
                    let mut managers = sync_managers.write().await;
                    let mut updates = Vec::new();
                    
                    let manager_count = managers.len();
                    if manager_count > 0 {
                        trace!(
                            manager_count = manager_count,
                            "sync flush: checking managers"
                        );
                    }
                    
                    for (topic_id, manager) in managers.iter_mut() {
                        let has_pending = manager.has_pending_changes();
                        let should_flush = manager.should_flush();
                        
                        if has_pending {
                            debug!(
                                topic = %hex::encode(&topic_id[..8]),
                                has_pending_changes = has_pending,
                                should_flush = should_flush,
                                "SYNC: checking manager for flush"
                            );
                        }
                        
                        if should_flush {
                            if let Some(update) = manager.generate_update() {
                                debug!(
                                    topic = %hex::encode(&topic_id[..8]),
                                    "SYNC: generated update for broadcast"
                                );
                                updates.push((*topic_id, update));
                            }
                        }
                    }
                    
                    updates
                };
                
                // Broadcast updates
                for (topic_id, update) in updates {
                    // Extract the data from the SyncMessage and wrap in TopicMessage
                    let data = match update {
                        crate::network::sync::protocol::SyncMessage::Update(u) => u.data,
                        _ => continue, // Should only be Update variants from generate_update
                    };
                    
                    // Encode as TopicMessage::SyncUpdate
                    let topic_msg = crate::network::membership::messages::TopicMessage::SyncUpdate(
                        crate::network::membership::messages::SyncUpdateMessage { data }
                    );
                    let encoded = topic_msg.encode();
                    
                    // Get topic members
                    let members = {
                        let db_lock = db.lock().await;
                        crate::data::get_topic_members(&db_lock, &topic_id)
                            .unwrap_or_default()
                    };
                    
                    if members.is_empty() {
                        warn!(
                            topic = %hex::encode(&topic_id[..8]),
                            "SYNC: no members found in DB to broadcast to"
                        );
                        continue;
                    }
                    
                    // Log member IDs for debugging
                    let member_ids: Vec<String> = members.iter()
                        .map(|m| hex::encode(&m[..8]))
                        .collect();
                    info!(
                        topic = %hex::encode(&topic_id[..8]),
                        update_size = encoded.len(),
                        member_count = members.len(),
                        members = ?member_ids,
                        "SYNC: broadcasting update to members"
                    );
                    
                    // Send to each member (excluding ourselves)
                    for member_id in members {
                        if member_id == our_public_key {
                            continue;
                        }
                        
                        // Use the send protocol to broadcast
                        // This reuses the existing send infrastructure
                        match send_sync_update(
                            &endpoint,
                            &topic_id,
                            &member_id,
                            &encoded,
                            &our_private_key,
                            &our_public_key,
                        ).await {
                            Ok(()) => {
                                info!(
                                    topic = %hex::encode(&topic_id[..8]),
                                    member = %hex::encode(&member_id[..8]),
                                    "SYNC: sent update to member"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    topic = %hex::encode(&topic_id[..8]),
                                    member = %hex::encode(&member_id[..8]),
                                    error = %e,
                                    "SYNC: failed to send update"
                                );
                            }
                        }
                    }
                }
            }
            _ = snapshot_timer.tick() => {
                // Check if still running
                if !*running.read().await {
                    break;
                }
                
                // Save snapshots for all managers
                let mut managers = sync_managers.write().await;
                let mut saved = 0;
                
                for (topic_id, manager) in managers.iter_mut() {
                    if let Err(e) = manager.save_snapshot() {
                        warn!(
                            topic = %hex::encode(&topic_id[..8]),
                            error = %e,
                            "failed to save sync snapshot"
                        );
                    } else {
                        saved += 1;
                    }
                }
                
                if saved > 0 {
                    snapshots_since_last += saved as u64;
                    debug!(
                        saved = saved,
                        total = snapshots_since_last,
                        "saved sync snapshots"
                    );
                }
            }
        }
    }
    
    // Save final snapshots before stopping
    {
        let mut managers = sync_managers.write().await;
        for (topic_id, manager) in managers.iter_mut() {
            if let Err(e) = manager.save_snapshot() {
                warn!(
                    topic = %hex::encode(&topic_id[..8]),
                    error = %e,
                    "failed to save sync snapshot on shutdown"
                );
            }
        }
    }
    
    info!("Sync flush task stopped");
}

/// Send a sync update to a specific member
async fn send_sync_update(
    endpoint: &Endpoint,
    topic_id: &[u8; 32],
    recipient_id: &[u8; 32],
    encoded_update: &[u8],
    our_private_key: &[u8; 32],
    our_public_key: &[u8; 32],
) -> Result<(), String> {
    use crate::security::create_packet;
    use crate::network::send::protocol::{SendMessage, SEND_ALPN};
    
    // Create packet (encrypts for topic)
    let packet = create_packet(topic_id, our_private_key, our_public_key, encoded_update)
        .map_err(|e| format!("failed to create packet: {}", e))?;
    
    // Encode as SendMessage
    let message = SendMessage::Packet(packet);
    let wire_bytes = message.encode();
    
    // Get connection to recipient
    let node_id = iroh::NodeId::from_bytes(recipient_id)
        .map_err(|e| format!("invalid node id: {}", e))?;
    
    // Try to connect with timeout (peer might be offline)
    let conn = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        endpoint.connect(node_id, SEND_ALPN)
    ).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Err(format!("connection failed: {}", e)),
        Err(_) => return Err("connection timeout (peer may be offline)".to_string()),
    };
    
    let mut send = conn
        .open_uni()
        .await
        .map_err(|e| format!("failed to open stream: {}", e))?;
    
    tokio::io::AsyncWriteExt::write_all(&mut send, &wire_bytes)
        .await
        .map_err(|e| format!("failed to write: {}", e))?;
    
    send.finish()
        .map_err(|e| format!("failed to finish stream: {}", e))?;
    
    // Wait for the stream to be fully acknowledged with timeout
    // Don't hang forever if recipient is offline
    let timeout = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        send.stopped()
    ).await;
    
    match timeout {
        Ok(Ok(_)) => {
            trace!(
                topic = %hex::encode(&topic_id[..8]),
                recipient = %hex::encode(&recipient_id[..8]),
                size = wire_bytes.len(),
                "sent sync update (acknowledged)"
            );
        }
        Ok(Err(e)) => {
            // Stream error but data was likely sent
            trace!(
                topic = %hex::encode(&topic_id[..8]),
                recipient = %hex::encode(&recipient_id[..8]),
                error = %e,
                "sync update stream error (data likely sent)"
            );
        }
        Err(_) => {
            // Timeout - recipient might be offline
            trace!(
                topic = %hex::encode(&topic_id[..8]),
                recipient = %hex::encode(&recipient_id[..8]),
                "sync update timeout (recipient may be offline)"
            );
        }
    }
    
    Ok(())
}
