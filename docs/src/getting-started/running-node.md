# Running a Node

How to start and configure a Harbor node in your application.

## Basic Startup

```rust
use harbor_core::{Protocol, ProtocolConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_key: [u8; 32] = get_db_key(); // See Database & Keyring docs

    let protocol = Protocol::start(
        ProtocolConfig::default()
            .with_db_key(db_key)
    ).await?;

    // Your app logic...

    protocol.stop().await;
    Ok(())
}
```

## Node Identity

Each node has a persistent Ed25519 identity stored in the database:

```rust
// Get your EndpointID (public key)
let my_id: [u8; 32] = protocol.endpoint_id();
println!("My EndpointID: {}", hex::encode(&my_id));

// Get relay URL (for NAT traversal)
if let Some(relay) = protocol.relay_url().await {
    println!("Relay: {}", relay);
}
```

The identity is created on first run and persists across restarts (as long as you use the same database with the same key).

## Configuration Options

### Database & Storage

```rust
let config = ProtocolConfig::default()
    .with_db_key(key)
    .with_db_path("/path/to/app.db".into())     // Default: "harbor_protocol.db"
    .with_blob_path("/path/to/blobs".into())    // Default: ".harbor_blobs/"
    .with_max_storage(50 * 1024 * 1024 * 1024); // Default: 10 GB
```

### Bootstrap Nodes

```rust
// Use default bootstrap nodes (recommended)
let config = ProtocolConfig::default();

// Add additional bootstrap node
let config = ProtocolConfig::default()
    .with_bootstrap_node("abc123...".to_string());

// Replace all bootstrap nodes
let config = ProtocolConfig::default()
    .with_bootstrap_nodes(vec![
        "node1...".to_string(),
        "node2...".to_string(),
    ]);
```

### Timing & Intervals

```rust
let config = ProtocolConfig::default()
    .with_harbor_pull_interval(60)           // Pull from Harbor Nodes (seconds)
    .with_harbor_sync_interval(300)          // Harbor-to-Harbor sync (seconds)
    .with_replication_factor(3)              // Replicate to N Harbor Nodes
    .with_dht_stable_refresh_interval(120);  // DHT refresh (seconds)
```

### Testing Configuration

For faster iteration during development:

```rust
let config = ProtocolConfig::for_testing()
    .with_db_key(key);
```

This uses shorter intervals, higher replication, and disabled Proof of Work.

## Listening for Events

Events are the primary way to receive data from the network:

```rust
let mut events = protocol.events().await?;

tokio::spawn(async move {
    while let Some(event) = events.recv().await {
        match event {
            ProtocolEvent::Message(msg) => {
                // Topic message received
            }
            ProtocolEvent::DmReceived(msg) => {
                // Direct message received
            }
            ProtocolEvent::ConnectRequest { peer_id, request_id, .. } => {
                // Someone wants to connect
            }
            ProtocolEvent::FileAnnounced(file) => {
                // File shared in a topic
            }
            ProtocolEvent::FileComplete(hash) => {
                // Download finished
            }
            ProtocolEvent::SyncUpdate { topic_id, sender, data } => {
                // CRDT delta received
            }
            _ => {}
        }
    }
});
```

## Graceful Shutdown

Always stop the protocol cleanly:

```rust
// Wait for shutdown signal
tokio::signal::ctrl_c().await?;

// Stop the protocol (closes connections, saves state)
protocol.stop().await;
```

## Running Multiple Nodes

For testing, you can run multiple nodes in the same process:

```rust
let node1 = Protocol::start(
    ProtocolConfig::for_testing()
        .with_db_key(key1)
        .with_db_path("node1.db".into())
).await?;

let node2 = Protocol::start(
    ProtocolConfig::for_testing()
        .with_db_key(key2)
        .with_db_path("node2.db".into())
).await?;

// Nodes can now communicate...
```

## Production Considerations

- **Database location**: Use a persistent, secure location (not temp directories)
- **Key management**: Use OS keyring or secure key derivation (see [Database & Keyring](../security/database.md))
- **Error handling**: Handle `ProtocolError` appropriately
- **Logging**: Set `RUST_LOG=info` or `debug` for troubleshooting

For running infrastructure (bootstrap nodes, Harbor nodes), see [Bootstrap Nodes](../operating/bootstrap.md).
