//! Harbor Protocol CLI
//!
//! Run a full Harbor node with DHT, packet storage, and message relay.
//!
//! Usage:
//!   harbor-cli --serve                              # Run as persistent node
//!   harbor-cli --serve --api-port 8080              # Run with HTTP API
//!   harbor-cli --serve --bootstrap <id>:<relay>     # Use custom bootstrap node
//!   harbor-cli --serve --no-default-bootstrap       # Don't use default bootstrap

mod http;
mod events;

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use harbor_core::{Protocol, ProtocolConfig};

/// Parse HARBOR_DB_KEY env var (64 hex chars) into a 32-byte key.
fn db_key_from_env() -> [u8; 32] {
    let hex_str = env::var("HARBOR_DB_KEY").unwrap_or_else(|_| {
        eprintln!("Error: HARBOR_DB_KEY environment variable is not set.");
        eprintln!("  Set it to a 64-character hex string (32 bytes), e.g.:");
        eprintln!("  export HARBOR_DB_KEY=$(openssl rand -hex 32)");
        std::process::exit(1);
    });
    let bytes = hex::decode(&hex_str).unwrap_or_else(|_| {
        eprintln!("Error: HARBOR_DB_KEY is not valid hex.");
        std::process::exit(1);
    });
    if bytes.len() != 32 {
        eprintln!("Error: HARBOR_DB_KEY must be exactly 64 hex characters (32 bytes), got {}.", hex_str.len());
        std::process::exit(1);
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    key
}

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
    println!("  --blob-path <PATH>          Blob storage path (default: .harbor_blobs/ next to db)");
    println!("  --api-port <PORT>           Enable HTTP API on this port");
    println!("  --max-storage <SIZE>        Max harbor storage (default: 10GB, e.g., 50GB, 100MB)");
    println!("  --bootstrap <ID:RELAY>      Use this bootstrap node (replaces defaults)");
    println!("  --no-default-bootstrap      Don't use any bootstrap (for first node)");
    println!("  --testing                   Use shorter intervals (5-10s) for testing, enables sync");
    println!("  --sync                      Enable CRDT sync feature");
    println!("  --help, -h                  Show this help");
    println!();
    println!("API Endpoints (when --api-port is set):");
    println!("  POST /api/topic             Create topic, returns invite");
    println!("  POST /api/join              Join topic (invite in body)");
    println!("  POST /api/send              Send message (JSON: topic, message)");
    println!("  POST /api/dm                Send DM (JSON: recipient, message)");
    println!("  GET  /api/stats             Get node statistics");
    println!("  GET  /api/topics            List all topics");
    println!("  GET  /api/invite/:topic     Get fresh invite for topic (all current members)");
    println!("  GET  /api/bootstrap         Get this node's bootstrap info");
    println!();
    println!("File Sharing Endpoints:");
    println!("  POST /api/share             Share file (JSON: topic, file_path)");
    println!("  POST /api/share/pull        Request blob download (JSON: hash)");
    println!("  POST /api/share/export      Export blob to file (JSON: hash, dest_path)");
    println!("  GET  /api/share/status/:hash Get share progress");
    println!("  GET  /api/share/blobs/:topic List blobs for topic");
    println!();
    println!("Sync Endpoints (CRDT collaboration):");
    println!("  POST /api/sync/enable       Enable sync for topic (JSON: topic)");
    println!("  POST /api/sync/text/insert  Insert text (JSON: topic, container, index, text)");
    println!("  POST /api/sync/text/delete  Delete text (JSON: topic, container, index, len)");
    println!("  GET  /api/sync/text/:topic/:container  Get text value");
    println!("  GET  /api/sync/status/:topic Get sync status");
    println!();
    println!("DM Sync Endpoints:");
    println!("  POST /api/dm/sync/update    Send DM sync update (JSON: recipient, data)");
    println!("  POST /api/dm/sync/request   Request DM sync state (JSON: recipient)");
    println!("  POST /api/dm/sync/respond   Respond to DM sync request (JSON: recipient, data)");
    println!();
    println!("DM Share Endpoints:");
    println!("  POST /api/dm/share          Share a file via DM (JSON: recipient, file_path)");
    println!();
    println!("Control Endpoints (peer connections & topic invites):");
    println!("  POST /api/control/connect          Request peer connection (JSON: peer, relay_url?, display_name?, token?)");
    println!("  POST /api/control/accept           Accept connection request (JSON: request_id)");
    println!("  POST /api/control/decline          Decline connection request (JSON: request_id, reason?)");
    println!("  POST /api/control/invite           Generate connect invite (QR code)");
    println!("  POST /api/control/connect-with-invite  Connect using invite (JSON: invite)");
    println!("  POST /api/control/block            Block a peer (JSON: peer)");
    println!("  POST /api/control/unblock          Unblock a peer (JSON: peer)");
    println!("  GET  /api/control/connections      List all connections");
    println!("  POST /api/control/topic-invite     Invite peer to topic (JSON: peer, topic)");
    println!("  POST /api/control/topic-accept     Accept topic invite (JSON: message_id)");
    println!("  POST /api/control/topic-decline    Decline topic invite (JSON: message_id)");
    println!("  POST /api/control/leave            Leave topic (JSON: topic)");
    println!("  POST /api/control/remove-member    Remove member from topic (JSON: topic, member)");
    println!("  POST /api/control/suggest          Suggest peer introduction (JSON: to_peer, suggested_peer, note?)");
    println!("  GET  /api/control/pending-invites  List pending topic invites");
    println!();
    println!("Environment:");
    println!("  HARBOR_DB_KEY               Database encryption key (64 hex chars, required)");
    println!("  RUST_LOG                    Set log level (e.g., info, debug)");
}

/// Parse a human-readable size string into bytes (e.g., "50GB", "100MB", "1TB")
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_uppercase();

    // Find where the number ends and unit begins
    let num_end = s.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(s.len());
    let (num_str, unit) = s.split_at(num_end);

    let num: f64 = num_str.parse().ok()?;

    let multiplier: u64 = match unit.trim() {
        "" | "B" => 1,
        "KB" | "K" => 1024,
        "MB" | "M" => 1024 * 1024,
        "GB" | "G" => 1024 * 1024 * 1024,
        "TB" | "T" => 1024 * 1024 * 1024 * 1024,
        _ => return None,
    };

    Some((num * multiplier as f64) as u64)
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

    // Parse --blob-path (default: hidden folder next to db)
    let blob_path: PathBuf = args.windows(2)
        .find(|w| w[0] == "--blob-path")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| {
            // Default: .harbor_blobs/ in same directory as database
            db_path.parent()
                .map(|p| p.join(".harbor_blobs"))
                .unwrap_or_else(|| PathBuf::from(".harbor_blobs"))
        });

    // Parse --api-port
    let api_port: Option<u16> = args.windows(2)
        .find(|w| w[0] == "--api-port")
        .and_then(|w| w[1].parse().ok());

    // Parse --bootstrap (can be specified multiple times)
    let custom_bootstraps: Vec<(String, Option<String>)> = args.windows(2)
        .filter(|w| w[0] == "--bootstrap")
        .filter_map(|w| parse_bootstrap_arg(&w[1]))
        .collect();

    // Parse --max-storage
    let max_storage: Option<u64> = args.windows(2)
        .find(|w| w[0] == "--max-storage")
        .and_then(|w| {
            parse_size(&w[1]).or_else(|| {
                eprintln!("⚠️  Invalid --max-storage value: {}", w[1]);
                eprintln!("   Expected format: 50GB, 100MB, 1TB, etc.");
                None
            })
        });

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

    let db_key = db_key_from_env();

    // Build config - use testing intervals if --testing flag is set
    let mut config = if testing_mode {
        ProtocolConfig::for_testing()
            .with_db_key(db_key)
            .with_db_path(db_path.clone())
            .with_blob_path(blob_path.clone())
    } else {
        ProtocolConfig::default()
            .with_db_key(db_key)
            .with_db_path(db_path.clone())
            .with_blob_path(blob_path.clone())
    };

    // Apply max storage if specified
    if let Some(bytes) = max_storage {
        config = config.with_max_storage(bytes);
        let display = if bytes >= 1024 * 1024 * 1024 * 1024 {
            format!("{:.1} TB", bytes as f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0))
        } else if bytes >= 1024 * 1024 * 1024 {
            format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        } else if bytes >= 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{} bytes", bytes)
        };
        println!("Max storage: {}", display);
    }

    // Handle bootstrap configuration
    if !custom_bootstraps.is_empty() {
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
        config = config.with_bootstrap_nodes(vec![]);
        println!("Bootstrap: none (--no-default-bootstrap)");
    } else {
        println!("Bootstrap: using defaults");
    }

    // Start the protocol
    println!("Starting Harbor Protocol...");
    println!("Database: {}", db_path.display());
    println!("Blob storage: {}", blob_path.display());

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
    let bootstrap_info = Arc::new(http::BootstrapInfo {
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

    // Shared state for active stream tracks
    let active_tracks: http::ActiveTracks = Arc::new(RwLock::new(HashMap::new()));

    // Start API server if port specified
    let api_handle = if let Some(port) = api_port {
        let protocol_clone = protocol.clone();
        let bootstrap_clone = bootstrap_info.clone();
        let tracks_clone = active_tracks.clone();
        Some(tokio::spawn(async move {
            http::run_api_server(protocol_clone, bootstrap_clone, port, tracks_clone).await;
        }))
    } else {
        None
    };

    // Run event loop until shutdown
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            println!();
            info!("Received shutdown signal");
        }
        _ = events::run_event_loop(protocol.clone(), active_tracks.clone()) => {}
    }

    // Cleanup
    if let Some(handle) = api_handle {
        handle.abort();
    }

    println!("Shutting down...");
    protocol.stop().await;
    println!("✅ Done");
}
