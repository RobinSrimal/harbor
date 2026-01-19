//! Tauri app integration with Harbor Protocol

use std::sync::Arc;
use tauri::State;
use tokio::sync::{Mutex, mpsc};
use serde::Serialize;

use harbor_core::{
    Protocol, ProtocolConfig, TopicInvite,
    ProtocolStats, DhtBucketInfo, TopicDetails, TopicSummary,
    ProtocolEvent,
};

/// Shared protocol state
pub struct AppState {
    protocol: Arc<Mutex<Option<Protocol>>>,
    event_rx: Arc<Mutex<Option<mpsc::Receiver<ProtocolEvent>>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            protocol: Arc::new(Mutex::new(None)),
            event_rx: Arc::new(Mutex::new(None)),
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

/// Combined event type for frontend
#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum AppEvent {
    Message(MessageEvent),
    FileAnnounced(FileAnnouncedEventFE),
    FileProgress(FileProgressEventFE),
    FileComplete(FileCompleteEventFE),
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
    let config = ProtocolConfig::for_testing()
        .with_db_key(DB_KEY)
        .with_bootstrap_node(bootstrap_id.to_string());
    
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

    protocol.send(&topic_arr, message.as_bytes())
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

/// Poll for new events (messages + file events)
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
                        AppEvent::Message(MessageEvent {
                            topic_id: hex::encode(msg.topic_id),
                            sender_id: hex::encode(msg.sender_id),
                            payload: String::from_utf8_lossy(&msg.payload).to_string(),
                            timestamp: msg.timestamp,
                        })
                    }
                    ProtocolEvent::FileAnnounced(ev) => {
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
                        AppEvent::FileComplete(FileCompleteEventFE {
                            hash: hex::encode(ev.hash),
                            display_name: ev.display_name,
                            total_size: ev.total_size,
                        })
                    }
                };
                events.push(app_event);
            }
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                return Err("Event channel disconnected".to_string());
            }
        }
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

    let hash = protocol.share_file(&topic_arr, &file_path)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize tracing
    tracing_subscriber::fmt::init();

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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
