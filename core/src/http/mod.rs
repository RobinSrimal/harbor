//! HTTP API server for Harbor CLI
//!
//! This module provides a simple HTTP API for testing and development.

mod parse;
mod handlers;

pub use handlers::{ActiveTracks, BootstrapInfo};

use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn, trace};

use harbor_core::Protocol;
use parse::{find_header_end, parse_content_length, http_response};
use handlers::*;

/// Run the HTTP API server
pub async fn run_api_server(
    protocol: Arc<Protocol>,
    bootstrap_info: Arc<BootstrapInfo>,
    port: u16,
    active_tracks: ActiveTracks,
) {
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("❌ Failed to bind API port {}: {}", port, e);
            return;
        }
    };

    info!(port = port, "API server started");

    loop {
        let (mut socket, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let protocol = protocol.clone();
        let bootstrap = bootstrap_info.clone();
        let tracks = active_tracks.clone();

        tokio::spawn(async move {
            // Buffer large enough for max message (512KB) + HTTP headers
            let mut buf = vec![0u8; 600 * 1024];
            let mut total_read = 0;

            // Read until we have complete HTTP request (headers + body based on Content-Length)
            loop {
                let n = match socket.read(&mut buf[total_read..]).await {
                    Ok(0) => break, // EOF
                    Ok(n) => n,
                    Err(_) => return,
                };
                total_read += n;

                // Check if we have complete headers (look for \r\n\r\n or \n\n)
                let data = &buf[..total_read];
                let header_end = if let Some(pos) = find_header_end(data) {
                    pos
                } else {
                    // Keep reading until we have headers
                    if total_read >= buf.len() {
                        warn!("API: request too large");
                        return;
                    }
                    continue;
                };

                // Parse Content-Length from headers
                let headers = String::from_utf8_lossy(&data[..header_end]);
                let content_length = parse_content_length(&headers);

                // Calculate expected total size
                let expected_total = header_end + content_length;

                if total_read >= expected_total {
                    break; // We have everything
                }

                // Need more data, keep reading
                if total_read >= buf.len() {
                    warn!("API: request too large");
                    return;
                }
            }

            if total_read == 0 {
                return;
            }

            let request = String::from_utf8_lossy(&buf[..total_read]);
            let response = handle_request(&protocol, &bootstrap, &tracks, &request).await;

            if let Err(e) = socket.write_all(response.as_bytes()).await {
                warn!("API: failed to send response: {}", e);
                return;
            }

            // Explicitly shutdown the socket to signal EOF to curl
            if let Err(e) = socket.shutdown().await {
                trace!("API: socket shutdown error (expected): {}", e);
            }
        });
    }
}

async fn handle_request(
    protocol: &Protocol,
    bootstrap: &BootstrapInfo,
    active_tracks: &ActiveTracks,
    request: &str,
) -> String {
    let lines: Vec<&str> = request.lines().collect();
    if lines.is_empty() {
        return http_response(400, "Bad Request");
    }

    let first_line = lines[0];
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return http_response(400, "Bad Request");
    }

    let method = parts[0];
    let path = parts[1];

    // Extract body (after empty line)
    let body = request
        .split("\r\n\r\n")
        .nth(1)
        .or_else(|| request.split("\n\n").nth(1))
        .unwrap_or("")
        .trim();

    // Handle /api/invite/:topic path
    if method == "GET" && path.starts_with("/api/invite/") {
        let topic_hex = &path[12..]; // Strip "/api/invite/"
        return handle_get_invite(protocol, topic_hex).await;
    }

    // Handle /api/share/status/:hash path
    if method == "GET" && path.starts_with("/api/share/status/") {
        let hash_hex = &path[18..]; // Strip "/api/share/status/"
        return handle_share_status(protocol, hash_hex).await;
    }

    // Handle /api/share/blobs/:topic path
    if method == "GET" && path.starts_with("/api/share/blobs/") {
        let topic_hex = &path[17..]; // Strip "/api/share/blobs/"
        return handle_list_blobs(protocol, topic_hex).await;
    }

    match (method, path) {
        ("POST", "/api/topic") => handle_create_topic(protocol).await,
        ("POST", "/api/join") => handle_join_topic(protocol, body).await,
        ("POST", "/api/send") => handle_send(protocol, body).await,
        ("POST", "/api/dm") => handle_send_dm(protocol, body).await,
        ("POST", "/api/share") => handle_share_file(protocol, body).await,
        ("POST", "/api/share/pull") => handle_share_pull(protocol, body).await,
        ("POST", "/api/share/export") => handle_share_export(protocol, body).await,
        // Sync transport endpoints
        ("POST", "/api/sync/update") => handle_sync_update(protocol, body).await,
        ("POST", "/api/sync/request") => handle_sync_request(protocol, body).await,
        ("POST", "/api/sync/respond") => handle_sync_respond(protocol, body).await,
        // DM sync endpoints
        ("POST", "/api/dm/sync/update") => handle_dm_sync_update(protocol, body).await,
        ("POST", "/api/dm/sync/request") => handle_dm_sync_request(protocol, body).await,
        ("POST", "/api/dm/sync/respond") => handle_dm_sync_respond(protocol, body).await,
        // DM share endpoints
        ("POST", "/api/dm/share") => handle_dm_share(protocol, body).await,
        // DM stream endpoints
        ("POST", "/api/dm/stream/request") => handle_dm_stream_request(protocol, body).await,
        // Stream endpoints
        ("POST", "/api/stream/request") => handle_stream_request(protocol, body).await,
        ("POST", "/api/stream/accept") => handle_stream_accept(protocol, body).await,
        ("POST", "/api/stream/reject") => handle_stream_reject(protocol, body).await,
        ("POST", "/api/stream/publish") => handle_stream_publish(active_tracks, body).await,
        ("POST", "/api/stream/end") => handle_stream_end(protocol, body).await,
        // Control endpoints
        ("POST", "/api/control/connect") => handle_control_connect(protocol, body).await,
        ("POST", "/api/control/accept") => handle_control_accept(protocol, body).await,
        ("POST", "/api/control/decline") => handle_control_decline(protocol, body).await,
        ("POST", "/api/control/invite") => handle_control_generate_invite(protocol).await,
        ("POST", "/api/control/connect-with-invite") => handle_control_connect_with_invite(protocol, body).await,
        ("POST", "/api/control/block") => handle_control_block(protocol, body).await,
        ("POST", "/api/control/unblock") => handle_control_unblock(protocol, body).await,
        ("GET", "/api/control/connections") => handle_control_list_connections(protocol).await,
        ("POST", "/api/control/topic-invite") => handle_control_topic_invite(protocol, body).await,
        ("POST", "/api/control/topic-accept") => handle_control_topic_accept(protocol, body).await,
        ("POST", "/api/control/topic-decline") => handle_control_topic_decline(protocol, body).await,
        ("POST", "/api/control/leave") => handle_control_leave(protocol, body).await,
        ("POST", "/api/control/remove-member") => handle_control_remove_member(protocol, body).await,
        ("POST", "/api/control/suggest") => handle_control_suggest(protocol, body).await,
        ("GET", "/api/control/pending-invites") => handle_control_pending_invites(protocol).await,
        ("GET", "/api/stats") => handle_stats(protocol).await,
        ("GET", "/api/topics") => handle_list_topics(protocol).await,
        ("GET", "/api/bootstrap") => handle_bootstrap(bootstrap),
        ("GET", "/api/health") => http_response(200, "OK"),
        _ => http_response(404, "Not Found"),
    }
}
