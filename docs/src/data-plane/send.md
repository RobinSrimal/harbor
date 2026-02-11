# Send (Messages)

Send is the **default messaging path**. It's ideal for chat messages, small payloads, and quick updates **up to 512 KB**. Harbor handles offline delivery automatically.

## Topics

```rust
protocol.send(Target::Topic(topic_id), b"Hello topic!").await?;
```

## Direct Messages

```rust
protocol.send(Target::Dm(peer_id), b"Hello directly!").await?;
```

## Events

```rust
match event {
    ProtocolEvent::DmReceived(msg) => {
        // msg.sender, msg.payload
    }
    ProtocolEvent::Message(msg) => {
        // msg.topic_id, msg.sender, msg.payload
    }
    _ => {}
}
```

## When to Use Send

- Chat and small payloads
- Quick notifications and state pings
- Anything that doesn't need streaming or file transfer

If the payload is larger than 512 KB, use [Share (Files)](./share.md). For CRDT deltas or snapshots, use [Sync (CRDT)](./sync.md).
