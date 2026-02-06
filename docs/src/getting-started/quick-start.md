# Quick Start

Get Harbor running in minutes.

## Prerequisites

- Rust toolchain (1.75+)
- A 32-byte database encryption key

## Installation

```bash
# Clone the repository
git clone https://github.com/RobinSrimal/harbor
cd harbor/core

# Build
cargo build --release
```

## Running Your First Node

```bash
# Generate a database key
export HARBOR_DB_KEY=$(openssl rand -hex 32)

# Start a node with the HTTP API
./target/release/harbor --serve --api-port 8080
```

You'll see output like:
```
Harbor Protocol Node v0.1.0

Starting Harbor Protocol...
Database: harbor.db
Blob storage: .harbor_blobs

=== Local Node Identity ===
EndpointID: a1b2c3d4...

Waiting for relay connection...
Relay: https://euc1-1.relay.iroh.link./

🚀 Harbor Node running
📡 API: http://127.0.0.1:8080
```

## Create a Topic

```bash
# Create a new topic
curl -X POST http://localhost:8080/api/topic
```

Response:
```json
{
  "topic_id": "abc123...",
  "invite": "746f7069..."
}
```

## Send a Message

```bash
# Send a message to the topic
curl -X POST http://localhost:8080/api/send \
  -H "Content-Type: application/json" \
  -d '{"topic": "abc123...", "message": "Hello, Harbor!"}'
```

## Join from Another Node

On a second machine (or terminal with different ports/paths):

```bash
export HARBOR_DB_KEY=$(openssl rand -hex 32)
./target/release/harbor --serve --api-port 8081 --db-path node2.db

# Join using the invite from node 1
curl -X POST http://localhost:8081/api/join \
  -H "Content-Type: application/json" \
  -d '{"invite": "746f7069..."}'
```

## What's Next?

- [Running a Node](./running-node.md) - Detailed node configuration
- [Topics & Membership](../concepts/topics.md) - Understanding topics
- [Simple Messaging App](../tutorials/messaging-app.md) - Build a chat app
