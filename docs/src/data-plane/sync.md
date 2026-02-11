# Sync (CRDT)

Sync provides **CRDT transport primitives** so you can build collaborative apps. Harbor carries small deltas; large snapshots go direct peer-to-peer.

## Topics

Send a delta update:

```rust
protocol.send_sync_update(Target::Topic(topic_id), delta_bytes).await?;
```

Request a full state:

```rust
protocol.request_sync(Target::Topic(topic_id)).await?;
```

Respond with a snapshot when you receive a request:

```rust
protocol.respond_sync(Target::Topic(topic_id), &requester_id, snapshot_bytes).await?;
```

## Direct Messages

The same APIs work with `Target::Dm(peer_id)`:

```rust
protocol.send_sync_update(Target::Dm(peer_id), delta_bytes).await?;
protocol.request_sync(Target::Dm(peer_id)).await?;
protocol.respond_sync(Target::Dm(peer_id), &peer_id, snapshot_bytes).await?;
```

## Events

```rust
match event {
    ProtocolEvent::SyncUpdate { topic_id, sender, data } => { /* apply delta */ }
    ProtocolEvent::SyncRequest { topic_id, requester } => { /* send snapshot */ }
    ProtocolEvent::SyncResponse { topic_id, sender, data } => { /* import snapshot */ }
    _ => {}
}
```

## When to Use Sync

Use Sync when you have CRDT data and need fast convergence. For chat-style messages, use [Send (Messages)](./send.md).
