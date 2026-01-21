# Harbor Protocol - Context for Claude

This file provides important context about Harbor's architecture for AI assistants working on this codebase.

## Overview

Harbor is a **CRDT-agnostic transport protocol** for distributed, offline-first collaboration. The protocol doesn't implement CRDT logic internally - that's the application layer's responsibility. Harbor provides three transport primitives:

1. **Sync Updates (deltas)** → `send_sync_update(topic, data: Vec<u8>)`
   - Broadcasts CRDT delta bytes to all topic members
   - Uses Send protocol (max 512KB)
   - Goes through Harbor nodes for offline delivery

2. **Sync Requests** → `request_sync(topic)`
   - Broadcasts request for full state to all topic members
   - Uses Send protocol
   - Triggers `SyncRequest` events on all members

3. **Sync Responses (full state)** → `respond_sync(topic, requester_id, data: Vec<u8>)`
   - Direct point-to-point connection via **SYNC_ALPN**
   - NO size limit (can be hundreds of MB)
   - Uses dedicated QUIC stream for large snapshots

**Key Design Decision**: Sync responses use a separate ALPN (`SYNC_ALPN`) to make it very clear they flow directly peer-to-peer, not through the broadcast/harbor system.

## Protocol Flow

```
Node A: Has CRDT state, makes edit
  ↓
Node A: Serializes CRDT delta → bytes
  ↓
Node A: send_sync_update(topic, delta_bytes)
  ↓ (broadcast via Send protocol)
All members: Receive SyncUpdate event with delta_bytes
  ↓
Each node: Deserializes and applies delta to their CRDT

---

Node B: Joins topic, needs full state
  ↓
Node B: request_sync(topic)
  ↓ (broadcast via Send protocol)
All members: Receive SyncRequest event
  ↓
Node A: Serializes full CRDT state → bytes
  ↓
Node A: respond_sync(topic, node_b_id, snapshot_bytes)
  ↓ (direct SYNC_ALPN connection)
Node B: Receives SyncResponse event with snapshot_bytes
  ↓
Node B: Deserializes and initializes CRDT state
```

## Protocol Architecture

### Three Main Protocol Layers

1. **DHT Layer** (`network/dht/`)
   - Kademlia-based distributed hash table
   - Peer discovery and routing
   - Bootstrap nodes for joining network

2. **Harbor Layer** (`network/harbor/`)
   - Store-and-forward for offline delivery
   - Nodes store packets for offline peers
   - Pull mechanism to retrieve missed messages
   - Sync functionality (batched packet sync between harbor nodes)

3. **Membership Layer** (`network/membership/`)
   - Topic-based pub/sub
   - Join/Leave announcements
   - Member verification and encryption
   - Message types: Content, Join, Leave, FileAnnouncement, CanSeed, SyncUpdate, SyncRequest

### ALPN Protocols

Harbor uses multiple ALPN identifiers for different protocols:

- `harbor/dht/0` - DHT operations (peer discovery, routing)
- `harbor/harbor/0` - Harbor protocol (store packets, pull packets, harbor sync)
- `harbor/send/0` - Direct send (receipts, direct messages)
- `harbor/share/0` - File sharing (chunk requests, bitfields)
- `harbor/sync/0` - **Sync responses ONLY** (direct large snapshots)

**Important**: `SYNC_ALPN` is only used for sync responses (full state snapshots). Sync updates and requests go through Send protocol.

### Message Flow

#### Regular Messages
1. Node creates topic, gets invite
2. Members join via invite (includes topic ID + encryption keys)
3. Node calls `send(topic, bytes)` → creates signed packet
4. Packet sent to online members directly via Send protocol
5. Packet replicated to Harbor nodes for offline members
6. Offline members pull packets when they come online

#### Sync Messages
1. **Updates (deltas)**: `send_sync_update()` → Broadcast via Send → Harbor replication
2. **Requests**: `request_sync()` → Broadcast via Send → Harbor replication
3. **Responses**: `respond_sync()` → Direct SYNC_ALPN connection (no Harbor)

### Key Files

- `core/src/protocol/core.rs` - Main Protocol struct, public API methods
- `core/src/handlers/incoming/` - Protocol handlers for different ALPNs
  - `send.rs` - Handles Send protocol (messages, sync updates/requests)
  - `sync.rs` - Handles SYNC_ALPN (sync responses only)
  - `harbor.rs` - Handles Harbor protocol (store/pull/sync)
- `core/src/network/membership/messages.rs` - TopicMessage enum (sent over Send protocol)
- `core/src/tasks/harbor_pull.rs` - Background task to pull packets from Harbor nodes

### TopicMessage Types

The `TopicMessage` enum defines message types sent via Send protocol:

```rust
pub enum TopicMessage {
    Content(Vec<u8>),              // Regular messages
    Join(JoinMessage),             // Join announcements
    Leave(LeaveMessage),           // Leave announcements
    FileAnnouncement(...),         // File sharing
    CanSeed(...),                  // File sharing
    SyncUpdate(SyncUpdateMessage), // CRDT delta bytes
    SyncRequest,                   // Request for full state
}
```

**Note**: There is NO `SyncResponse` in this enum! Sync responses are sent via direct SYNC_ALPN connection, not through TopicMessage/Send protocol.

## Testing & Simulations

See [SIMULATIONS.md](SIMULATIONS.md) for details on running simulations and tests.

## Common Gotchas

1. **Don't confuse Send protocol with SYNC_ALPN**: Sync updates/requests use Send, sync responses use SYNC_ALPN
2. **TopicMessage is only for Send protocol**: Direct SYNC_ALPN uses raw format `[topic_id][type_byte][data]`
3. **Harbor sync vs CRDT sync**: "Harbor sync" = syncing packets between harbor nodes; "CRDT sync" = application-level state synchronization
4. **Size limits**: Send protocol has 512KB limit; SYNC_ALPN has no limit (designed for large snapshots)
5. **The protocol is CRDT-agnostic**: Applications choose their own CRDT library (Loro, Yjs, Automerge, etc.)

## Development Notes

- The codebase uses async Rust with Tokio
- QUIC transport via `iroh` library
- SQLite for local persistence (identity, topics, harbor packets, etc.)
- Simulations use bash scripts + curl to test the HTTP API
- Log parsing is used for simulation verification (grep for event types)

## Key Design Principles

1. **Application owns CRDT state** - Protocol only transports bytes
2. **Separate protocol for large data** - SYNC_ALPN for unlimited size responses
3. **Offline-first** - Harbor layer stores packets for offline peers
4. **Topic-based** - All communication scoped to topics with invite-based membership
5. **End-to-end encrypted** - Topics use shared keys, packets are signed
