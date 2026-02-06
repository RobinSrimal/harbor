# Quick Start

Get started building with Harbor in minutes.

## Add Dependency

```toml
[dependencies]
harbor-core = { git = "https://github.com/RobinSrimal/harbor", branch = "main" }
tokio = { version = "1", features = ["full"] }
```

## Minimal Example

```rust
use harbor_core::{Protocol, ProtocolConfig, ProtocolEvent, Target};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Database encryption key (32 bytes)
    let db_key: [u8; 32] = [0u8; 32]; // Use a real key in production!

    // Start the protocol
    let config = ProtocolConfig::default()
        .with_db_key(db_key)
        .with_db_path("myapp.db".into());

    let protocol = Protocol::start(config).await?;

    // Print your identity
    println!("My EndpointID: {}", hex::encode(protocol.endpoint_id()));

    // Create a topic
    let invite = protocol.create_topic().await?;
    println!("Topic created: {}", hex::encode(&invite.topic_id));
    println!("Invite: {}", invite.to_hex()?);

    // Send a message to the topic
    protocol.send(Target::Topic(invite.topic_id), b"Hello, Harbor!").await?;

    // Listen for events
    let mut events = protocol.events().await?;
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ProtocolEvent::Message(msg) => {
                    println!("Received: {}", String::from_utf8_lossy(&msg.payload));
                }
                _ => {}
            }
        }
    });

    // Keep running...
    tokio::signal::ctrl_c().await?;

    protocol.stop().await;
    Ok(())
}
```

## Key Concepts

1. **Protocol** - The main entry point. Start it, use it, stop it.
2. **ProtocolConfig** - Configure database path, encryption key, bootstrap nodes, etc.
3. **Target** - Where to send: `Target::Topic(id)` or `Target::Dm(peer_id)`
4. **ProtocolEvent** - Events you receive: messages, file announcements, connection requests, etc.

## What's Next?

- [Configuration](./configuration.md) - All configuration options
- [Topics & Membership](../concepts/topics.md) - Group communication
- [Direct Messages](../concepts/direct-messages.md) - 1:1 messaging
- [Simple Messaging App](../tutorials/messaging-app.md) - Complete example
- [API Reference](../tutorials/api-reference.md) - Full API documentation
