# Simple Messaging App

Build a basic 1:1 messaging app using Harbor's Rust API.

## Overview

We'll create a terminal chat app where two peers can message each other directly, with offline delivery support.

## Dependencies

```toml
[dependencies]
harbor-core = { path = "../core" }
tokio = { version = "1", features = ["full"] }
hex = "0.4"
```

## The Code

```rust
use harbor_core::{Protocol, ProtocolConfig, ProtocolEvent, Target};
use std::io::{self, BufRead, Write};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get database key from environment
    let db_key = get_db_key()?;

    // Parse command-line args
    let args: Vec<String> = std::env::args().collect();
    let db_path = args.get(1).map(|s| s.as_str()).unwrap_or("chat.db");

    // Start the protocol
    let config = ProtocolConfig::default()
        .with_db_key(db_key)
        .with_db_path(db_path.into());

    let protocol = Protocol::start(config).await?;

    // Print our identity
    let my_id = protocol.endpoint_id();
    println!("Your ID: {}", hex::encode(&my_id));

    // Wait for relay
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    if let Some(relay) = protocol.relay_url().await {
        println!("Relay: {}", relay);
    }

    println!("\nCommands:");
    println!("  /invite           - Generate a connect invite");
    println!("  /connect <invite> - Connect using an invite");
    println!("  /chat <peer_id>   - Start chatting with a peer");
    println!("  /quit             - Exit");
    println!();

    // Channel for user input
    let (tx, mut rx) = mpsc::channel::<String>(100);

    // Spawn input reader
    let tx_clone = tx.clone();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            if let Ok(line) = line {
                let _ = tx_clone.blocking_send(line);
            }
        }
    });

    // Spawn event listener
    let proto_events = protocol.clone();
    tokio::spawn(async move {
        let mut events = proto_events.events().await.unwrap();
        while let Some(event) = events.recv().await {
            match event {
                ProtocolEvent::DmReceived(msg) => {
                    let from = hex::encode(&msg.sender);
                    let text = String::from_utf8_lossy(&msg.payload);
                    println!("\n[{}]: {}", &from[..8], text);
                    print!("> ");
                    io::stdout().flush().ok();
                }
                ProtocolEvent::ConnectRequest { peer_id, request_id, display_name, .. } => {
                    let name = display_name.unwrap_or_else(|| hex::encode(&peer_id[..4]));
                    println!("\n📨 Connection request from: {}", name);
                    // Auto-accept for this demo
                    let _ = proto_events.accept_connection(&request_id).await;
                    println!("✅ Accepted connection");
                    print!("> ");
                    io::stdout().flush().ok();
                }
                ProtocolEvent::ConnectAccepted { peer_id, .. } => {
                    println!("\n✅ Connected to: {}", hex::encode(&peer_id[..8]));
                    print!("> ");
                    io::stdout().flush().ok();
                }
                _ => {}
            }
        }
    });

    // Current chat peer
    let mut chat_peer: Option<[u8; 32]> = None;

    // Main loop
    loop {
        print!("> ");
        io::stdout().flush()?;

        let input = match rx.recv().await {
            Some(line) => line,
            None => break,
        };

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if input.starts_with('/') {
            let parts: Vec<&str> = input.splitn(2, ' ').collect();
            let cmd = parts[0];
            let arg = parts.get(1).map(|s| *s);

            match cmd {
                "/quit" => break,

                "/invite" => {
                    let invite = protocol.generate_connect_invite().await?;
                    println!("Share this invite:\n{}", invite.encode());
                }

                "/connect" => {
                    if let Some(invite_str) = arg {
                        match protocol.connect_with_invite(invite_str).await {
                            Ok(_) => println!("Connection request sent..."),
                            Err(e) => println!("Error: {}", e),
                        }
                    } else {
                        println!("Usage: /connect <invite>");
                    }
                }

                "/chat" => {
                    if let Some(peer_hex) = arg {
                        match hex::decode(peer_hex) {
                            Ok(bytes) if bytes.len() == 32 => {
                                let mut peer = [0u8; 32];
                                peer.copy_from_slice(&bytes);
                                chat_peer = Some(peer);
                                println!("Now chatting with: {}", &peer_hex[..8]);
                            }
                            _ => println!("Invalid peer ID (expected 64 hex chars)"),
                        }
                    } else {
                        println!("Usage: /chat <peer_id>");
                    }
                }

                _ => println!("Unknown command: {}", cmd),
            }
        } else {
            // Send message to current chat peer
            if let Some(peer) = chat_peer {
                protocol.send(Target::Dm(peer), input.as_bytes()).await?;
            } else {
                println!("No peer selected. Use /chat <peer_id> first.");
            }
        }
    }

    println!("Shutting down...");
    protocol.stop().await;
    Ok(())
}

fn get_db_key() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let hex_str = std::env::var("HARBOR_DB_KEY")
        .map_err(|_| "Set HARBOR_DB_KEY environment variable (64 hex chars)")?;

    let bytes = hex::decode(&hex_str)?;
    if bytes.len() != 32 {
        return Err("HARBOR_DB_KEY must be 64 hex characters".into());
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}
```

## Running the App

### Terminal 1 (Alice)

```bash
export HARBOR_DB_KEY=$(openssl rand -hex 32)
cargo run -- alice.db
```

### Terminal 2 (Bob)

```bash
export HARBOR_DB_KEY=$(openssl rand -hex 32)
cargo run -- bob.db
```

## Usage

### 1. Generate an Invite (Bob)

```
> /invite
Share this invite:
Y29ubmVjdC4uLg==
```

### 2. Connect (Alice)

```
> /connect Y29ubmVjdC4uLg==
Connection request sent...
✅ Connected to: e5f6g7h8
```

### 3. Start Chatting

Alice:
```
> /chat e5f6g7h8e5f6g7h8e5f6g7h8e5f6g7h8e5f6g7h8e5f6g7h8e5f6g7h8e5f6g7h8
Now chatting with: e5f6g7h8
> Hey Bob!
```

Bob:
```
[a1b2c3d4]: Hey Bob!
> /chat a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4
Now chatting with: a1b2c3d4
> Hi Alice! How are you?
```

## Offline Delivery

Try this:
1. Stop Bob's app (Ctrl+C)
2. Alice sends a message
3. Restart Bob's app
4. Bob receives the message (pulled from Harbor Nodes)

This works because Harbor automatically stores undelivered messages on Harbor Nodes.

## Key Concepts Demonstrated

1. **Protocol initialization** with `Protocol::start(config)`
2. **Event handling** via `protocol.events().await`
3. **Connection flow** with invite generation and acceptance
4. **Direct messaging** using `Target::Dm(peer_id)`
5. **Offline delivery** via Harbor Nodes (transparent)

## Next Steps

For a production app, you'd add:

- **UI**: Tauri, Electron, or native mobile
- **Message persistence**: Store chat history locally
- **Contact list**: Track connected peers with display names
- **Group chat**: Use topics instead of DMs
- **File sharing**: Add `share_file()` support
- **Read receipts**: Track `ProtocolEvent::Receipt` events
