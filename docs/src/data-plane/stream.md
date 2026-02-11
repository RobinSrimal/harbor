# Stream (Live Media)

Stream is for **low-latency, continuous data** like audio, video, or realtime telemetry.

A stream is negotiated first, then one side publishes and the other consumes.

## Topics

Request a stream with a peer inside a topic:

```rust
let request_id = protocol.request_stream(
    &topic_id,
    &peer_id,
    "camera",
    Vec::new(), // optional catalog/metadata
).await?;
```

Accept or reject on the other side:

```rust
protocol.accept_stream(&request_id).await?;
// or
protocol.reject_stream(&request_id, Some("Busy".into())).await?;
```

Publish or consume once connected:

```rust
let producer = protocol.publish_to_stream(&request_id, "camera").await?;
let consumer = protocol.consume_stream(&request_id, "camera").await?;
```

## Direct Messages

```rust
let request_id = protocol.dm_stream_request(&peer_id, "audio").await?;
```

The rest of the flow is the same: accept, then publish/consume.

## Events

```rust
match event {
    ProtocolEvent::StreamRequest { request_id, .. } => { /* prompt user */ }
    ProtocolEvent::StreamConnected { request_id, .. } => { /* ready */ }
    _ => {}
}
```

## When to Use Stream

Use Stream for real-time or continuous data. For files, use [Share (Files)](./share.md). For message-style data, use [Send (Messages)](./send.md).
