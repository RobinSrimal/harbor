# Share (Files)

Share is for **large files** and payloads **above 512 KB**. It announces a file and lets peers pull chunks on demand.

## Topics

Announce a file to a topic:

```rust
let hash = protocol.share_file(
    Target::Topic(topic_id),
    "/path/to/video.mp4"
).await?;
```

Peers can download it:

```rust
protocol.request_blob(&hash).await?;
```

Track progress and export when done:

```rust
let status = protocol.get_share_status(&hash).await?;
protocol.export_blob(&hash, "/path/to/output.mp4").await?;
```

## Direct Messages

Use the same API with `Target::Dm(peer_id)`:

```rust
let hash = protocol.share_file(Target::Dm(peer_id), "/path/to/file").await?;
protocol.request_blob(&hash).await?;
```

## Events

```rust
match event {
    ProtocolEvent::FileAnnounced(file) => { /* decide to download */ }
    ProtocolEvent::FileComplete(hash) => { /* export */ }
    _ => {}
}
```

## When to Use Share

Use Share for large files or any data above 512 KB. For smaller payloads, use [Send (Messages)](./send.md).
