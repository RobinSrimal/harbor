# API Reference

The Rust API for building applications with Harbor.

## Getting Started

```rust
use harbor_core::{Protocol, ProtocolConfig, ProtocolEvent, Target};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ProtocolConfig::default()
        .with_db_key(my_32_byte_key)
        .with_db_path("app.db".into());

    let protocol = Protocol::start(config).await?;

    // Your app logic here...

    protocol.stop().await;
    Ok(())
}
```

## Topics

### Create Topic

```rust
pub async fn create_topic(&self) -> Result<TopicInvite, ProtocolError>
```

Creates a new topic with you as the only member. Returns an invite to share.

```rust
let invite = protocol.create_topic().await?;
println!("Share this invite: {}", invite.to_hex()?);
```

### Join Topic

```rust
pub async fn join_topic(&self, invite: TopicInvite) -> Result<(), ProtocolError>
```

Joins a topic using an invite. Announces your presence to other members.

```rust
let invite = TopicInvite::from_hex(&invite_string)?;
protocol.join_topic(invite).await?;
```

### Leave Topic

```rust
pub async fn leave_topic(&self, topic_id: &[u8; 32]) -> Result<(), ProtocolError>
```

### List Topics

```rust
pub async fn list_topics(&self) -> Result<Vec<[u8; 32]>, ProtocolError>
```

### Get Invite

```rust
pub async fn get_invite(&self, topic_id: &[u8; 32]) -> Result<TopicInvite, ProtocolError>
```

Returns a fresh invite with all current members.

## Messaging

### Send Message

```rust
pub async fn send(&self, target: Target, payload: &[u8]) -> Result<(), ProtocolError>
```

Send a message (max 512 KB) to a topic or DM.

```rust
// Send to topic
protocol.send(Target::Topic(topic_id), b"Hello topic!").await?;

// Send DM
protocol.send(Target::Dm(peer_id), b"Hello directly!").await?;
```

## File Sharing

### Share File

```rust
pub async fn share_file(
    &self,
    target: Target,
    file_path: impl AsRef<Path>,
) -> Result<[u8; 32], ProtocolError>
```

Share a file (≥512 KB). Returns the BLAKE3 hash.

```rust
let hash = protocol.share_file(Target::Topic(topic_id), "/path/to/video.mp4").await?;
```

### Get Share Status

```rust
pub async fn get_share_status(&self, hash: &[u8; 32]) -> Result<ShareStatus, ProtocolError>
```

```rust
let status = protocol.get_share_status(&hash).await?;
println!("Progress: {}/{}", status.chunks_complete, status.total_chunks);
```

### Request Blob Download

```rust
pub async fn request_blob(&self, hash: &[u8; 32]) -> Result<(), ProtocolError>
```

Start downloading a file announced by another peer.

### Export Blob

```rust
pub async fn export_blob(
    &self,
    hash: &[u8; 32],
    dest_path: impl AsRef<Path>,
) -> Result<(), ProtocolError>
```

Export a completed download to a file.

### List Topic Blobs

```rust
pub async fn list_topic_blobs(&self, topic_id: &[u8; 32]) -> Result<Vec<BlobMetadata>, ProtocolError>
```

## Sync (CRDT Support)

Harbor provides transport primitives for CRDTs. You bring your own CRDT library (Loro, Yjs, Automerge, etc.).

### Send Sync Update

```rust
pub async fn send_sync_update(&self, target: Target, data: Vec<u8>) -> Result<(), ProtocolError>
```

Broadcast a CRDT delta (max 512 KB). Supports Harbor offline storage.

```rust
let delta = my_crdt.export_updates();
protocol.send_sync_update(Target::Topic(topic_id), delta).await?;
```

### Request Sync

```rust
pub async fn request_sync(&self, target: Target) -> Result<(), ProtocolError>
```

Request full state from peers. They respond via `respond_sync()`.

```rust
protocol.request_sync(Target::Topic(topic_id)).await?;
// Wait for SyncResponse events...
```

### Respond to Sync Request

```rust
pub async fn respond_sync(
    &self,
    target: Target,
    requester_id: &[u8; 32],
    data: Vec<u8>,
) -> Result<(), ProtocolError>
```

Send full state snapshot. Uses direct connection - **no size limit**.

```rust
// On receiving SyncRequest event:
let snapshot = my_crdt.export_snapshot();
protocol.respond_sync(Target::Topic(topic_id), &requester_id, snapshot).await?;
```

## Peer Connections (Control)

### Generate Connect Invite

```rust
pub async fn generate_connect_invite(&self) -> Result<ConnectInvite, ProtocolError>
```

Generate an invite for QR codes or sharing.

```rust
let invite = protocol.generate_connect_invite().await?;
let qr_string = invite.encode();  // Share this
```

### Connect with Invite

```rust
pub async fn connect_with_invite(&self, invite: &str) -> Result<[u8; 32], ProtocolError>
```

Connect using an invite string.

### Request Connection

```rust
pub async fn request_connection(
    &self,
    peer_id: &[u8; 32],
    relay_url: Option<&str>,
    display_name: Option<&str>,
    token: Option<[u8; 32]>,
) -> Result<[u8; 32], ProtocolError>
```

### Accept/Decline Connection

```rust
pub async fn accept_connection(&self, request_id: &[u8; 32]) -> Result<(), ProtocolError>
pub async fn decline_connection(&self, request_id: &[u8; 32], reason: Option<&str>) -> Result<(), ProtocolError>
```

### Block/Unblock Peer

```rust
pub async fn block_peer(&self, peer_id: &[u8; 32]) -> Result<(), ProtocolError>
pub async fn unblock_peer(&self, peer_id: &[u8; 32]) -> Result<(), ProtocolError>
```

### List Connections

```rust
pub async fn list_connections(&self) -> Result<Vec<ConnectionInfo>, ProtocolError>
```

## Topic Invites

### Invite Peer to Topic

```rust
pub async fn invite_to_topic(
    &self,
    peer_id: &[u8; 32],
    topic_id: &[u8; 32],
) -> Result<[u8; 32], ProtocolError>
```

Invite a connected peer to a topic.

### Accept/Decline Topic Invite

```rust
pub async fn accept_topic_invite(&self, message_id: &[u8; 32]) -> Result<(), ProtocolError>
pub async fn decline_topic_invite(&self, message_id: &[u8; 32]) -> Result<(), ProtocolError>
```

### Remove Member

```rust
pub async fn remove_topic_member(
    &self,
    topic_id: &[u8; 32],
    member_id: &[u8; 32],
) -> Result<(), ProtocolError>
```

Remove a member and rotate epoch key.

### Suggest Peer

```rust
pub async fn suggest_peer(
    &self,
    to_peer: &[u8; 32],
    suggested_peer: &[u8; 32],
    note: Option<&str>,
) -> Result<[u8; 32], ProtocolError>
```

Introduce two peers to each other.

## Streaming

### Request Stream

```rust
pub async fn request_stream(
    &self,
    topic_id: &[u8; 32],
    peer_id: &[u8; 32],
    name: &str,
    catalog: Vec<u8>,
) -> Result<[u8; 32], ProtocolError>

pub async fn dm_stream_request(
    &self,
    peer_id: &[u8; 32],
    name: &str,
) -> Result<[u8; 32], ProtocolError>
```

### Accept/Reject Stream

```rust
pub async fn accept_stream(&self, request_id: &[u8; 32]) -> Result<(), ProtocolError>
pub async fn reject_stream(&self, request_id: &[u8; 32], reason: Option<String>) -> Result<(), ProtocolError>
```

### Publish/Consume

```rust
pub async fn publish_to_stream(
    &self,
    request_id: &[u8; 32],
    broadcast_name: &str,
) -> Result<BroadcastProducer, ProtocolError>

pub async fn consume_stream(
    &self,
    request_id: &[u8; 32],
    broadcast_name: &str,
) -> Result<BroadcastConsumer, ProtocolError>
```

### End Stream

```rust
pub async fn end_stream(&self, request_id: &[u8; 32]) -> Result<(), ProtocolError>
```

## Events

Subscribe to protocol events:

```rust
let mut events = protocol.events().await?;
while let Some(event) = events.recv().await {
    match event {
        ProtocolEvent::Message(msg) => { /* topic message */ }
        ProtocolEvent::DmReceived(msg) => { /* direct message */ }
        ProtocolEvent::FileAnnounced(file) => { /* file shared */ }
        ProtocolEvent::FileComplete(hash) => { /* download done */ }
        ProtocolEvent::SyncUpdate { topic_id, sender, data } => { /* CRDT delta */ }
        ProtocolEvent::SyncRequest { topic_id, requester } => { /* state requested */ }
        ProtocolEvent::SyncResponse { topic_id, sender, data } => { /* full state */ }
        ProtocolEvent::ConnectRequest { peer_id, request_id, .. } => { /* connection request */ }
        ProtocolEvent::TopicInviteReceived { .. } => { /* invited to topic */ }
        ProtocolEvent::StreamRequest { request_id, .. } => { /* stream incoming */ }
        ProtocolEvent::StreamConnected { request_id, is_source } => { /* ready for media */ }
        _ => {}
    }
}
```

## Stats

```rust
pub async fn get_stats(&self) -> Result<ProtocolStats, ProtocolError>
pub async fn get_dht_buckets(&self) -> Result<Vec<DhtBucketInfo>, ProtocolError>
pub async fn get_topic_details(&self, topic_id: &[u8; 32]) -> Result<TopicDetails, ProtocolError>
```

## Node Identity

```rust
// Get your EndpointID (public key)
let my_id: [u8; 32] = protocol.endpoint_id();

// Get relay URL (for NAT traversal)
let relay: Option<String> = protocol.relay_url().await;
```
