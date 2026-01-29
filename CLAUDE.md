# Harbor Protocol - Context for Claude

This file provides important context about Harbor's architecture for AI assistants working on this codebase.

## Codebase Architecture

### Layer Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Protocol (protocol/)                      │
│         Thin API layer + startup/shutdown only               │
│      Delegates to services, exposes public methods           │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    Services (network/)                       │
│     Self-contained units that own connections & handlers     │
│                                                              │
│  ┌───────────┐ ┌─────────────┐ ┌───────────┐ ┌───────────┐  │
│  │SendService│ │HarborService│ │ShareService│ │ DhtService│  │
│  └───────────┘ └─────────────┘ └───────────┘ └───────────┘  │
│        │              │              │              │        │
│        └──────────────┼──────────────┼──────────────┘        │
│                       ▼              ▼                       │
│              Services call each other directly               │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   Connection (transport)                     │
│        Per-peer, per-ALPN transport primitive                │
│     Wraps QUIC, handles relay URLs, connection pooling       │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                      Data (data/)                            │
│              Persistence layer (SQLite + blobs)              │
└─────────────────────────────────────────────────────────────┘
```

### Protocol (`core/src/protocol/`)

**Role**: Thin API shell + startup/shutdown orchestration.

- `core.rs` - Protocol struct, `start()`, `stop()`, service initialization
- `send.rs`, `topics.rs`, `share.rs` - Public API methods that delegate to services
- `events.rs` - Event types emitted to application
- `config.rs`, `error.rs`, `types.rs` - Configuration and types

**Important**: Protocol does NOT coordinate between services. Services call each other directly when needed. Protocol just:
1. Creates and wires up services on startup
2. Exposes public API (delegates to services)
3. Handles shutdown

### Services (`core/src/network/`)

**Role**: Self-contained units that own their connections, context, and business logic.

Each service:
- Owns a connection pool for its ALPN
- Has access to shared context (db, event_tx, identity)
- Handles both incoming and outgoing operations
- Calls other services directly when needed

| Service | ALPN | Responsibility |
|---------|------|----------------|
| `SendService` | `harbor/send/0` | Message delivery, receipts |
| `HarborService` | `harbor/harbor/0` | Store-and-forward for offline peers |
| `ShareService` | `harbor/share/0` | File chunk distribution |
| `DhtService` | `harbor/dht/0` | Peer discovery, routing table |
| `SyncService` | `harbor/sync/0` | Large sync responses (point-to-point) |

**Service interaction example** (send message):
1. User calls `protocol.send(topic, data)`
2. Protocol delegates to `send_service.send(topic, data)`
3. SendService tries direct delivery via its connections
4. If peer offline, SendService calls `harbor_service.store(packet)`
5. No Protocol involvement after initial delegation

### Handlers (`core/src/handlers/`)

**Role**: Process incoming connections/messages for each protocol.

- `incoming/` - Handle incoming QUIC connections by ALPN
- `outgoing/` - Outgoing operations (currently on Protocol, moving to Services)

**Current state**: Handlers are `impl Protocol` methods.
**Target state**: Handlers belong to their respective Services.

### Tasks (`core/src/tasks/`)

**Role**: Background automation that runs periodically.

| Task | Purpose |
|------|---------|
| `harbor_pull.rs` | Pull missed packets from Harbor nodes |
| `harbor_replication.rs` | Replicate packets to Harbor nodes |
| `dht_persist.rs` | Periodically save DHT routing table |

Tasks are spawned by Protocol on startup and run independently. They use Services to perform their work.

### Data (`core/src/data/`)

**Role**: Persistence layer - SQLite database + blob storage.

- `schema.rs` - Database schema (tables for identity, topics, members, packets, etc.)
- `identity.rs` - Local identity (keypair) management
- `topics.rs` - Topic and membership persistence
- `send.rs` - Outgoing packet tracking
- `harbor.rs` - Harbor packet storage
- `dht.rs` - DHT routing table persistence
- `blobs.rs` - File chunk storage (filesystem)

**Key principle**: Data layer is accessed by Services, not directly by Protocol or handlers.

### Connection (transport primitive)

**Role**: Per-peer, per-ALPN transport abstraction over QUIC.

```rust
struct Connection {
    peer_id: NodeId,
    alpn: Alpn,
    relay_url: Option<RelayUrl>,
    // connection state, streams, etc.
}
```

- Wraps iroh/QUIC connection details
- Handles relay URLs for NAT traversal
- Managed by Services in connection pools
- Encryption is at QUIC level (transport) - packet encryption is only for Harbor storage

## Protocol Layers (Conceptual)

### ALPN Protocols

Harbor uses multiple ALPN identifiers for different protocols:

| ALPN | Service | Purpose |
|------|---------|---------|
| `harbor/send/0` | SendService | Message delivery, receipts |
| `harbor/harbor/0` | HarborService | Store/pull packets, harbor-to-harbor sync |
| `harbor/dht/0` | DhtService | Peer discovery, routing |
| `harbor/share/0` | ShareService | File chunk requests, bitfields |
| `harbor/sync/0` | SyncService | **Sync responses ONLY** (large snapshots) |

## Key Files

| Path | Purpose |
|------|---------|
| `core/src/protocol/core.rs` | Protocol struct, startup, public API |
| `core/src/network/send/` | SendService, Send protocol messages |
| `core/src/network/harbor/` | HarborService, store-and-forward |
| `core/src/network/dht/` | DhtService, Kademlia DHT |
| `core/src/network/share/` | ShareService, file distribution |
| `core/src/network/sync/` | SyncService, large sync responses |
| `core/src/handlers/incoming/` | Incoming connection handlers (per ALPN) |
| `core/src/handlers/outgoing/` | Outgoing operations |
| `core/src/tasks/` | Background tasks (pull, replication, persist) |
| `core/src/data/` | Persistence (SQLite schema, queries) |

## Testing & Simulations

See [SIMULATIONS.md](SIMULATIONS.md) for details on running simulations and tests.

## Common Gotchas

1. **Don't confuse Send protocol with SYNC_ALPN**: Sync updates/requests use Send, sync responses use SYNC_ALPN
2. **TopicMessage is only for Send protocol**: Direct SYNC_ALPN uses raw format `[topic_id][type_byte][data]`
3. **Harbor sync vs CRDT sync**: "Harbor sync" = syncing packets between harbor nodes; "CRDT sync" = application-level state synchronization
4. **Size limits**: Send protocol has 512KB limit; SYNC_ALPN has no limit (designed for large snapshots)
5. **The protocol is CRDT-agnostic**: Applications choose their own CRDT library (Loro, Yjs, Automerge, etc.)
6. **Protocol is just API**: Don't add business logic to Protocol - it delegates to Services
7. **Services call services**: Services communicate directly, not through Protocol
8. **Encryption layers**: QUIC provides transport encryption; packet encryption is only needed for Harbor storage

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
6. **Services are self-contained** - Each service owns its connections, handlers, and business logic
7. **Protocol is a thin shell** - API + startup only, no coordination logic
8. **Connection as transport primitive** - Per-peer, per-ALPN abstraction over QUIC
