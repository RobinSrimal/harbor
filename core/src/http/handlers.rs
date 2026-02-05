//! HTTP API endpoint handlers

use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;
use tracing::info;

use harbor_core::{Protocol, TopicInvite};
use super::parse::{extract_json_string, http_response, http_json_response};

/// Shared state for active stream tracks (source side)
/// Maps request_id → TrackProducer for reuse across publish calls
pub type ActiveTracks = Arc<RwLock<HashMap<[u8; 32], moq_lite::TrackProducer>>>;

/// Bootstrap info for sharing
pub struct BootstrapInfo {
    pub endpoint_id: String,
    pub relay_url: Option<String>,
}

pub async fn handle_create_topic(protocol: &Protocol) -> String {
    match protocol.create_topic().await {
        Ok(invite) => {
            match invite.to_hex() {
                Ok(hex) => http_response(200, &hex),
                Err(e) => http_response(500, &format!("Failed to encode invite: {}", e)),
            }
        }
        Err(e) => http_response(500, &format!("Failed to create topic: {}", e)),
    }
}

pub async fn handle_get_invite(protocol: &Protocol, topic_hex: &str) -> String {
    let topic_bytes = match hex::decode(topic_hex.trim()) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID (must be 64 hex chars)"),
    };

    match protocol.get_invite(&topic_bytes).await {
        Ok(invite) => {
            match invite.to_hex() {
                Ok(hex) => http_response(200, &hex),
                Err(e) => http_response(500, &format!("Failed to encode invite: {}", e)),
            }
        }
        Err(e) => http_response(404, &format!("Topic not found: {}", e)),
    }
}

pub async fn handle_join_topic(protocol: &Protocol, body: &str) -> String {
    let invite_hex = body.trim();
    if invite_hex.is_empty() {
        return http_response(400, "Missing invite in body");
    }

    match TopicInvite::from_hex(invite_hex) {
        Ok(invite) => {
            let topic_hex = hex::encode(invite.topic_id);
            match protocol.join_topic(invite).await {
                Ok(()) => http_response(200, &format!("Joined topic: {}", topic_hex)),
                Err(e) => http_response(500, &format!("Failed to join: {}", e)),
            }
        }
        Err(e) => http_response(400, &format!("Invalid invite: {}", e)),
    }
}

pub async fn handle_send(protocol: &Protocol, body: &str) -> String {
    info!("API: handle_send started, body_len={}", body.len());

    // Expect JSON: {"topic": "hex...", "message": "text"}
    let topic_start = body.find("\"topic\"");
    let msg_start = body.find("\"message\"");

    if topic_start.is_none() || msg_start.is_none() {
        tracing::warn!("API: JSON parse failed - missing 'topic' or 'message' field, body_len={}", body.len());
        return http_response(400, "Expected JSON with 'topic' and 'message' fields");
    }

    let topic_hex = extract_json_string(body, "topic");
    let message = extract_json_string(body, "message");

    if topic_hex.is_none() || message.is_none() {
        tracing::warn!("API: JSON parse failed - couldn't extract values, body_len={}", body.len());
        return http_response(400, "Failed to parse topic or message");
    }

    let topic_hex = topic_hex.unwrap();
    let message = message.unwrap();

    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID (must be 64 hex chars)"),
    };

    info!("API: calling protocol.send()");
    let result = match protocol.send(harbor_core::Target::Topic(topic_bytes), message.as_bytes()).await {
        Ok(()) => {
            info!("API: protocol.send() completed successfully");
            http_response(200, "Message sent")
        }
        Err(e) => {
            info!("API: protocol.send() failed: {}", e);
            http_response(500, &format!("Failed to send: {}", e))
        }
    };
    info!("API: handle_send returning response");
    result
}

/// POST /api/dm - Send a direct message
/// Body: {"recipient":"hex64","message":"text"}
pub async fn handle_send_dm(protocol: &Protocol, body: &str) -> String {
    let recipient_hex = extract_json_string(body, "recipient");
    let message = extract_json_string(body, "message");

    if recipient_hex.is_none() || message.is_none() {
        return http_response(400, "Expected JSON with 'recipient' and 'message' fields");
    }

    let recipient_hex = recipient_hex.unwrap();
    let message = message.unwrap();

    let recipient_bytes = match hex::decode(&recipient_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid recipient ID (must be 64 hex chars)"),
    };

    match protocol.send(harbor_core::Target::Dm(recipient_bytes), message.as_bytes()).await {
        Ok(()) => {
            info!("API: DM sent to {}", &recipient_hex[..16]);
            http_response(200, "DM sent")
        }
        Err(e) => {
            info!("API: DM send failed: {}", e);
            http_response(500, &format!("Failed to send DM: {}", e))
        }
    }
}

pub async fn handle_stats(protocol: &Protocol) -> String {
    match protocol.get_stats().await {
        Ok(stats) => {
            // dht_nodes = in-memory (real-time, accurate)
            // dht_nodes_db = database (persisted, may lag behind)
            let json = format!(
                r#"{{"endpoint_id":"{}","dht_nodes":{},"dht_nodes_db":{},"topics":{},"harbor_packets":{}}}"#,
                stats.identity.endpoint_id,
                stats.dht.in_memory_nodes,
                stats.dht.total_nodes,
                stats.topics.subscribed_count,
                stats.harbor.packets_stored
            );
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to get stats: {}", e)),
    }
}

pub async fn handle_list_topics(protocol: &Protocol) -> String {
    match protocol.list_topics().await {
        Ok(topics) => {
            let hex_topics: Vec<String> = topics.iter().map(|t| hex::encode(t)).collect();
            let json = format!(r#"{{"topics":[{}]}}"#,
                hex_topics.iter().map(|t| format!("\"{}\"", t)).collect::<Vec<_>>().join(",")
            );
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to list topics: {}", e)),
    }
}

pub fn handle_bootstrap(bootstrap: &BootstrapInfo) -> String {
    let relay = bootstrap.relay_url.as_deref().unwrap_or("");
    let json = format!(
        r#"{{"endpoint_id":"{}","relay_url":"{}","bootstrap_arg":"{}:{}"}}"#,
        bootstrap.endpoint_id,
        relay,
        bootstrap.endpoint_id,
        relay
    );
    http_json_response(200, &json)
}

// ============================================================================
// Share API Handlers
// ============================================================================

/// Share a file with a topic
/// POST /api/share
/// Body: {"topic": "hex...", "file_path": "/path/to/file"}
pub async fn handle_share_file(protocol: &Protocol, body: &str) -> String {
    let topic_hex = match extract_json_string(body, "topic") {
        Some(t) => t,
        None => return http_response(400, "Missing 'topic' field"),
    };

    let file_path = match extract_json_string(body, "file_path") {
        Some(f) => f,
        None => return http_response(400, "Missing 'file_path' field"),
    };

    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID (must be 64 hex chars)"),
    };

    match protocol.share_file(harbor_core::Target::Topic(topic_bytes), &file_path).await {
        Ok(hash) => {
            let json = format!(r#"{{"hash":"{}","status":"announced"}}"#, hex::encode(hash));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to share file: {}", e)),
    }
}

/// Get share status for a blob
/// GET /api/share/status/:hash
pub async fn handle_share_status(protocol: &Protocol, hash_hex: &str) -> String {
    let hash_bytes = match hex::decode(hash_hex.trim()) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid hash (must be 64 hex chars)"),
    };

    match protocol.get_share_status(&hash_bytes).await {
        Ok(status) => {
            let state_str = match status.state {
                harbor_core::data::BlobState::Partial => "partial",
                harbor_core::data::BlobState::Complete => "complete",
            };
            let json = format!(
                r#"{{"hash":"{}","display_name":"{}","total_size":{},"total_chunks":{},"chunks_complete":{},"state":"{}","section_count":{}}}"#,
                hex::encode(status.hash),
                status.display_name,
                status.total_size,
                status.total_chunks,
                status.chunks_complete,
                state_str,
                status.num_sections
            );
            http_json_response(200, &json)
        }
        Err(e) => http_response(404, &format!("Blob not found: {}", e)),
    }
}

/// List blobs for a topic
/// GET /api/share/blobs/:topic
pub async fn handle_list_blobs(protocol: &Protocol, topic_hex: &str) -> String {
    let topic_bytes = match hex::decode(topic_hex.trim()) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID (must be 64 hex chars)"),
    };

    match protocol.list_topic_blobs(&topic_bytes).await {
        Ok(blobs) => {
            let blob_jsons: Vec<String> = blobs.iter().map(|b| {
                let state_str = match b.state {
                    harbor_core::data::BlobState::Partial => "partial",
                    harbor_core::data::BlobState::Complete => "complete",
                };
                format!(
                    r#"{{"hash":"{}","display_name":"{}","total_size":{},"total_chunks":{},"state":"{}"}}"#,
                    hex::encode(b.hash),
                    b.display_name,
                    b.total_size,
                    b.total_chunks,
                    state_str
                )
            }).collect();
            let json = format!(r#"{{"blobs":[{}]}}"#, blob_jsons.join(","));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to list blobs: {}", e)),
    }
}

/// Request to pull a blob
/// POST /api/share/pull
/// Body: {"hash": "hex..."}
pub async fn handle_share_pull(protocol: &Protocol, body: &str) -> String {
    let hash_hex = match extract_json_string(body, "hash") {
        Some(h) => h,
        None => return http_response(400, "Missing 'hash' field"),
    };

    let hash_bytes = match hex::decode(&hash_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid hash (must be 64 hex chars)"),
    };

    match protocol.request_blob(&hash_bytes).await {
        Ok(()) => http_response(200, "Pull started"),
        Err(e) => http_response(500, &format!("Failed to start pull: {}", e)),
    }
}

/// Export a completed blob to a file
/// POST /api/share/export
/// Body: {"hash": "hex...", "dest_path": "/path/to/output"}
pub async fn handle_share_export(protocol: &Protocol, body: &str) -> String {
    let hash_hex = match extract_json_string(body, "hash") {
        Some(h) => h,
        None => return http_response(400, "Missing 'hash' field"),
    };

    let dest_path = match extract_json_string(body, "dest_path") {
        Some(p) => p,
        None => return http_response(400, "Missing 'dest_path' field"),
    };

    let hash_bytes = match hex::decode(&hash_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid hash (must be 64 hex chars)"),
    };

    match protocol.export_blob(&hash_bytes, &dest_path).await {
        Ok(()) => http_response(200, "Exported successfully"),
        Err(e) => http_response(500, &format!("Failed to export: {}", e)),
    }
}

// ============================================================================
// Sync API Handlers
// ============================================================================

/// Send a sync update to all topic members
/// POST /api/sync/update
/// Body: {"topic": "hex...", "data": "hex-encoded-crdt-update"}
pub async fn handle_sync_update(protocol: &Protocol, body: &str) -> String {
    let topic_hex = match extract_json_string(body, "topic") {
        Some(t) => t,
        None => return http_response(400, "Missing 'topic' field"),
    };

    let data_hex = match extract_json_string(body, "data") {
        Some(d) => d,
        None => return http_response(400, "Missing 'data' field"),
    };

    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID (must be 64 hex chars)"),
    };

    let data = match hex::decode(&data_hex) {
        Ok(d) => d,
        Err(_) => return http_response(400, "Invalid hex data"),
    };

    match protocol.send_sync_update(harbor_core::Target::Topic(topic_bytes), data).await {
        Ok(()) => http_response(200, "Sync update sent"),
        Err(e) => http_response(500, &format!("Failed to send sync update: {}", e)),
    }
}

/// Request sync state from topic members
/// POST /api/sync/request
/// Body: {"topic": "hex..."}
pub async fn handle_sync_request(protocol: &Protocol, body: &str) -> String {
    let topic_hex = match extract_json_string(body, "topic") {
        Some(t) => t,
        None => return http_response(400, "Missing 'topic' field"),
    };

    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID (must be 64 hex chars)"),
    };

    match protocol.request_sync(harbor_core::Target::Topic(topic_bytes)).await {
        Ok(()) => http_response(200, "Sync request sent"),
        Err(e) => http_response(500, &format!("Failed to request sync: {}", e)),
    }
}

/// Respond to a sync request with current state
/// POST /api/sync/respond
/// Body: {"topic": "hex...", "requester": "hex...", "data": "hex-encoded-crdt-snapshot"}
pub async fn handle_sync_respond(protocol: &Protocol, body: &str) -> String {
    let topic_hex = match extract_json_string(body, "topic") {
        Some(t) => t,
        None => return http_response(400, "Missing 'topic' field"),
    };

    let requester_hex = match extract_json_string(body, "requester") {
        Some(r) => r,
        None => return http_response(400, "Missing 'requester' field"),
    };

    let data_hex = match extract_json_string(body, "data") {
        Some(d) => d,
        None => return http_response(400, "Missing 'data' field"),
    };

    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID (must be 64 hex chars)"),
    };

    let requester_bytes = match hex::decode(&requester_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid requester ID (must be 64 hex chars)"),
    };

    let data = match hex::decode(&data_hex) {
        Ok(d) => d,
        Err(_) => return http_response(400, "Invalid hex data"),
    };

    match protocol.respond_sync(harbor_core::Target::Topic(topic_bytes), &requester_bytes, data).await {
        Ok(()) => http_response(200, "Sync response sent"),
        Err(e) => http_response(500, &format!("Failed to respond to sync: {}", e)),
    }
}

// ============================================================================
// DM Sync API Handlers
// ============================================================================

/// POST /api/dm/sync/update
/// Body: {"recipient": "hex64", "data": "hex-encoded-crdt-update"}
pub async fn handle_dm_sync_update(protocol: &Protocol, body: &str) -> String {
    let recipient_hex = match extract_json_string(body, "recipient") {
        Some(r) => r,
        None => return http_response(400, "Missing 'recipient' field"),
    };

    let data_hex = match extract_json_string(body, "data") {
        Some(d) => d,
        None => return http_response(400, "Missing 'data' field"),
    };

    let recipient_bytes = match hex::decode(&recipient_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid recipient ID (must be 64 hex chars)"),
    };

    let data = match hex::decode(&data_hex) {
        Ok(d) => d,
        Err(_) => return http_response(400, "Invalid hex data"),
    };

    match protocol.send_sync_update(harbor_core::Target::Dm(recipient_bytes), data).await {
        Ok(()) => http_response(200, "DM sync update sent"),
        Err(e) => http_response(500, &format!("Failed to send DM sync update: {}", e)),
    }
}

/// POST /api/dm/sync/request
/// Body: {"recipient": "hex64"}
pub async fn handle_dm_sync_request(protocol: &Protocol, body: &str) -> String {
    let recipient_hex = match extract_json_string(body, "recipient") {
        Some(r) => r,
        None => return http_response(400, "Missing 'recipient' field"),
    };

    let recipient_bytes = match hex::decode(&recipient_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid recipient ID (must be 64 hex chars)"),
    };

    match protocol.request_sync(harbor_core::Target::Dm(recipient_bytes)).await {
        Ok(()) => http_response(200, "DM sync request sent"),
        Err(e) => http_response(500, &format!("Failed to send DM sync request: {}", e)),
    }
}

/// POST /api/dm/sync/respond
/// Body: {"recipient": "hex64", "data": "hex-encoded-crdt-snapshot"}
pub async fn handle_dm_sync_respond(protocol: &Protocol, body: &str) -> String {
    let recipient_hex = match extract_json_string(body, "recipient") {
        Some(r) => r,
        None => return http_response(400, "Missing 'recipient' field"),
    };

    let data_hex = match extract_json_string(body, "data") {
        Some(d) => d,
        None => return http_response(400, "Missing 'data' field"),
    };

    let recipient_bytes = match hex::decode(&recipient_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid recipient ID (must be 64 hex chars)"),
    };

    let data = match hex::decode(&data_hex) {
        Ok(d) => d,
        Err(_) => return http_response(400, "Invalid hex data"),
    };

    // requester_id is unused for DM target, pass recipient as placeholder
    match protocol.respond_sync(harbor_core::Target::Dm(recipient_bytes), &recipient_bytes, data).await {
        Ok(()) => http_response(200, "DM sync response sent"),
        Err(e) => http_response(500, &format!("Failed to send DM sync response: {}", e)),
    }
}

// ============================================================================
// DM Share API Handlers
// ============================================================================

/// POST /api/dm/share
/// Body: {"recipient": "hex64", "file_path": "/path/to/file"}
pub async fn handle_dm_share(protocol: &Protocol, body: &str) -> String {
    let recipient_hex = match extract_json_string(body, "recipient") {
        Some(r) => r,
        None => return http_response(400, "Missing 'recipient' field"),
    };

    let file_path = match extract_json_string(body, "file_path") {
        Some(p) => p,
        None => return http_response(400, "Missing 'file_path' field"),
    };

    let recipient_bytes = match hex::decode(&recipient_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid recipient ID (must be 64 hex chars)"),
    };

    match protocol.share_file(harbor_core::Target::Dm(recipient_bytes), &file_path).await {
        Ok(hash) => {
            let json = format!(r#"{{"hash":"{}","status":"announced"}}"#, hex::encode(hash));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to share DM file: {}", e)),
    }
}

// ============================================================================
// Stream API Handlers
// ============================================================================

/// Request a DM stream to a peer (peer-to-peer, no topic)
/// POST /api/dm/stream/request
/// Body: {"peer": "hex...", "name": "stream-name"}
pub async fn handle_dm_stream_request(protocol: &Protocol, body: &str) -> String {
    let peer_hex = match extract_json_string(body, "peer") {
        Some(p) => p,
        None => return http_response(400, "Missing 'peer' field"),
    };
    let name = extract_json_string(body, "name").unwrap_or_else(|| "test".to_string());

    let peer_bytes = match hex::decode(&peer_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid peer ID"),
    };

    match protocol.dm_stream_request(&peer_bytes, &name).await {
        Ok(request_id) => {
            let json = format!(r#"{{"request_id":"{}"}}"#, hex::encode(request_id));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to request DM stream: {}", e)),
    }
}

/// Request a stream to a peer
/// POST /api/stream/request
/// Body: {"topic": "hex...", "peer": "hex...", "name": "stream-name"}
pub async fn handle_stream_request(protocol: &Protocol, body: &str) -> String {
    let topic_hex = match extract_json_string(body, "topic") {
        Some(t) => t,
        None => return http_response(400, "Missing 'topic' field"),
    };
    let peer_hex = match extract_json_string(body, "peer") {
        Some(p) => p,
        None => return http_response(400, "Missing 'peer' field"),
    };
    let name = extract_json_string(body, "name").unwrap_or_else(|| "test".to_string());

    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID"),
    };
    let peer_bytes = match hex::decode(&peer_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid peer ID"),
    };

    match protocol.request_stream(&topic_bytes, &peer_bytes, &name, vec![]).await {
        Ok(request_id) => {
            let json = format!(r#"{{"request_id":"{}"}}"#, hex::encode(request_id));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to request stream: {}", e)),
    }
}

/// Accept a stream request
/// POST /api/stream/accept
/// Body: {"request_id": "hex..."}
pub async fn handle_stream_accept(protocol: &Protocol, body: &str) -> String {
    let id_hex = match extract_json_string(body, "request_id") {
        Some(h) => h,
        None => return http_response(400, "Missing 'request_id' field"),
    };
    let id_bytes = match hex::decode(&id_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid request_id"),
    };

    match protocol.accept_stream(&id_bytes).await {
        Ok(()) => http_response(200, "Stream accepted"),
        Err(e) => http_response(500, &format!("Failed to accept stream: {}", e)),
    }
}

/// Reject a stream request
/// POST /api/stream/reject
/// Body: {"request_id": "hex...", "reason": "optional reason"}
pub async fn handle_stream_reject(protocol: &Protocol, body: &str) -> String {
    let id_hex = match extract_json_string(body, "request_id") {
        Some(h) => h,
        None => return http_response(400, "Missing 'request_id' field"),
    };
    let reason = extract_json_string(body, "reason");

    let id_bytes = match hex::decode(&id_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid request_id"),
    };

    match protocol.reject_stream(&id_bytes, reason).await {
        Ok(()) => http_response(200, "Stream rejected"),
        Err(e) => http_response(500, &format!("Failed to reject stream: {}", e)),
    }
}

/// Publish data to an active stream
/// POST /api/stream/publish
/// Body: {"request_id": "hex...", "data": "hex-encoded-payload"}
pub async fn handle_stream_publish(active_tracks: &ActiveTracks, body: &str) -> String {
    let id_hex = match extract_json_string(body, "request_id") {
        Some(h) => h,
        None => return http_response(400, "Missing 'request_id' field"),
    };
    let data_hex = match extract_json_string(body, "data") {
        Some(d) => d,
        None => return http_response(400, "Missing 'data' field"),
    };

    let id_bytes = match hex::decode(&id_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid request_id"),
    };
    let data = match hex::decode(&data_hex) {
        Ok(d) => d,
        Err(_) => return http_response(400, "Invalid hex data"),
    };

    // Write frame to the existing track (created on StreamConnected)
    let mut tracks = active_tracks.write().await;
    match tracks.get_mut(&id_bytes) {
        Some(track) => {
            track.write_frame(bytes::Bytes::from(data));
            info!(
                request_id = %hex::encode(&id_bytes[..8]),
                "Stream data published"
            );
            http_response(200, "Published")
        }
        None => http_response(404, "No active track for this stream — not connected yet?"),
    }
}

/// End an active stream
/// POST /api/stream/end
/// Body: {"request_id": "hex..."}
pub async fn handle_stream_end(protocol: &Protocol, body: &str) -> String {
    let id_hex = match extract_json_string(body, "request_id") {
        Some(h) => h,
        None => return http_response(400, "Missing 'request_id' field"),
    };
    let id_bytes = match hex::decode(&id_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid request_id"),
    };

    match protocol.end_stream(&id_bytes).await {
        Ok(()) => http_response(200, "Stream ended"),
        Err(e) => http_response(500, &format!("Failed to end stream: {}", e)),
    }
}

// ============================================================================
// Control API Handlers
// ============================================================================

/// Request a connection to a peer
/// POST /api/control/connect
/// Body: {"peer": "hex64", "relay_url": "optional", "display_name": "optional", "token": "hex64 optional"}
pub async fn handle_control_connect(protocol: &Protocol, body: &str) -> String {
    let peer_hex = match extract_json_string(body, "peer") {
        Some(p) => p,
        None => return http_response(400, "Missing 'peer' field"),
    };
    let relay_url = extract_json_string(body, "relay_url");
    let display_name = extract_json_string(body, "display_name");
    let token_hex = extract_json_string(body, "token");

    let peer_bytes = match hex::decode(&peer_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid peer ID (must be 64 hex chars)"),
    };

    let token = if let Some(t) = token_hex {
        match hex::decode(&t) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                Some(arr)
            }
            _ => return http_response(400, "Invalid token (must be 64 hex chars)"),
        }
    } else {
        None
    };

    match protocol.request_connection(&peer_bytes, relay_url.as_deref(), display_name.as_deref(), token).await {
        Ok(request_id) => {
            let json = format!(r#"{{"request_id":"{}"}}"#, hex::encode(request_id));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to request connection: {}", e)),
    }
}

/// Accept a pending connection request
/// POST /api/control/accept
/// Body: {"request_id": "hex64"}
pub async fn handle_control_accept(protocol: &Protocol, body: &str) -> String {
    let id_hex = match extract_json_string(body, "request_id") {
        Some(h) => h,
        None => return http_response(400, "Missing 'request_id' field"),
    };
    let id_bytes = match hex::decode(&id_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid request_id"),
    };

    match protocol.accept_connection(&id_bytes).await {
        Ok(()) => http_response(200, "Connection accepted"),
        Err(e) => http_response(500, &format!("Failed to accept connection: {}", e)),
    }
}

/// Decline a pending connection request
/// POST /api/control/decline
/// Body: {"request_id": "hex64", "reason": "optional"}
pub async fn handle_control_decline(protocol: &Protocol, body: &str) -> String {
    let id_hex = match extract_json_string(body, "request_id") {
        Some(h) => h,
        None => return http_response(400, "Missing 'request_id' field"),
    };
    let reason = extract_json_string(body, "reason");

    let id_bytes = match hex::decode(&id_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid request_id"),
    };

    match protocol.decline_connection(&id_bytes, reason.as_deref()).await {
        Ok(()) => http_response(200, "Connection declined"),
        Err(e) => http_response(500, &format!("Failed to decline connection: {}", e)),
    }
}

/// Generate a connect invite (for QR code / invite string)
/// POST /api/control/invite
pub async fn handle_control_generate_invite(protocol: &Protocol) -> String {
    match protocol.generate_connect_invite().await {
        Ok(invite) => {
            let json = format!(
                r#"{{"endpoint_id":"{}","token":"{}","relay_url":"{}","invite":"{}"}}"#,
                hex::encode(invite.endpoint_id),
                hex::encode(invite.token),
                invite.relay_url.as_deref().unwrap_or(""),
                invite.encode()
            );
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to generate invite: {}", e)),
    }
}

/// Connect using an invite string
/// POST /api/control/connect-with-invite
/// Body: {"invite": "endpoint:token:relay"}
pub async fn handle_control_connect_with_invite(protocol: &Protocol, body: &str) -> String {
    let invite_str = match extract_json_string(body, "invite") {
        Some(i) => i,
        None => return http_response(400, "Missing 'invite' field"),
    };

    match protocol.connect_with_invite(&invite_str).await {
        Ok(request_id) => {
            let json = format!(r#"{{"request_id":"{}"}}"#, hex::encode(request_id));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to connect with invite: {}", e)),
    }
}

/// Block a peer
/// POST /api/control/block
/// Body: {"peer": "hex64"}
pub async fn handle_control_block(protocol: &Protocol, body: &str) -> String {
    let peer_hex = match extract_json_string(body, "peer") {
        Some(p) => p,
        None => return http_response(400, "Missing 'peer' field"),
    };
    let peer_bytes = match hex::decode(&peer_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid peer ID"),
    };

    match protocol.block_peer(&peer_bytes).await {
        Ok(()) => http_response(200, "Peer blocked"),
        Err(e) => http_response(500, &format!("Failed to block peer: {}", e)),
    }
}

/// Unblock a peer
/// POST /api/control/unblock
/// Body: {"peer": "hex64"}
pub async fn handle_control_unblock(protocol: &Protocol, body: &str) -> String {
    let peer_hex = match extract_json_string(body, "peer") {
        Some(p) => p,
        None => return http_response(400, "Missing 'peer' field"),
    };
    let peer_bytes = match hex::decode(&peer_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid peer ID"),
    };

    match protocol.unblock_peer(&peer_bytes).await {
        Ok(()) => http_response(200, "Peer unblocked"),
        Err(e) => http_response(500, &format!("Failed to unblock peer: {}", e)),
    }
}

/// List all connections
/// GET /api/control/connections
pub async fn handle_control_list_connections(protocol: &Protocol) -> String {
    match protocol.list_connections().await {
        Ok(connections) => {
            let conn_jsons: Vec<String> = connections.iter().map(|c| {
                let state_str = match c.state {
                    harbor_core::ConnectionState::PendingOutgoing => "pending_outgoing",
                    harbor_core::ConnectionState::PendingIncoming => "pending_incoming",
                    harbor_core::ConnectionState::Connected => "connected",
                    harbor_core::ConnectionState::Declined => "declined",
                    harbor_core::ConnectionState::Blocked => "blocked",
                };
                format!(
                    r#"{{"peer_id":"{}","state":"{}","display_name":"{}","relay_url":"{}"}}"#,
                    hex::encode(c.peer_id),
                    state_str,
                    c.display_name.as_deref().unwrap_or(""),
                    c.relay_url.as_deref().unwrap_or("")
                )
            }).collect();
            let json = format!(r#"{{"connections":[{}]}}"#, conn_jsons.join(","));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to list connections: {}", e)),
    }
}

/// Invite a peer to a topic
/// POST /api/control/topic-invite
/// Body: {"peer": "hex64", "topic": "hex64"}
pub async fn handle_control_topic_invite(protocol: &Protocol, body: &str) -> String {
    let peer_hex = match extract_json_string(body, "peer") {
        Some(p) => p,
        None => return http_response(400, "Missing 'peer' field"),
    };
    let topic_hex = match extract_json_string(body, "topic") {
        Some(t) => t,
        None => return http_response(400, "Missing 'topic' field"),
    };

    let peer_bytes = match hex::decode(&peer_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid peer ID"),
    };
    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID"),
    };

    match protocol.invite_to_topic(&peer_bytes, &topic_bytes).await {
        Ok(message_id) => {
            let json = format!(r#"{{"message_id":"{}"}}"#, hex::encode(message_id));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to invite to topic: {}", e)),
    }
}

/// Accept a topic invitation
/// POST /api/control/topic-accept
/// Body: {"message_id": "hex64"}
pub async fn handle_control_topic_accept(protocol: &Protocol, body: &str) -> String {
    let id_hex = match extract_json_string(body, "message_id") {
        Some(h) => h,
        None => return http_response(400, "Missing 'message_id' field"),
    };
    let id_bytes = match hex::decode(&id_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid message_id"),
    };

    match protocol.accept_topic_invite(&id_bytes).await {
        Ok(()) => http_response(200, "Topic invite accepted"),
        Err(e) => http_response(500, &format!("Failed to accept topic invite: {}", e)),
    }
}

/// Decline a topic invitation
/// POST /api/control/topic-decline
/// Body: {"message_id": "hex64"}
pub async fn handle_control_topic_decline(protocol: &Protocol, body: &str) -> String {
    let id_hex = match extract_json_string(body, "message_id") {
        Some(h) => h,
        None => return http_response(400, "Missing 'message_id' field"),
    };
    let id_bytes = match hex::decode(&id_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid message_id"),
    };

    match protocol.decline_topic_invite(&id_bytes).await {
        Ok(()) => http_response(200, "Topic invite declined"),
        Err(e) => http_response(500, &format!("Failed to decline topic invite: {}", e)),
    }
}

/// Leave a topic via Control protocol
/// POST /api/control/leave
/// Body: {"topic": "hex64"}
pub async fn handle_control_leave(protocol: &Protocol, body: &str) -> String {
    let topic_hex = match extract_json_string(body, "topic") {
        Some(t) => t,
        None => return http_response(400, "Missing 'topic' field"),
    };
    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID"),
    };

    match protocol.leave_topic(&topic_bytes).await {
        Ok(()) => http_response(200, "Left topic"),
        Err(e) => http_response(500, &format!("Failed to leave topic: {}", e)),
    }
}

/// Remove a member from a topic (admin only)
/// POST /api/control/remove-member
/// Body: {"topic": "hex64", "member": "hex64"}
pub async fn handle_control_remove_member(protocol: &Protocol, body: &str) -> String {
    let topic_hex = match extract_json_string(body, "topic") {
        Some(t) => t,
        None => return http_response(400, "Missing 'topic' field"),
    };
    let member_hex = match extract_json_string(body, "member") {
        Some(m) => m,
        None => return http_response(400, "Missing 'member' field"),
    };

    let topic_bytes = match hex::decode(&topic_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid topic ID"),
    };
    let member_bytes = match hex::decode(&member_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid member ID"),
    };

    match protocol.remove_topic_member(&topic_bytes, &member_bytes).await {
        Ok(()) => http_response(200, "Member removed"),
        Err(e) => http_response(500, &format!("Failed to remove member: {}", e)),
    }
}

/// Suggest a peer to another peer (introduction)
/// POST /api/control/suggest
/// Body: {"to_peer": "hex64", "suggested_peer": "hex64", "note": "optional"}
pub async fn handle_control_suggest(protocol: &Protocol, body: &str) -> String {
    let to_hex = match extract_json_string(body, "to_peer") {
        Some(t) => t,
        None => return http_response(400, "Missing 'to_peer' field"),
    };
    let suggested_hex = match extract_json_string(body, "suggested_peer") {
        Some(s) => s,
        None => return http_response(400, "Missing 'suggested_peer' field"),
    };
    let note = extract_json_string(body, "note");

    let to_bytes = match hex::decode(&to_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid to_peer ID"),
    };
    let suggested_bytes = match hex::decode(&suggested_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return http_response(400, "Invalid suggested_peer ID"),
    };

    match protocol.suggest_peer(&to_bytes, &suggested_bytes, note.as_deref()).await {
        Ok(message_id) => {
            let json = format!(r#"{{"message_id":"{}"}}"#, hex::encode(message_id));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to suggest peer: {}", e)),
    }
}

/// List pending topic invites
/// GET /api/control/pending-invites
pub async fn handle_control_pending_invites(protocol: &Protocol) -> String {
    match protocol.list_pending_invites().await {
        Ok(invites) => {
            let invite_jsons: Vec<String> = invites.iter().map(|i| {
                format!(
                    r#"{{"message_id":"{}","topic_id":"{}","sender_id":"{}","topic_name":"{}"}}"#,
                    hex::encode(i.message_id),
                    hex::encode(i.topic_id),
                    hex::encode(i.sender_id),
                    i.topic_name.as_deref().unwrap_or("")
                )
            }).collect();
            let json = format!(r#"{{"pending_invites":[{}]}}"#, invite_jsons.join(","));
            http_json_response(200, &json)
        }
        Err(e) => http_response(500, &format!("Failed to list pending invites: {}", e)),
    }
}
