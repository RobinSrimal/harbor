//! Harbor Protocol CLI
//!
//! Run a full Harbor node with DHT, packet storage, and message relay.
//!
//! Usage:
//!   harbor-cli --serve                              # Run as persistent node
//!   harbor-cli --serve --api-port 8080              # Run with HTTP API
//!   harbor-cli --serve --bootstrap <id>:<relay>     # Use custom bootstrap node
//!   harbor-cli --serve --no-default-bootstrap       # Don't use default bootstrap

use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn, trace};

use harbor_core::{Protocol, ProtocolConfig, TopicInvite};

/// Fixed database key for persistent identity (32 bytes)
const DB_KEY: [u8; 32] = [
    0x48, 0x61, 0x72, 0x62, 0x6f, 0x72, 0x4e, 0x6f,  // "HarborNo"
    0x64, 0x65, 0x4b, 0x65, 0x79, 0x32, 0x30, 0x32,  // "deKey202"
    0x35, 0x5f, 0x76, 0x31, 0x5f, 0x73, 0x65, 0x72,  // "5_v1_ser"
    0x76, 0x65, 0x72, 0x5f, 0x6b, 0x65, 0x79, 0x21,  // "ver_key!"
];

fn print_usage() {
    println!("Harbor Protocol Node v0.1.0");
    println!();
    println!("Usage:");
    println!("  harbor-cli --serve                              Run as persistent node");
    println!("  harbor-cli --serve --api-port 8080              Run with HTTP API");
    println!("  harbor-cli --serve --bootstrap <id>:<relay>     Use custom bootstrap");
    println!("  harbor-cli --serve --no-default-bootstrap       Start without bootstrap");
    println!();
    println!("Options:");
    println!("  --serve, -s                 Run in serve mode (required)");
    println!("  --db-path <PATH>            Database path (default: harbor.db)");
    println!("  --api-port <PORT>           Enable HTTP API on this port");
    println!("  --bootstrap <ID:RELAY>      Use this bootstrap node (replaces defaults)");
    println!("  --no-default-bootstrap      Don't use any bootstrap (for first node)");
    println!("  --testing                   Use shorter intervals (5-10s) for testing");
    println!("  --help, -h                  Show this help");
    println!();
    println!("API Endpoints (when --api-port is set):");
    println!("  POST /api/topic             Create topic, returns invite");
    println!("  POST /api/join              Join topic (invite in body)");
    println!("  POST /api/send              Send message (JSON: topic, message)");
    println!("  POST /api/refresh_members   Trigger member discovery refresh");
    println!("  GET  /api/stats             Get node statistics");
    println!("  GET  /api/topics            List all topics");
    println!("  GET  /api/invite/:topic     Get fresh invite for topic (all current members)");
    println!("  GET  /api/bootstrap         Get this node's bootstrap info");
    println!();
    println!("Environment:");
    println!("  RUST_LOG                    Set log level (e.g., info, debug)");
}

/// Bootstrap info for sharing
struct BootstrapInfo {
    endpoint_id: String,
    relay_url: Option<String>,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    
    // Parse arguments
    let show_help = args.iter().any(|a| a == "--help" || a == "-h");
    let serve_mode = args.iter().any(|a| a == "--serve" || a == "-s");
    let no_default_bootstrap = args.iter().any(|a| a == "--no-default-bootstrap");
    let testing_mode = args.iter().any(|a| a == "--testing");
    
    // Parse --db-path
    let db_path: PathBuf = args.windows(2)
        .find(|w| w[0] == "--db-path")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| PathBuf::from("harbor.db"));
    
    // Parse --api-port
    let api_port: Option<u16> = args.windows(2)
        .find(|w| w[0] == "--api-port")
        .and_then(|w| w[1].parse().ok());

    // Parse --bootstrap (can be specified multiple times)
    let custom_bootstraps: Vec<(String, Option<String>)> = args.windows(2)
        .filter(|w| w[0] == "--bootstrap")
        .filter_map(|w| parse_bootstrap_arg(&w[1]))
        .collect();
    
    if show_help {
        print_usage();
        return;
    }

    if !serve_mode {
        print_usage();
        println!();
        println!("💡 Run with --serve to start a persistent node");
        return;
    }

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    println!("Harbor Protocol Node v0.1.0");
    println!();

    // Build config - use testing intervals if --testing flag is set
    let mut config = if testing_mode {
        ProtocolConfig::for_testing()
            .with_db_key(DB_KEY)
            .with_db_path(db_path.clone())
    } else {
        ProtocolConfig::default()
            .with_db_key(DB_KEY)
            .with_db_path(db_path.clone())
    };

    // Handle bootstrap configuration:
    // - If custom bootstrap nodes are provided, use ONLY those (ignore defaults)
    // - If --no-default-bootstrap is set, use no bootstrap
    // - Otherwise, use hardcoded defaults
    if !custom_bootstraps.is_empty() {
        // Custom bootstrap nodes provided - use ONLY these
        let bootstrap_strings: Vec<String> = custom_bootstraps
            .iter()
            .map(|(endpoint_id, relay_url)| {
                if let Some(relay) = relay_url {
                    format!("{}:{}", endpoint_id, relay)
                } else {
                    endpoint_id.clone()
                }
            })
            .collect();
        
        config = config.with_bootstrap_nodes(bootstrap_strings);
        
        for (endpoint_id, relay_url) in &custom_bootstraps {
            if let Some(relay) = relay_url {
                println!("Bootstrap: {}... @ {}", &endpoint_id[..16], relay);
            } else {
                println!("Bootstrap: {}...", &endpoint_id[..16.min(endpoint_id.len())]);
            }
        }
    } else if no_default_bootstrap {
        // Explicitly no bootstrap
        config = config.with_bootstrap_nodes(vec![]);
        println!("Bootstrap: none (--no-default-bootstrap)");
    } else {
        // Use hardcoded defaults (already in ProtocolConfig::default())
        println!("Bootstrap: using defaults");
    }

    // Start the protocol
    println!("Starting Harbor Protocol...");
    println!("Database: {}", db_path.display());
    
    let protocol = match Protocol::start(config).await {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("❌ Failed to start protocol: {}", e);
            return;
        }
    };

    let endpoint_id = protocol.endpoint_id();
    let endpoint_hex = hex::encode(&endpoint_id);

    println!();
    println!("=== Local Node Identity ===");
    println!("EndpointID: {}", endpoint_hex);

    // Wait for relay connection
    println!();
    println!("Waiting for relay connection...");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let relay_url = protocol.relay_url().await;
    if let Some(ref relay) = relay_url {
        println!("Relay: {}", relay);
    } else {
        println!("⚠️  No relay connected");
    }

    // Create bootstrap info for API
    let bootstrap_info = Arc::new(BootstrapInfo {
        endpoint_id: endpoint_hex.clone(),
        relay_url: relay_url.clone(),
    });

    // Print stats
    if let Ok(stats) = protocol.get_stats().await {
        println!();
        println!("=== Statistics ===");
        println!("DHT Nodes: {}", stats.dht.total_nodes);
        println!("Topics: {}", stats.topics.subscribed_count);
        println!("Harbor Packets: {}", stats.harbor.packets_stored);
    }

    println!();
    println!("🚀 Harbor Node running");
    if let Some(port) = api_port {
        println!("📡 API: http://127.0.0.1:{}", port);
    }
    println!();
    
    // Print bootstrap info for convenience
    if let Some(ref relay) = relay_url {
        println!("To use this node as bootstrap:");
        println!("  --bootstrap {}:{}", endpoint_hex, relay);
    }
    
    println!();
    println!("Press Ctrl+C to stop...");
    println!();

    // Start API server if port specified
    let api_handle = if let Some(port) = api_port {
        let protocol_clone = protocol.clone();
        let bootstrap_clone = bootstrap_info.clone();
        Some(tokio::spawn(async move {
            run_api_server(protocol_clone, bootstrap_clone, port).await;
        }))
    } else {
        None
    };

    // Process events
    let mut events = protocol.events().await;
    
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            println!();
            info!("Received shutdown signal");
        }
        _ = async {
            if let Some(ref mut rx) = events {
                while let Some(msg) = rx.recv().await {
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
            } else {
                std::future::pending::<()>().await
            }
        } => {}
    }

    // Cleanup
    if let Some(handle) = api_handle {
        handle.abort();
    }
    
    println!("Shutting down...");
    protocol.stop().await;
    println!("✅ Done");
}

/// Parse bootstrap argument: "endpoint_id:relay_url" or just "endpoint_id"
fn parse_bootstrap_arg(arg: &str) -> Option<(String, Option<String>)> {
    if arg.contains(':') {
        // Could be endpoint_id:relay_url or just a relay URL with colons
        // Endpoint IDs are 64 hex chars, so check if first part looks like one
        if let Some(colon_pos) = arg.find(':') {
            let first_part = &arg[..colon_pos];
            // Check if it looks like a hex endpoint ID (64 chars, all hex)
            if first_part.len() == 64 && first_part.chars().all(|c| c.is_ascii_hexdigit()) {
                let endpoint_id = first_part.to_string();
                let relay_url = arg[colon_pos + 1..].to_string();
                return Some((endpoint_id, Some(relay_url)));
            }
        }
    }
    
    // Just an endpoint ID (or invalid format)
    if arg.len() == 64 && arg.chars().all(|c| c.is_ascii_hexdigit()) {
        Some((arg.to_string(), None))
    } else {
        eprintln!("⚠️  Invalid bootstrap format: {}...", &arg[..arg.len().min(32)]);
        eprintln!("   Expected: <64-hex-endpoint-id> or <64-hex-endpoint-id>:<relay-url>");
        None
    }
}

// ============================================================================
// Simple HTTP API Server
// ============================================================================

async fn run_api_server(protocol: Arc<Protocol>, bootstrap_info: Arc<BootstrapInfo>, port: u16) {
    use tokio::net::TcpListener;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
            let response = handle_request(&protocol, &bootstrap, &request).await;
            
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

/// Find the end of HTTP headers (position after \r\n\r\n or \n\n)
fn find_header_end(data: &[u8]) -> Option<usize> {
    // Look for \r\n\r\n
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i+4] == b"\r\n\r\n" {
            return Some(i + 4);
        }
    }
    // Look for \n\n (curl sometimes uses this)
    for i in 0..data.len().saturating_sub(1) {
        if &data[i..i+2] == b"\n\n" {
            return Some(i + 2);
        }
    }
    None
}

/// Parse Content-Length header from HTTP headers string
fn parse_content_length(headers: &str) -> usize {
    for line in headers.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("content-length:") {
            if let Some(value) = line.split(':').nth(1) {
                if let Ok(len) = value.trim().parse::<usize>() {
                    return len;
                }
            }
        }
    }
    0 // No Content-Length header means no body
}

async fn handle_request(protocol: &Protocol, bootstrap: &BootstrapInfo, request: &str) -> String {
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

    match (method, path) {
        ("POST", "/api/topic") => handle_create_topic(protocol).await,
        ("POST", "/api/join") => handle_join_topic(protocol, body).await,
        ("POST", "/api/send") => handle_send(protocol, body).await,
        ("POST", "/api/refresh_members") => handle_refresh_members(protocol).await,
        ("GET", "/api/stats") => handle_stats(protocol).await,
        ("GET", "/api/topics") => handle_list_topics(protocol).await,
        ("GET", "/api/bootstrap") => handle_bootstrap(bootstrap),
        ("GET", "/api/health") => http_response(200, "OK"),
        _ => http_response(404, "Not Found"),
    }
}

async fn handle_create_topic(protocol: &Protocol) -> String {
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

async fn handle_get_invite(protocol: &Protocol, topic_hex: &str) -> String {
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

async fn handle_join_topic(protocol: &Protocol, body: &str) -> String {
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

async fn handle_send(protocol: &Protocol, body: &str) -> String {
    info!("API: handle_send started, body_len={}", body.len());
    
    // Expect JSON: {"topic": "hex...", "message": "text"}
    let topic_start = body.find("\"topic\"");
    let msg_start = body.find("\"message\"");
    
    if topic_start.is_none() || msg_start.is_none() {
        warn!("API: JSON parse failed - missing 'topic' or 'message' field, body_len={}", body.len());
        return http_response(400, "Expected JSON with 'topic' and 'message' fields");
    }

    let topic_hex = extract_json_string(body, "topic");
    let message = extract_json_string(body, "message");

    if topic_hex.is_none() || message.is_none() {
        warn!("API: JSON parse failed - couldn't extract values, body_len={}", body.len());
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
    let result = match protocol.send(&topic_bytes, message.as_bytes()).await {
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

async fn handle_refresh_members(protocol: &Protocol) -> String {
    info!("API: handle_refresh_members started");
    
    match protocol.refresh_members().await {
        Ok(count) => {
            info!("API: refresh_members completed, {} topics refreshed", count);
            http_response(200, &format!("Refreshed {} topics", count))
        }
        Err(e) => {
            warn!("API: refresh_members failed: {}", e);
            http_response(500, &format!("Failed to refresh: {}", e))
        }
    }
}

async fn handle_stats(protocol: &Protocol) -> String {
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

async fn handle_list_topics(protocol: &Protocol) -> String {
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

fn handle_bootstrap(bootstrap: &BootstrapInfo) -> String {
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

fn http_response(status: u16, body: &str) -> String {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status, status_text, body.len(), body
    )
}

fn http_json_response(status: u16, body: &str) -> String {
    let status_text = match status {
        200 => "OK",
        _ => "Error",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status, status_text, body.len(), body
    )
}

/// Simple JSON string extraction (avoids adding serde_json dependency)
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let start = json.find(&pattern)?;
    let after_key = &json[start + pattern.len()..];
    
    let colon_pos = after_key.find(':')?;
    let after_colon = &after_key[colon_pos + 1..];
    
    let quote_start = after_colon.find('"')?;
    let after_quote = &after_colon[quote_start + 1..];
    
    let mut end = 0;
    let chars: Vec<char> = after_quote.chars().collect();
    while end < chars.len() {
        if chars[end] == '"' && (end == 0 || chars[end - 1] != '\\') {
            break;
        }
        end += 1;
    }
    
    if end < chars.len() {
        Some(after_quote[..end].to_string())
    } else {
        None
    }
}
