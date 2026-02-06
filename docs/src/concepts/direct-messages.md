# Direct Messages

Direct messages (DMs) provide private 1:1 communication between two peers.

## Establishing a Connection

Before sending DMs, peers must establish a connection:

```rust
// Generate an invite token (for QR codes, links, etc.)
let token = protocol.generate_connect_token().await?;

// Request connection to a peer
protocol.request_connect(&peer_id, Some("Alice"), Some(token)).await?;
```

The other peer receives a `ConnectRequest` event:

```rust
// Accept the connection
protocol.accept_connect(&peer_id).await?;

// Or decline
protocol.decline_connect(&peer_id, Some("Not accepting new contacts")).await?;
```

## Sending DMs

Once connected:

```rust
protocol.send(Target::Dm(peer_id), b"Hello!").await?;
```

## DM Features

DMs support all the same features as topics:

### Messaging
```rust
protocol.send(Target::Dm(peer_id), payload).await?;
```

### File Sharing
```rust
protocol.share_file(Target::Dm(peer_id), "/path/to/file").await?;
```

### CRDT Sync
```rust
protocol.send_sync_update(Target::Dm(peer_id), update).await?;
protocol.request_sync(Target::Dm(peer_id)).await?;
```

### Streaming
```rust
protocol.dm_stream_request(&peer_id, "audio").await?;
```

## Offline Delivery

DMs also support offline delivery via Harbor Nodes:

1. You send a DM to an offline peer
2. The message is stored on Harbor Nodes using the recipient's EndpointID as the HarborID
3. When the peer comes online, they pull messages addressed to their EndpointID

This is simpler than topics - each peer just pulls packets stored under their own EndpointID.

## Blocking Peers

```rust
protocol.block_peer(&peer_id).await?;
```

Blocked peers:
- Cannot send you messages
- Cannot initiate connections
- Are rejected at the connection gate

## Peer Suggestions

Introduce two peers to each other:

```rust
protocol.suggest_peer(
    &target_peer,      // Who receives the suggestion
    &suggested_peer,   // Who you're suggesting
    Some("My friend Bob")
).await?;
```

The target receives a `PeerSuggested` event with the suggested peer's info.

## Connection States

Peer relationships have states:

| State | Description |
|-------|-------------|
| `None` | No relationship |
| `Pending` | Connection requested, awaiting response |
| `Connected` | Both peers accepted |
| `Blocked` | Peer is blocked |

```rust
let connections = protocol.list_connections().await?;
for conn in connections {
    println!("{}: {:?}", conn.peer_id, conn.state);
}
```
