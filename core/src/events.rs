//! Protocol event handling for CLI

use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use harbor_core::{Protocol, ProtocolEvent};

/// Shared state for active stream tracks (source side)
pub type ActiveTracks = Arc<RwLock<HashMap<[u8; 32], moq_lite::TrackProducer>>>;

/// Run the event processing loop
pub async fn run_event_loop(protocol: Arc<Protocol>, active_tracks: ActiveTracks) {
    let mut events = protocol.events().await;

    if let Some(ref mut rx) = events {
        while let Some(event) = rx.recv().await {
            handle_event(protocol.clone(), active_tracks.clone(), event).await;
        }
    }
}

async fn handle_event(protocol: Arc<Protocol>, active_tracks: ActiveTracks, event: ProtocolEvent) {
    match event {
        ProtocolEvent::Message(msg) => {
            // Try to decode payload as UTF-8 for display
            let content = String::from_utf8_lossy(&msg.payload);
            info!(
                topic = %hex::encode(&msg.topic_id[..8]),
                sender = %hex::encode(&msg.sender_id[..8]),
                size = msg.payload.len(),
                content = %content,
                "Received message"
            );
        }
        ProtocolEvent::FileAnnounced(ev) => {
            info!(
                topic = %hex::encode(&ev.topic_id[..8]),
                source = %hex::encode(&ev.source_id[..8]),
                hash = %hex::encode(&ev.hash[..8]),
                name = %ev.display_name,
                size = ev.total_size,
                "File announced"
            );
        }
        ProtocolEvent::FileProgress(ev) => {
            debug!(
                hash = %hex::encode(&ev.hash[..8]),
                progress = %format!("{}/{}", ev.chunks_complete, ev.total_chunks),
                "File progress"
            );
        }
        ProtocolEvent::FileComplete(ev) => {
            info!(
                hash = %hex::encode(&ev.hash[..8]),
                name = %ev.display_name,
                size = ev.total_size,
                "File complete"
            );
        }
        ProtocolEvent::SyncUpdate(ev) => {
            debug!(
                topic = %hex::encode(&ev.topic_id[..8]),
                sender = %hex::encode(&ev.sender_id[..8]),
                size = ev.data.len(),
                "Sync update received"
            );
        }
        ProtocolEvent::SyncRequest(ev) => {
            debug!(
                topic = %hex::encode(&ev.topic_id[..8]),
                sender = %hex::encode(&ev.sender_id[..8]),
                "Sync request received"
            );
        }
        ProtocolEvent::SyncResponse(ev) => {
            info!(
                topic = %hex::encode(&ev.topic_id[..8]),
                size = ev.data.len(),
                "Sync response received"
            );
        }
        ProtocolEvent::StreamRequest(ev) => {
            info!(
                request_id = %hex::encode(&ev.request_id[..8]),
                name = %ev.name,
                "Stream request received"
            );
            // Auto-accept stream requests (for simulation testing)
            let p = Arc::clone(&protocol);
            let rid = ev.request_id;
            tokio::spawn(async move {
                match p.accept_stream(&rid).await {
                    Ok(()) => info!(request_id = %hex::encode(&rid[..8]), "Stream auto-accepted"),
                    Err(e) => warn!(request_id = %hex::encode(&rid[..8]), error = %e, "Stream auto-accept failed"),
                }
            });
        }
        ProtocolEvent::StreamAccepted(ev) => {
            info!(request_id = %hex::encode(&ev.request_id[..8]), "Stream accepted");
        }
        ProtocolEvent::StreamRejected(ev) => {
            info!(request_id = %hex::encode(&ev.request_id[..8]), "Stream rejected");
        }
        ProtocolEvent::StreamEnded(ev) => {
            // Clean up stored track producer
            active_tracks.write().await.remove(&ev.request_id);
            info!(request_id = %hex::encode(&ev.request_id[..8]), "Stream ended");
        }
        ProtocolEvent::DmReceived(ev) => {
            let content = String::from_utf8_lossy(&ev.payload);
            info!(
                sender = %hex::encode(&ev.sender_id[..8]),
                size = ev.payload.len(),
                content = %content,
                "DM received"
            );
        }
        ProtocolEvent::StreamConnected(ev) => {
            info!(
                request_id = %hex::encode(&ev.request_id[..8]),
                is_source = ev.is_source,
                "MOQ stream connected"
            );
            // Source side: create broadcast and wait for subscriber's track request
            if ev.is_source {
                let p = Arc::clone(&protocol);
                let rid = ev.request_id;
                let tracks = Arc::clone(&active_tracks);
                tokio::spawn(async move {
                    match p.publish_to_stream(&rid, "test").await {
                        Ok(mut broadcast) => {
                            // Wait for the destination to subscribe to the "data" track.
                            // The subscriber creates a TrackProducer via requested_track()
                            // which we use to write frames.
                            if let Some(track) = broadcast.requested_track().await {
                                info!(
                                    request_id = %hex::encode(&rid[..8]),
                                    track = %track.info.name,
                                    "Track requested by subscriber"
                                );
                                tracks.write().await.insert(rid, track);
                            } else {
                                warn!(request_id = %hex::encode(&rid[..8]), "Broadcast closed before track requested");
                            }
                            // Keep broadcast alive for the stream's lifetime
                            let _broadcast = broadcast;
                            tokio::sync::Notify::new().notified().await;
                        }
                        Err(e) => {
                            warn!(request_id = %hex::encode(&rid[..8]), error = %e, "Failed to create broadcast for stream");
                        }
                    }
                });
            }
            // Destination side: spawn consumer to read incoming data
            if !ev.is_source {
                let p = Arc::clone(&protocol);
                let rid = ev.request_id;
                tokio::spawn(async move {
                    // Retry consume_stream — the source may not have announced
                    // the broadcast yet when we first connect
                    let broadcast = {
                        let mut result = None;
                        for attempt in 0..10 {
                            match p.consume_stream(&rid, "test").await {
                                Ok(b) => { result = Some(b); break; }
                                Err(_) if attempt < 9 => {
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                }
                                Err(e) => {
                                    warn!(request_id = %hex::encode(&rid[..8]), error = %e, "Stream consume failed after retries");
                                    return;
                                }
                            }
                        }
                        result.unwrap()
                    };
                    info!(request_id = %hex::encode(&rid[..8]), "Stream consumer started");
                    let mut track = broadcast.subscribe_track(&moq_lite::Track { name: "data".to_string(), priority: 0 });
                    while let Ok(Some(mut group)) = track.next_group().await {
                        while let Ok(Some(frame)) = group.read_frame().await {
                            info!(
                                request_id = %hex::encode(&rid[..8]),
                                size = frame.len(),
                                data = %hex::encode(&frame),
                                "Stream data received"
                            );
                        }
                    }
                    info!(request_id = %hex::encode(&rid[..8]), "Stream consumer ended");
                });
            }
        }
        ProtocolEvent::DmSyncUpdate(ev) => {
            info!(sender = %hex::encode(&ev.sender_id[..8]), "DM sync update received");
        }
        ProtocolEvent::DmSyncRequest(ev) => {
            info!(sender = %hex::encode(&ev.sender_id[..8]), "DM sync request received");
        }
        ProtocolEvent::DmSyncResponse(ev) => {
            info!(sender = %hex::encode(&ev.sender_id[..8]), "DM sync response received");
        }
        ProtocolEvent::DmFileAnnounced(ev) => {
            info!(
                sender = %hex::encode(&ev.sender_id[..8]),
                hash = %hex::encode(&ev.hash[..8]),
                name = %ev.display_name,
                "DM file announced"
            );
        }
        // Control events
        ProtocolEvent::ConnectionRequest(ev) => {
            info!(
                peer = %hex::encode(&ev.peer_id[..8]),
                request_id = %hex::encode(&ev.request_id[..8]),
                display_name = ?ev.display_name,
                "CONNECTION_REQUEST received"
            );
        }
        ProtocolEvent::ConnectionAccepted(ev) => {
            info!(
                peer = %hex::encode(&ev.peer_id[..8]),
                request_id = %hex::encode(&ev.request_id[..8]),
                "CONNECTION_ACCEPTED"
            );
        }
        ProtocolEvent::ConnectionDeclined(ev) => {
            info!(
                peer = %hex::encode(&ev.peer_id[..8]),
                request_id = %hex::encode(&ev.request_id[..8]),
                reason = ?ev.reason,
                "CONNECTION_DECLINED"
            );
        }
        ProtocolEvent::TopicInviteReceived(ev) => {
            info!(
                sender = %hex::encode(&ev.sender_id[..8]),
                topic = %hex::encode(&ev.topic_id[..8]),
                message_id = %hex::encode(&ev.message_id[..8]),
                topic_name = ?ev.topic_name,
                member_count = ev.member_count,
                "TOPIC_INVITE_RECEIVED"
            );
        }
        ProtocolEvent::TopicMemberJoined(ev) => {
            info!(
                topic = %hex::encode(&ev.topic_id[..8]),
                member = %hex::encode(&ev.member_id[..8]),
                "TOPIC_MEMBER_JOINED"
            );
        }
        ProtocolEvent::TopicMemberLeft(ev) => {
            info!(
                topic = %hex::encode(&ev.topic_id[..8]),
                member = %hex::encode(&ev.member_id[..8]),
                "TOPIC_MEMBER_LEFT"
            );
        }
        ProtocolEvent::TopicEpochRotated(ev) => {
            info!(
                topic = %hex::encode(&ev.topic_id[..8]),
                new_epoch = ev.new_epoch,
                removed_member = %hex::encode(&ev.removed_member[..8]),
                "TOPIC_EPOCH_ROTATED"
            );
        }
        ProtocolEvent::PeerSuggested(ev) => {
            info!(
                introducer = %hex::encode(&ev.introducer_id[..8]),
                suggested = %hex::encode(&ev.suggested_peer_id[..8]),
                note = ?ev.note,
                "PEER_SUGGESTED"
            );
        }
    }
}
