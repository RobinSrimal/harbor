//! Tauri app integration with Harbor Protocol

use std::sync::Arc;
use std::collections::HashMap;
use tauri::State;
use tokio::sync::{Mutex, mpsc};
use serde::Serialize;

use harbor_core::{
    Protocol, ProtocolConfig, TopicInvite, Target,
    ProtocolStats, DhtBucketInfo, TopicDetails, TopicSummary,
    ProtocolEvent,
};

mod video;

/// Shared protocol state
pub struct AppState {
    protocol: Arc<Mutex<Option<Protocol>>>,
    event_rx: Arc<Mutex<Option<mpsc::Receiver<ProtocolEvent>>>>,
    frontend_log_path: Arc<Mutex<Option<std::path::PathBuf>>>,
    video_pipelines: Arc<Mutex<HashMap<String, video::pipeline::VideoPipeline>>>,
    video_consumers: Arc<Mutex<HashMap<String, video::consumer::VideoConsumer>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            protocol: Arc::new(Mutex::new(None)),
            event_rx: Arc::new(Mutex::new(None)),
            frontend_log_path: Arc::new(Mutex::new(None)),
            video_pipelines: Arc::new(Mutex::new(HashMap::new())),
            video_consumers: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Response types for the frontend
#[derive(Serialize)]
pub struct StartResponse {
    endpoint_id: String,
}

#[derive(Serialize)]
pub struct TopicResponse {
    topic_id: String,
    members: Vec<String>,
    invite_hex: String,
}

#[derive(Serialize, Clone)]
pub struct MessageEvent {
    topic_id: String,
    sender_id: String,
    payload: String,
    timestamp: i64,
}

/// File announced event for frontend
#[derive(Serialize, Clone)]
pub struct FileAnnouncedEventFE {
    topic_id: String,
    source_id: String,
    hash: String,
    display_name: String,
    total_size: u64,
    total_chunks: u32,
    timestamp: i64,
}

/// File progress event for frontend
#[derive(Serialize, Clone)]
pub struct FileProgressEventFE {
    hash: String,
    chunks_complete: u32,
    total_chunks: u32,
}

/// File complete event for frontend  
#[derive(Serialize, Clone)]
pub struct FileCompleteEventFE {
    hash: String,
    display_name: String,
    total_size: u64,
}

/// Sync update event for frontend (includes raw bytes)
#[derive(Serialize, Clone)]
pub struct SyncUpdateEventFE {
    topic_id: String,
    sender_id: String,
    #[serde(with = "serde_bytes_as_array")]
    data: Vec<u8>,
}

/// Sync request event for frontend
#[derive(Serialize, Clone)]
pub struct SyncRequestEventFE {
    topic_id: String,
    sender_id: String,
}

/// Sync response event for frontend (includes raw bytes)
#[derive(Serialize, Clone)]
pub struct SyncResponseEventFE {
    topic_id: String,
    #[serde(with = "serde_bytes_as_array")]
    data: Vec<u8>,
}

// Helper module for serializing Vec<u8> as array
mod serde_bytes_as_array {
    use serde::{Serializer, Serialize};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        bytes.serialize(serializer)
    }
}

/// Stream request event for frontend
#[derive(Serialize, Clone)]
pub struct StreamRequestEventFE {
    topic_id: Option<String>,
    peer_id: String,
    request_id: String,
    name: String,
}

/// Stream accepted event for frontend
#[derive(Serialize, Clone)]
pub struct StreamAcceptedEventFE {
    request_id: String,
}

/// Stream rejected event for frontend
#[derive(Serialize, Clone)]
pub struct StreamRejectedEventFE {
    request_id: String,
    reason: Option<String>,
}

/// Stream ended event for frontend
#[derive(Serialize, Clone)]
pub struct StreamEndedEventFE {
    request_id: String,
    peer_id: String,
}

/// Stream connected event for frontend
#[derive(Serialize, Clone)]
pub struct StreamConnectedEventFE {
    request_id: String,
    topic_id: Option<String>,
    peer_id: String,
    is_source: bool,
}

/// Combined event type for frontend
#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum AppEvent {
    Message(MessageEvent),
    FileAnnounced(FileAnnouncedEventFE),
    FileProgress(FileProgressEventFE),
    FileComplete(FileCompleteEventFE),
    SyncUpdate(SyncUpdateEventFE),
    SyncRequest(SyncRequestEventFE),
    SyncResponse(SyncResponseEventFE),
    StreamRequest(StreamRequestEventFE),
    StreamAccepted(StreamAcceptedEventFE),
    StreamRejected(StreamRejectedEventFE),
    StreamEnded(StreamEndedEventFE),
    StreamConnected(StreamConnectedEventFE),
}

/// Fixed database key for persistent identity (32 bytes)
/// This allows the test app to reuse the same key pair across restarts
const DB_KEY: [u8; 32] = [
    0x48, 0x61, 0x72, 0x62, 0x6f, 0x72, 0x54, 0x65,  // "HarborTe"
    0x73, 0x74, 0x41, 0x70, 0x70, 0x4b, 0x65, 0x79,  // "stAppKey"
    0x32, 0x30, 0x32, 0x35, 0x5f, 0x76, 0x31, 0x5f,  // "2025_v1_"
    0x73, 0x65, 0x63, 0x72, 0x65, 0x74, 0x21, 0x21,  // "secret!!"
];

/// Start the Harbor Protocol
#[tauri::command]
async fn start_protocol(state: State<'_, AppState>) -> Result<StartResponse, String> {
    let mut protocol_guard = state.protocol.lock().await;
    
    if protocol_guard.is_some() {
        return Err("Protocol already running".to_string());
    }

    // Bootstrap ID from bootstrap.rs
    let bootstrap_id = "1f5c436e4511ab2db15db837b827f237931bc722ede6f223c7f5d41b412c0cad";
    
    // Use fixed db_key for persistent identity
    // Add bootstrap node so we can join the DHT network
    // Check for HARBOR_DB_PATH env var to allow multiple instances
    let mut config = ProtocolConfig::for_testing()
        .with_db_key(DB_KEY)
        .with_bootstrap_node(bootstrap_id.to_string());

    // Allow overriding db path via environment variable for running multiple instances
    if let Ok(db_path) = std::env::var("HARBOR_DB_PATH") {
        println!("Using custom database path: {}", db_path);
        config = config.with_db_path(db_path.into());
    }
    
    let protocol = Protocol::start(config)
        .await
        .map_err(|e| e.to_string())?;

    let endpoint_id = hex::encode(protocol.endpoint_id());
    
    // Print endpoint_id and compare with bootstrap
    println!();
    println!("========================================");
    println!("TEST APP ENDPOINT ID:");
    println!("{}", endpoint_id);
    println!();
    println!("BOOTSTRAP ID:");
    println!("{}", bootstrap_id);
    println!();
    if endpoint_id == bootstrap_id {
        println!("✅ MATCH: This node IS the bootstrap node!");
    } else {
        println!("❌ NO MATCH: Endpoint ID differs from bootstrap ID");
        println!("   Identity may have changed or database was reset.");
    }
    println!("========================================");
    
    // Wait a moment for relay connection, then print info
    println!("Waiting for relay connection...");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    
    let relay_url = protocol.relay_url().await;
    if let Some(ref relay) = relay_url {
        println!("✅ RELAY URL: {}", relay);
    } else {
        println!("⚠️  NO RELAY CONNECTED - cross-network may not work!");
    }
    println!("========================================");
    
    // Print bootstrap configuration info
    println!();
    println!("TO USE THIS NODE AS BOOTSTRAP, UPDATE:");
    println!("  core/src/network/dht/bootstrap.rs");
    println!();
    println!("  BootstrapNode::from_hex_with_relay(");
    println!("      \"{}\",", endpoint_id);
    println!("      Some(\"harbor-bootstrap-1\"),");
    if let Some(ref relay) = relay_url {
        println!("      \"{}\",", relay);
    } else {
        println!("      \"<RELAY_URL>\",");
    }
    println!("  ),");
    println!("========================================");
    println!();
    
    // Get event receiver
    if let Some(rx) = protocol.events().await {
        let mut rx_guard = state.event_rx.lock().await;
        *rx_guard = Some(rx);
    }

    *protocol_guard = Some(protocol);

    // Set up frontend log file path if using custom db
    if let Ok(db_path) = std::env::var("HARBOR_DB_PATH") {
        let path = std::path::Path::new(&db_path);
        if let Some(parent) = path.parent() {
            let log_path = parent.join("frontend.log");
            let mut log_guard = state.frontend_log_path.lock().await;
            *log_guard = Some(log_path);
        }
    }

    Ok(StartResponse { endpoint_id })
}

/// Stop the protocol
#[tauri::command]
async fn stop_protocol(state: State<'_, AppState>) -> Result<(), String> {
    let mut protocol_guard = state.protocol.lock().await;
    
    if let Some(protocol) = protocol_guard.take() {
        protocol.stop().await;
    }
    
    Ok(())
}

/// Get our endpoint ID
#[tauri::command]
async fn get_endpoint_id(state: State<'_, AppState>) -> Result<String, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    Ok(hex::encode(protocol.endpoint_id()))
}

/// Create a new topic
#[tauri::command]
async fn create_topic(state: State<'_, AppState>) -> Result<TopicResponse, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    let invite = protocol.create_topic()
        .await
        .map_err(|e| e.to_string())?;

    Ok(TopicResponse {
        topic_id: hex::encode(invite.topic_id),
        members: invite.members.iter().map(hex::encode).collect(),
        invite_hex: invite.to_hex().map_err(|e| e.to_string())?,
    })
}

/// Join a topic using an invite
#[tauri::command]
async fn join_topic(state: State<'_, AppState>, invite_hex: String) -> Result<TopicResponse, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    let invite = TopicInvite::from_hex(&invite_hex)
        .map_err(|e| e.to_string())?;

    let topic_id = invite.topic_id;
    let members = invite.members.clone();

    protocol.join_topic(invite)
        .await
        .map_err(|e| e.to_string())?;

    Ok(TopicResponse {
        topic_id: hex::encode(topic_id),
        members: members.iter().map(hex::encode).collect(),
        invite_hex,
    })
}

/// Leave a topic
#[tauri::command]
async fn leave_topic(state: State<'_, AppState>, topic_id: String) -> Result<(), String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;

    let topic_bytes = hex::decode(&topic_id)
        .map_err(|e| e.to_string())?;

    if topic_bytes.len() != 32 {
        return Err("Invalid topic ID length".to_string());
    }

    let mut topic_arr = [0u8; 32];
    topic_arr.copy_from_slice(&topic_bytes);

    protocol.leave_topic(&topic_arr)
        .await
        .map_err(|e| e.to_string())
}

/// Send a message to a topic
#[tauri::command]
async fn send_message(
    state: State<'_, AppState>,
    topic_id: String,
    message: String,
) -> Result<(), String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    let topic_bytes = hex::decode(&topic_id)
        .map_err(|e| e.to_string())?;
    
    if topic_bytes.len() != 32 {
        return Err("Invalid topic ID length".to_string());
    }
    
    let mut topic_arr = [0u8; 32];
    topic_arr.copy_from_slice(&topic_bytes);

    protocol.send(harbor_core::Target::Topic(topic_arr), message.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

/// List all topics
#[tauri::command]
async fn list_topics(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    let topics = protocol.list_topics()
        .await
        .map_err(|e| e.to_string())?;

    Ok(topics.iter().map(hex::encode).collect())
}

/// Get topic invite
#[tauri::command]
async fn get_invite(state: State<'_, AppState>, topic_id: String) -> Result<String, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    let topic_bytes = hex::decode(&topic_id)
        .map_err(|e| e.to_string())?;
    
    if topic_bytes.len() != 32 {
        return Err("Invalid topic ID length".to_string());
    }
    
    let mut topic_arr = [0u8; 32];
    topic_arr.copy_from_slice(&topic_bytes);

    let invite = protocol.get_invite(&topic_arr)
        .await
        .map_err(|e| e.to_string())?;

    invite.to_hex().map_err(|e| e.to_string())
}

/// Poll for new events (messages + file events + sync events)
#[tauri::command]
async fn poll_events(state: State<'_, AppState>) -> Result<Vec<AppEvent>, String> {
    let mut rx_guard = state.event_rx.lock().await;
    let rx = rx_guard.as_mut().ok_or("No event receiver")?;

    let mut events = Vec::new();

    // Non-blocking receive of all available events
    loop {
        match rx.try_recv() {
            Ok(event) => {
                let app_event = match event {
                    ProtocolEvent::Message(msg) => {
                        println!("[poll_events] Message event - topic: {}", hex::encode(&msg.topic_id[..4]));
                        AppEvent::Message(MessageEvent {
                            topic_id: hex::encode(msg.topic_id),
                            sender_id: hex::encode(msg.sender_id),
                            payload: String::from_utf8_lossy(&msg.payload).to_string(),
                            timestamp: msg.timestamp,
                        })
                    }
                    ProtocolEvent::FileAnnounced(ev) => {
                        println!("[poll_events] FileAnnounced event - hash: {}", hex::encode(&ev.hash[..4]));
                        AppEvent::FileAnnounced(FileAnnouncedEventFE {
                            topic_id: hex::encode(ev.topic_id),
                            source_id: hex::encode(ev.source_id),
                            hash: hex::encode(ev.hash),
                            display_name: ev.display_name,
                            total_size: ev.total_size,
                            total_chunks: ev.total_chunks,
                            timestamp: ev.timestamp,
                        })
                    }
                    ProtocolEvent::FileProgress(ev) => {
                        AppEvent::FileProgress(FileProgressEventFE {
                            hash: hex::encode(ev.hash),
                            chunks_complete: ev.chunks_complete,
                            total_chunks: ev.total_chunks,
                        })
                    }
                    ProtocolEvent::FileComplete(ev) => {
                        println!("[poll_events] FileComplete event - hash: {}", hex::encode(&ev.hash[..4]));
                        AppEvent::FileComplete(FileCompleteEventFE {
                            hash: hex::encode(ev.hash),
                            display_name: ev.display_name,
                            total_size: ev.total_size,
                        })
                    }
                    ProtocolEvent::SyncUpdate(ev) => {
                        println!("[poll_events] SyncUpdate event - topic: {}, sender: {}, size: {} bytes",
                            hex::encode(&ev.topic_id[..4]),
                            hex::encode(&ev.sender_id[..4]),
                            ev.data.len()
                        );
                        // Pass through to frontend - it will handle with Loro
                        AppEvent::SyncUpdate(SyncUpdateEventFE {
                            topic_id: hex::encode(ev.topic_id),
                            sender_id: hex::encode(ev.sender_id),
                            data: ev.data,
                        })
                    }
                    ProtocolEvent::SyncRequest(ev) => {
                        println!("[poll_events] SyncRequest event - topic: {}, sender: {}",
                            hex::encode(&ev.topic_id[..4]),
                            hex::encode(&ev.sender_id[..4])
                        );
                        // Pass through to frontend - it will export and respond
                        AppEvent::SyncRequest(SyncRequestEventFE {
                            topic_id: hex::encode(ev.topic_id),
                            sender_id: hex::encode(ev.sender_id),
                        })
                    }
                    ProtocolEvent::SyncResponse(ev) => {
                        println!("[poll_events] SyncResponse event - topic: {}, size: {} bytes",
                            hex::encode(&ev.topic_id[..4]),
                            ev.data.len()
                        );
                        // Pass through to frontend - it will import the snapshot
                        AppEvent::SyncResponse(SyncResponseEventFE {
                            topic_id: hex::encode(ev.topic_id),
                            data: ev.data,
                        })
                    }
                    ProtocolEvent::StreamRequest(ev) => {
                        println!("[poll_events] StreamRequest from {}", hex::encode(&ev.peer_id[..4]));
                        AppEvent::StreamRequest(StreamRequestEventFE {
                            topic_id: ev.topic_id.map(hex::encode),
                            peer_id: hex::encode(ev.peer_id),
                            request_id: hex::encode(ev.request_id),
                            name: ev.name,
                        })
                    }
                    ProtocolEvent::StreamAccepted(ev) => {
                        println!("[poll_events] StreamAccepted {}", hex::encode(&ev.request_id[..4]));
                        AppEvent::StreamAccepted(StreamAcceptedEventFE {
                            request_id: hex::encode(ev.request_id),
                        })
                    }
                    ProtocolEvent::StreamRejected(ev) => {
                        println!("[poll_events] StreamRejected {}", hex::encode(&ev.request_id[..4]));
                        AppEvent::StreamRejected(StreamRejectedEventFE {
                            request_id: hex::encode(ev.request_id),
                            reason: ev.reason,
                        })
                    }
                    ProtocolEvent::StreamEnded(ev) => {
                        println!("[poll_events] StreamEnded {}", hex::encode(&ev.request_id[..4]));
                        AppEvent::StreamEnded(StreamEndedEventFE {
                            request_id: hex::encode(ev.request_id),
                            peer_id: hex::encode(ev.peer_id),
                        })
                    }
                    ProtocolEvent::StreamConnected(ev) => {
                        println!("[poll_events] StreamConnected {} is_source={}", hex::encode(&ev.request_id[..4]), ev.is_source);
                        AppEvent::StreamConnected(StreamConnectedEventFE {
                            request_id: hex::encode(ev.request_id),
                            topic_id: ev.topic_id.map(hex::encode),
                            peer_id: hex::encode(ev.peer_id),
                            is_source: ev.is_source,
                        })
                    }
                    // DM events — not yet surfaced
                    ProtocolEvent::DmReceived(_)
                    | ProtocolEvent::DmSyncUpdate(_)
                    | ProtocolEvent::DmSyncRequest(_)
                    | ProtocolEvent::DmSyncResponse(_)
                    | ProtocolEvent::DmFileAnnounced(_) => continue,
                };
                events.push(app_event);
            }
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                return Err("Event channel disconnected".to_string());
            }
        }
    }

    if !events.is_empty() {
        println!("[poll_events] Returning {} events to frontend", events.len());
    }

    Ok(events)
}

/// Poll for new messages only (backwards compatibility)
#[tauri::command]
async fn poll_messages(state: State<'_, AppState>) -> Result<Vec<MessageEvent>, String> {
    let events = poll_events(state).await?;
    
    Ok(events.into_iter().filter_map(|e| {
        if let AppEvent::Message(msg) = e {
            Some(msg)
        } else {
            None
        }
    }).collect())
}

// ============================================================================
// Stats & Monitoring Commands
// ============================================================================

/// Get comprehensive protocol statistics
#[tauri::command]
async fn get_stats(state: State<'_, AppState>) -> Result<ProtocolStats, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    protocol.get_stats()
        .await
        .map_err(|e| e.to_string())
}

/// Get detailed DHT bucket information
#[tauri::command]
async fn get_dht_buckets(state: State<'_, AppState>) -> Result<Vec<DhtBucketInfo>, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    protocol.get_dht_buckets()
        .await
        .map_err(|e| e.to_string())
}

/// Get detailed information about a specific topic
#[tauri::command]
async fn get_topic_details(state: State<'_, AppState>, topic_id: String) -> Result<TopicDetails, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    let topic_bytes = hex::decode(&topic_id)
        .map_err(|e| e.to_string())?;
    
    if topic_bytes.len() != 32 {
        return Err("Invalid topic ID length".to_string());
    }
    
    let mut topic_arr = [0u8; 32];
    topic_arr.copy_from_slice(&topic_bytes);

    protocol.get_topic_details(&topic_arr)
        .await
        .map_err(|e| e.to_string())
}

/// List all topics with summary info
#[tauri::command]
async fn list_topic_summaries(state: State<'_, AppState>) -> Result<Vec<TopicSummary>, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    protocol.list_topic_summaries()
        .await
        .map_err(|e| e.to_string())
}

// ============================================================================
// File Sharing Commands
// ============================================================================

/// Share response for frontend
#[derive(Serialize)]
pub struct ShareResponse {
    hash: String,
    display_name: String,
    total_size: u64,
    total_chunks: u32,
}

/// Share a file to a topic
#[tauri::command]
async fn share_file(
    state: State<'_, AppState>,
    topic_id: String,
    file_path: String,
) -> Result<ShareResponse, String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    let topic_bytes = hex::decode(&topic_id)
        .map_err(|e| e.to_string())?;
    
    if topic_bytes.len() != 32 {
        return Err("Invalid topic ID length".to_string());
    }
    
    let mut topic_arr = [0u8; 32];
    topic_arr.copy_from_slice(&topic_bytes);

    let hash = protocol.share_file(harbor_core::Target::Topic(topic_arr), &file_path)
        .await
        .map_err(|e| e.to_string())?;
    
    // Get file info for response
    let file_name = std::path::Path::new(&file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    
    let file_size = std::fs::metadata(&file_path)
        .map(|m| m.len())
        .unwrap_or(0);
    
    let total_chunks = ((file_size + 512 * 1024 - 1) / (512 * 1024)) as u32;
    
    Ok(ShareResponse {
        hash: hex::encode(hash),
        display_name: file_name,
        total_size: file_size,
        total_chunks,
    })
}

/// Export a completed file to a destination path
#[tauri::command]
async fn export_file(
    state: State<'_, AppState>,
    hash: String,
    destination: String,
) -> Result<(), String> {
    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;
    
    let hash_bytes = hex::decode(&hash)
        .map_err(|e| e.to_string())?;
    
    if hash_bytes.len() != 32 {
        return Err("Invalid hash length".to_string());
    }
    
    let mut hash_arr = [0u8; 32];
    hash_arr.copy_from_slice(&hash_bytes);

    protocol.export_blob(&hash_arr, &destination)
        .await
        .map_err(|e| e.to_string())
}

// ============================================================================
// Sync Commands (Pass-through to Harbor Protocol)
// ============================================================================

/// Send a sync update (CRDT delta bytes) to all topic members
#[tauri::command]
async fn sync_send_update(
    state: State<'_, AppState>,
    topic_id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    println!("[sync_send_update] Called for topic: {}", &topic_id[..16]);
    println!("[sync_send_update] Data size: {} bytes", data.len());

    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;

    let topic_bytes = hex::decode(&topic_id)
        .map_err(|e| e.to_string())?;

    if topic_bytes.len() != 32 {
        return Err("Invalid topic ID length".to_string());
    }

    let mut topic_arr = [0u8; 32];
    topic_arr.copy_from_slice(&topic_bytes);

    println!("[sync_send_update] Calling protocol.send_sync_update...");
    protocol.send_sync_update(Target::Topic(topic_arr), data)
        .await
        .map_err(|e| {
            println!("[sync_send_update] ✗ Error: {}", e);
            e.to_string()
        })?;

    println!("[sync_send_update] ✓ Success");
    Ok(())
}

/// Request sync state from all topic members
#[tauri::command]
async fn sync_request(
    state: State<'_, AppState>,
    topic_id: String,
) -> Result<(), String> {
    println!("[sync_request] Called for topic: {}", &topic_id[..16]);

    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;

    let topic_bytes = hex::decode(&topic_id)
        .map_err(|e| e.to_string())?;

    if topic_bytes.len() != 32 {
        return Err("Invalid topic ID length".to_string());
    }

    let mut topic_arr = [0u8; 32];
    topic_arr.copy_from_slice(&topic_bytes);

    println!("[sync_request] Calling protocol.request_sync...");
    protocol.request_sync(Target::Topic(topic_arr))
        .await
        .map_err(|e| {
            println!("[sync_request] ✗ Error: {}", e);
            e.to_string()
        })?;

    println!("[sync_request] ✓ Success");
    Ok(())
}

/// Respond to a sync request with full CRDT state
#[tauri::command]
async fn sync_respond(
    state: State<'_, AppState>,
    topic_id: String,
    requester_id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    println!("[sync_respond] Called for topic: {}", &topic_id[..16]);
    println!("[sync_respond] Requester: {}", &requester_id[..16]);
    println!("[sync_respond] Data size: {} bytes", data.len());

    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;

    let topic_bytes = hex::decode(&topic_id)
        .map_err(|e| e.to_string())?;
    let requester_bytes = hex::decode(&requester_id)
        .map_err(|e| e.to_string())?;

    if topic_bytes.len() != 32 || requester_bytes.len() != 32 {
        return Err("Invalid ID length".to_string());
    }

    let mut topic_arr = [0u8; 32];
    let mut requester_arr = [0u8; 32];
    topic_arr.copy_from_slice(&topic_bytes);
    requester_arr.copy_from_slice(&requester_bytes);

    println!("[sync_respond] Calling protocol.respond_sync...");
    protocol.respond_sync(Target::Topic(topic_arr), &requester_arr, data)
        .await
        .map_err(|e| {
            println!("[sync_respond] ✗ Error: {}", e);
            e.to_string()
        })?;

    println!("[sync_respond] ✓ Success");
    Ok(())
}

/// Write a log message to the frontend log file
#[tauri::command]
async fn write_frontend_log(state: State<'_, AppState>, message: String) -> Result<(), String> {
    let log_guard = state.frontend_log_path.lock().await;

    if let Some(ref log_path) = *log_guard {
        use std::io::Write;

        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let log_line = format!("[{}] {}\n", timestamp, message);

        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
        {
            let _ = file.write_all(log_line.as_bytes());
        }
    }

    Ok(())
}

// ============================================================================
// Video Streaming Commands
// ============================================================================

/// List available cameras
#[tauri::command]
async fn list_cameras() -> Result<Vec<serde_json::Value>, String> {
    let cameras = video::capture::list_cameras()?;
    Ok(cameras
        .into_iter()
        .map(|c| serde_json::json!({ "index": c.index, "name": c.name }))
        .collect())
}

/// Helper to parse hex string to [u8; 32]
fn parse_id(hex_str: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(hex_str).map_err(|e| e.to_string())?;
    if bytes.len() != 32 {
        return Err("Invalid ID length".to_string());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Request a video stream to a peer in a topic
#[tauri::command]
async fn request_stream(
    state: State<'_, AppState>,
    topic_id: String,
    peer_id: String,
    name: String,
) -> Result<String, String> {
    let topic_arr = parse_id(&topic_id)?;
    let peer_arr = parse_id(&peer_id)?;

    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;

    let request_id = protocol
        .request_stream(&topic_arr, &peer_arr, &name, Vec::new())
        .await
        .map_err(|e| e.to_string())?;

    Ok(hex::encode(request_id))
}

/// Accept an incoming stream request
#[tauri::command]
async fn accept_stream(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    let id = parse_id(&request_id)?;

    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;

    protocol.accept_stream(&id).await.map_err(|e| e.to_string())
}

/// Reject an incoming stream request
#[tauri::command]
async fn reject_stream(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    let id = parse_id(&request_id)?;

    let protocol_guard = state.protocol.lock().await;
    let protocol = protocol_guard.as_ref().ok_or("Protocol not running")?;

    protocol
        .reject_stream(&id, Some("User rejected".to_string()))
        .await
        .map_err(|e| e.to_string())
}

/// Start video capture + encode + publish on an accepted stream
#[tauri::command]
async fn start_video_stream(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    let id = parse_id(&request_id)?;

    let pipeline = video::pipeline::VideoPipeline::start(
        state.protocol.clone(),
        id,
        640, // width
        480, // height
        30,  // fps
    )
    .await?;

    let mut pipelines = state.video_pipelines.lock().await;
    pipelines.insert(request_id, pipeline);

    Ok(())
}

/// Stop a running video stream
#[tauri::command]
async fn stop_video_stream(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    let mut pipelines = state.video_pipelines.lock().await;
    if let Some(mut pipeline) = pipelines.remove(&request_id) {
        pipeline.stop();
    }

    // Also end the stream at protocol level
    let id = parse_id(&request_id)?;
    let protocol_guard = state.protocol.lock().await;
    if let Some(protocol) = protocol_guard.as_ref() {
        let _ = protocol.end_stream(&id).await;
    }

    Ok(())
}

/// Start receiving + decoding video from an active stream
#[tauri::command]
async fn start_video_receive(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    let id = parse_id(&request_id)?;

    let consumer = video::consumer::VideoConsumer::start(
        state.protocol.clone(),
        id,
        640,
        480,
    )
    .await?;

    let mut consumers = state.video_consumers.lock().await;
    consumers.insert(request_id, consumer);

    Ok(())
}

/// Stop receiving video
#[tauri::command]
async fn stop_video_receive(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    let mut consumers = state.video_consumers.lock().await;
    if let Some(mut consumer) = consumers.remove(&request_id) {
        consumer.stop();
    }
    Ok(())
}

/// Get the latest decoded video frame as a base64-encoded JPEG
#[tauri::command]
async fn get_video_frame(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<Option<String>, String> {
    let consumers = state.video_consumers.lock().await;
    if let Some(consumer) = consumers.get(&request_id) {
        Ok(consumer.get_frame())
    } else {
        Ok(None)
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize tracing - write to file if HARBOR_DB_PATH is set
    // Use try_init() to avoid panic if already initialized
    if let Ok(db_path) = std::env::var("HARBOR_DB_PATH") {
        // Extract instance directory from db path (e.g., "./test-data/instance1/harbor.db" -> "./test-data/instance1")
        let path = std::path::Path::new(&db_path);
        if let Some(parent) = path.parent() {
            // Create the directory if it doesn't exist
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("Failed to create log directory: {}", e);
                let _ = tracing_subscriber::fmt::try_init();
            } else {
                let log_path = parent.join("app.log");
                println!("Logging to: {}", log_path.display());

                // Create log file
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                {
                    Ok(log_file) => {
                        // Initialize tracing with file output
                        let _ = tracing_subscriber::fmt()
                            .with_writer(std::sync::Mutex::new(log_file))
                            .with_ansi(false)
                            .try_init();
                    }
                    Err(e) => {
                        eprintln!("Failed to create log file: {}", e);
                        let _ = tracing_subscriber::fmt::try_init();
                    }
                }
            }
        } else {
            let _ = tracing_subscriber::fmt::try_init();
        }
    } else {
        let _ = tracing_subscriber::fmt::try_init();
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            start_protocol,
            stop_protocol,
            get_endpoint_id,
            create_topic,
            join_topic,
            leave_topic,
            send_message,
            list_topics,
            get_invite,
            poll_messages,
            poll_events,
            // Stats & Monitoring
            get_stats,
            get_dht_buckets,
            get_topic_details,
            list_topic_summaries,
            // File Sharing
            share_file,
            export_file,
            // Sync Commands
            sync_send_update,
            sync_request,
            sync_respond,
            // Logging
            write_frontend_log,
            // Video Streaming
            list_cameras,
            request_stream,
            accept_stream,
            reject_stream,
            start_video_stream,
            stop_video_stream,
            start_video_receive,
            stop_video_receive,
            get_video_frame,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
