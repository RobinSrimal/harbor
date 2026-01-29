# harbor-core

The core crate for Harbor — peer-to-peer messaging with offline delivery.

## Usage

```rust
use harbor_core::{Protocol, ProtocolConfig, ProtocolEvent, Target, TopicInvite};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ProtocolConfig::default();
    let protocol = Protocol::start(config).await?;

    // Create a topic
    let invite = protocol.create_topic().await?;

    // Send a message to all topic members
    protocol.send(Target::Topic(invite.topic_id), b"Hello, Harbor!").await?;

    // Send a direct message
    protocol.send(Target::Dm(peer_id), b"Hello directly!").await?;

    // Listen for events
    let mut events = protocol.events().await.unwrap();
    while let Some(event) = events.recv().await {
        match event {
            ProtocolEvent::Message(msg) => {
                println!("Message: {:?}", String::from_utf8_lossy(&msg.payload));
            }
            ProtocolEvent::DmReceived(msg) => {
                println!("DM: {:?}", String::from_utf8_lossy(&msg.payload));
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
├── data/           # SQLCipher-encrypted storage + blob storage
├── handlers/       # Incoming/outgoing message handlers
├── network/
│   ├── dht/        # Kademlia DHT implementation
│   ├── harbor/     # Harbor Node protocol (store, pull, harbor sync)
│   ├── membership/ # Topic join/leave messages
│   ├── send/       # Message sending (includes CRDT sync messages)
│   ├── share/      # P2P file sharing protocol
│   ├── stream/     # Real-time streaming transport
│   └── sync/       # CRDT sync protocol (direct peer-to-peer, for initial sync)
├── protocol/       # Protocol struct and public API
├── resilience/     # Rate limiting, PoW, storage limits
├── security/       # Cryptographic operations
└── tasks/          # Background tasks (harbor pull, maintenance)
```

## Core Concepts

### Topics

Group communication is organized around **topics**. A topic:
- Has a 32-byte TopicID
- Maintains a list of member EndpointIDs
- Derives encryption keys from the TopicID

```rust
// Create a topic (you become the first member)
let invite = protocol.create_topic().await?;

// Share the invite with others
let invite_string = invite.to_hex()?;

// Join using an invite
let invite = TopicInvite::from_hex(&invite_string)?;
protocol.join_topic(invite).await?;
```

### Direct Messages

Peer-to-peer communication without topics. DMs support messaging, sync, file sharing, and streaming — the same capabilities as topics but between two peers directly.

```rust
protocol.send(Target::Dm(peer_id), b"Hello!").await?;
```

### Send Protocol

Messages are delivered with best-effort semantics:

1. Encrypt and sign the packet
2. Send directly to all online recipients
3. Collect read receipts
4. Replicate to Harbor Nodes if any receipts are missing

```rust
// Send to all topic members (max 512 KB)
protocol.send(Target::Topic(topic_id), payload).await?;
```

### Share Protocol (File Sharing)

For files ≥512KB, use the Share protocol for efficient P2P distribution:

```rust
// Share a file with topic members
let hash = protocol.share_file(Target::Topic(topic_id), "/path/to/video.mp4").await?;

// Share a file via DM
let hash = protocol.share_file(Target::Dm(peer_id), "/path/to/file.pdf").await?;

// Check share status
let status = protocol.get_share_status(&hash).await?;
println!("Progress: {}/{} chunks", status.chunks_complete, status.total_chunks);

// Export a completed file
protocol.export_blob(&hash, "/path/to/output.mp4").await?;
```

**How it works:**
1. File is split into 512KB chunks, hashed with BLAKE3
2. `FileAnnouncement` is broadcast to recipients via Send protocol
3. Recipients pull chunks directly from source and other seeders
4. When a peer completes download, it broadcasts `CanSeed`
5. Chunks spread across the swarm (BitTorrent-like distribution)

**Events emitted:**
- `FileAnnounced` - A file was shared
- `FileProgress` - Download progress update
- `FileComplete` - All chunks received

### Sync Primitives (CRDT Support)

Three primitives for building collaborative applications with CRDTs. All methods accept `Target::Topic(id)` or `Target::Dm(id)`:

```rust
// Send a CRDT update (delta) to all topic members
protocol.send_sync_update(Target::Topic(topic_id), update_bytes).await?;

// Request full state from all topic members
protocol.request_sync(Target::Topic(topic_id)).await?;

// Respond with full state to a specific peer (direct connection, no size limit)
protocol.respond_sync(Target::Topic(topic_id), &requester_id, snapshot_bytes).await?;

// DM equivalents
protocol.send_sync_update(Target::Dm(peer_id), update_bytes).await?;
protocol.request_sync(Target::Dm(peer_id)).await?;
protocol.respond_sync(Target::Dm(peer_id), &peer_id, snapshot_bytes).await?;
```

**How it works:**
1. **SyncUpdate**: Broadcasts CRDT deltas via Send protocol (max 512KB), replicated through Harbor Nodes
2. **SyncRequest**: Broadcasts a request for full state to all members
3. **SyncResponse**: Direct peer-to-peer transfer via dedicated SYNC_ALPN protocol (no size limit)

**Events emitted:**
- `SyncUpdate` / `DmSyncUpdate` - A CRDT update was received
- `SyncRequest` / `DmSyncRequest` - A peer is requesting full state
- `SyncResponse` / `DmSyncResponse` - Full state snapshot received

**Example use case:** The test app uses these primitives with [Loro CRDT](https://loro.dev) for real-time collaborative text editing.

### Stream Protocol

Transport layer for real-time streaming between peers. Applications handle the media encoding/decoding — Harbor provides the signaling and QUIC transport.

```rust
// Request a stream within a topic
let request_id = protocol.request_stream(&topic_id, &peer_id, "audio", catalog).await?;

// DM stream (no topic)
let request_id = protocol.dm_stream_request(&peer_id, "audio").await?;

// Accept/reject incoming requests
protocol.accept_stream(&request_id).await?;
protocol.reject_stream(&request_id, Some("busy".into())).await?;

// After StreamConnected event, publish or consume media
let producer = protocol.publish_to_stream(&request_id, "audio").await?;
let consumer = protocol.consume_stream(&request_id, "audio").await?;

// End a stream
protocol.end_stream(&request_id).await?;
```

**Events emitted:**
- `StreamRequest` - A peer wants to start a stream
- `StreamAccepted` - Stream request was accepted
- `StreamRejected` - Stream request was rejected
- `StreamConnected` - QUIC session established, ready for media
- `StreamEnded` - Stream was terminated

### Harbor Nodes

When recipients are offline, packets are stored on Harbor Nodes:

- Any node can be a Harbor Node
- Nodes responsible for a HarborID (hash of TopicID) store packets
- Offline peers pull missed packets when they reconnect
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

    ..Default::default()
};

// Or for testing (shorter intervals, no PoW)
let config = ProtocolConfig::for_testing();
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

1. **Confidentiality**: AEAD encryption
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

# Debug streaming
RUST_LOG=harbor_core::network::stream=debug cargo run -p harbor-core

# Multiple modules
RUST_LOG=info,harbor_core::network=debug cargo run -p harbor-core
```
