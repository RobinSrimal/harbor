# harbor-core

The foundation crate for Harbor Protocol - peer-to-peer topic-based messaging.

## Features

- **Topic-based messaging**: Send encrypted messages to all members of a topic
- **CRDT sync primitives**: Three sync operations for building collaborative applications (SyncUpdate, SyncRequest, SyncResponse)
- **File sharing**: P2P distribution for large files (≥512KB) with BLAKE3 chunking
- **Offline delivery**: Harbor Nodes store messages for offline members
- **DHT routing**: Kademlia-style distributed hash table for peer discovery
- **End-to-end encryption**: All messages encrypted with topic-derived keys
- **Persistent identity**: Ed25519 keypair stored in encrypted database

## Usage

```toml
[dependencies]
harbor-core = "0.1"
```

```rust
use harbor_core::{Protocol, ProtocolConfig, ProtocolEvent, TopicInvite};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Start the protocol
    let config = ProtocolConfig::default();
    let protocol = Protocol::start(config).await?;

    // Create a new topic
    let invite = protocol.create_topic().await?;
    println!("Topic created: {}", hex::encode(invite.topic_id));

    // Send a message
    protocol.send(&invite.topic_id, b"Hello, Harbor!").await?;

    // Listen for events (messages, file announcements, etc.)
    let mut events = protocol.events().await.unwrap();
    while let Some(event) = events.recv().await {
        match event {
            ProtocolEvent::Message(msg) => {
                println!("Message: {:?}", String::from_utf8_lossy(&msg.payload));
            }
            ProtocolEvent::FileAnnounced(file) => {
                println!("File shared: {} ({} bytes)", file.display_name, file.total_size);
            }
            _ => {}
        }
    }

    Ok(())
}
```

## Architecture

```
harbor-core/
├── data/           # SQLCipher-encrypted storage + file storage for Share
├── handlers/       # Incoming/outgoing message handlers
├── network/
│   ├── dht/        # Kademlia DHT implementation
│   ├── harbor/     # Harbor Node protocol (store, pull, harbor sync)
│   ├── membership/ # Topic join/leave messages
│   ├── send/       # Direct message sending (includes CRDT sync messages)
│   ├── share/      # P2P file sharing protocol
│   └── sync/       # CRDT sync protocol (direct peer-to-peer, no size limit)
├── protocol/       # Core Protocol struct 
├── resilience/     # Rate limiting, PoW, storage limits
├── security/       # Cryptographic operations
└── tasks/          # Background tasks (harbor pull, share pull, maintenance)
```

## Core Concepts

### Topics

All communication is organized around **topics**. A topic:
- Has a 32-byte TopicID
- Maintains a list of member EndpointIDs
- Derives encryption keys from the TopicID

```rust
// Create a topic (you become the first member)
let invite = protocol.create_topic().await?;

// Share the invite with others
let invite_string = invite.to_base64();

// Join using an invite
let invite = TopicInvite::from_base64(&invite_string)?;
protocol.join_topic(invite).await?;
```

### Send Mode

Messages are delivered with best-effort semantics:

1. Encrypt and sign the packet
2. Send directly to all online members
3. Collect read receipts
4. Replicate to Harbor Nodes if any receipts are missing

```rust
// Send to all topic members (max 512 KB)
protocol.send(&topic_id, payload).await?;
```

### Share Mode (File Sharing)

For files ≥512KB, use the Share protocol for efficient P2P distribution:

```rust
// Share a large file with topic members
let hash = protocol.share_file(&topic_id, "/path/to/video.mp4").await?;
println!("Shared with hash: {}", hex::encode(hash));

// Check share status
let status = protocol.share_status(&hash).await?;
println!("Progress: {}/{} chunks", status.chunks_complete, status.total_chunks);

// Export a completed file
protocol.export_file(&hash, "/path/to/output.mp4").await?;
```

**How it works:**
1. File is split into 512KB chunks, hashed with BLAKE3
2. `FileAnnouncement` is broadcast to topic via Send protocol
3. Recipients pull chunks directly from source and other seeders
4. When a peer completes download, it broadcasts `CanSeed`
5. Chunks spread across the swarm (BitTorrent-like distribution)

**Events emitted:**
- `FileAnnounced` - A file was shared to the topic
- `FileProgress` - Download progress update
- `FileComplete` - All chunks received

### Sync Primitives (CRDT Support)

Harbor provides three primitives for building collaborative applications with CRDTs:

```rust
// Send a CRDT update (delta) to all topic members
protocol.sync_send_update(&topic_id, update_bytes).await?;

// Request full state from all topic members
protocol.sync_request(&topic_id).await?;

// Respond with full state to a specific peer (direct connection, no size limit)
protocol.sync_respond(&topic_id, &requester_id, snapshot_bytes).await?;
```

**How it works:**
1. **SyncUpdate**: Broadcasts CRDT deltas via Send protocol (max 512KB), replicated through Harbor Nodes
2. **SyncRequest**: Broadcasts a request for full state to all members
3. **SyncResponse**: Direct peer-to-peer transfer via dedicated SYNC_ALPN protocol (no size limit)

**Events emitted:**
- `SyncUpdate` - A CRDT update was received
- `SyncRequest` - A peer is requesting full state
- `SyncResponse` - Full state snapshot received

**Example use case:** The test app uses these primitives with [Loro CRDT](https://loro.dev) for real-time collaborative text editing.

### Harbor Nodes

When members are offline, packets are stored on Harbor Nodes:

- Any node can be a Harbor Node
- Nodes responsible for a HarborID (hash of TopicID) store packets
- Offline members pull missed packets when they reconnect
- Packets expire after 3 months (configurable)

### DHT

Kademlia-style distributed hash table for:
- Peer discovery by EndpointID
- Finding Harbor Nodes for a topic
- Maintaining network connectivity

## Configuration

```rust
let config = ProtocolConfig {
    // Database path (encrypted with SQLCipher)
    db_path: Some(PathBuf::from("harbor.db")),
    
    // Database encryption key (generated if not provided)
    db_key: None,
    
    // Blob storage path for shared files (defaults to .harbor_blobs/)
    blob_path: Some(PathBuf::from(".harbor_blobs")),
    
    // Bootstrap nodes for DHT
    bootstrap_nodes: vec![],
    
    // Testing mode (shorter intervals)
    testing: false,
    
    ..Default::default()
};
```

## Database Schema

The protocol uses **SQLCipher** (encrypted SQLite) for persistence.

| Table | Purpose |
|-------|---------|
| `local_node` | Local node's Ed25519 keypair |
| `peers` | Known peers with addresses and latency |
| `dht_routing` | DHT routing table entries |
| `topics` | Subscribed topics with HarborID |
| `topic_members` | EndpointID + relay URL per topic |
| `outgoing_packets` | Packets awaiting delivery confirmation |
| `outgoing_recipients` | Per-recipient delivery status |
| `harbor_cache` | Packets stored as Harbor Node |
| `harbor_recipients` | Per-recipient Harbor delivery status |
| `pulled_packets` | Tracking pulled packets (deduplication) |
| `blobs` | File metadata (hash, size, state) |
| `blob_recipients` | Per-recipient file transfer status |
| `blob_sections` | Section-level replication traces |

## Packet Security

All packets provide three guarantees:

1. **Confidentiality**: AEAD encryption with topic-derived key
2. **Integrity**: AEAD tag + HMAC
3. **Authenticity**: Ed25519 signature

```
Packet = {
    packet_id:   [u8; 32],   // Unique identifier
    harbor_id:   [u8; 32],   // Hash of TopicID
    endpoint_id: [u8; 32],   // Sender's public key
    nonce:       [u8; 12],   // AEAD nonce
    ciphertext:  Vec<u8>,    // Encrypted payload
    tag:         [u8; 16],   // AEAD tag
    mac:         [u8; 32],   // HMAC (proves topic membership)
    signature:   [u8; 64],   // Ed25519 signature
}
```

## Resilience

The `resilience` module provides protection against abuse:

- **Rate Limiting**: Per-connection and per-HarborID limits
- **Proof of Work**: Optional PoW for Harbor store requests
- **Storage Limits**: Configurable caps with automatic eviction

## CLI

The crate includes a CLI for testing:

```bash
# Run a node
cargo run -p harbor-core -- --serve

# With custom database
cargo run -p harbor-core -- --serve --db-path ./my-node.db

# With bootstrap node
cargo run -p harbor-core -- --serve --bootstrap <endpoint_id>:<relay_url>

# Testing mode (shorter intervals)
cargo run -p harbor-core -- --serve --testing
```

## Testing

```bash
# Run all tests
cargo test -p harbor-core

# Run with output
cargo test -p harbor-core -- --nocapture

# Run specific test
cargo test -p harbor-core test_name
```

## Logging

Control verbosity with `RUST_LOG`:

```bash
# Debug Harbor Node operations
RUST_LOG=harbor_core::network::harbor=debug cargo run -p harbor-core

# Debug DHT
RUST_LOG=harbor_core::network::dht=debug cargo run -p harbor-core

# Debug file sharing
RUST_LOG=harbor_core::network::share=debug cargo run -p harbor-core

# Multiple modules
RUST_LOG=info,harbor_core::network=debug cargo run -p harbor-core
```
