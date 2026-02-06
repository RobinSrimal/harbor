# Services & Protocols

Harbor's architecture is organized around self-contained services, each with its own ALPN (Application-Layer Protocol Negotiation) identifier.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Protocol (API Layer)                      │
│         Thin shell: startup, shutdown, public methods        │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                        Services                              │
│                                                              │
│  ┌───────────┐ ┌─────────────┐ ┌───────────┐ ┌───────────┐  │
│  │SendService│ │HarborService│ │ShareService│ │ DhtService│  │
│  └───────────┘ └─────────────┘ └───────────┘ └───────────┘  │
│  ┌───────────┐ ┌─────────────┐                              │
│  │SyncService│ │ControlService│                              │
│  └───────────┘ └─────────────┘                              │
│                                                              │
│              Services call each other directly               │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   Shared Resources                           │
│        Database, Event Channel, Identity, Endpoint          │
└─────────────────────────────────────────────────────────────┘
```

## Service Summary

| Service | ALPN | Purpose |
|---------|------|---------|
| SendService | `harbor/send/0` | Message delivery and receipts |
| HarborService | `harbor/harbor/0` | Store-and-forward, harbor-to-harbor sync |
| ControlService | `harbor/control/0` | Peer connections, topic invites |
| ShareService | `harbor/share/0` | File chunk requests and transfers |
| SyncService | `harbor/sync/0` | Large sync responses (CRDT snapshots) |
| DhtService | `harbor/dht/0` | Peer discovery, routing |

---

## SendService (`harbor/send/0`)

Primary messaging protocol for delivering messages to online peers.

**Responsibilities**:
- Message delivery to online peers
- Delivery receipt collection
- Triggering Harbor storage when receipts are missing

**Message Types**:
- `TopicMessage`: Encrypted message to topic members
- `DmMessage`: Encrypted direct message
- `SyncUpdate`: CRDT delta update
- `SyncRequest`: Request full CRDT state
- `Receipt`: Delivery confirmation

**Characteristics**:
- Size limit: 512 KB
- Supports Harbor storage for offline delivery
- Per-peer PoW (8-20 bits, byte-based scaling)

---

## HarborService (`harbor/harbor/0`)

Store-and-forward service for offline message delivery.

**Responsibilities**:
- Storing packets for offline recipients
- Handling `Pull` requests from peers
- Harbor-to-Harbor synchronization
- Storage quota enforcement

**Message Types**:
- `Store`: Store a packet for later delivery
- `Pull`: Retrieve packets by HarborID
- `HarborSync`: Sync packets between Harbor Nodes

**Characteristics**:
- Packets encrypted with topic keys (Harbor Nodes can't read content)
- PoW required (18-32 bits, byte-based scaling)
- Storage limits enforced (default 10 GB)

---

## ControlService (`harbor/control/0`)

Manages peer lifecycle and topic membership.

**Responsibilities**:
- Peer connection lifecycle (requests, accepts, declines)
- Topic invites and membership management
- Blocking and peer suggestions

**Message Types**:
- `ConnectRequest` / `ConnectAccept` / `ConnectDecline`
- `TopicInvite`
- `TopicJoin` / `TopicLeave`
- `RemoveMember`
- `Suggest`

**Characteristics**:
- PoW required (18-28 bits, request-based scaling)
- All messages are authenticated

---

## ShareService (`harbor/share/0`)

BitTorrent-style P2P file distribution.

**Responsibilities**:
- File chunk management
- Distribution coordination
- Seeder tracking

**Message Types**:
- `ChunkRequest`: Request specific chunks
- `ChunkResponse`: Chunk data
- `Bitfield`: Advertise available chunks
- `CanSeed`: Announce seeder status

**Characteristics**:
- No size limit
- Content-addressed chunks (BLAKE3)
- No PoW (flow controlled by chunk requests)

---

## SyncService (`harbor/sync/0`)

For large CRDT state transfers that exceed the Send protocol's 512 KB limit.

**Responsibilities**:
- Large CRDT snapshot transfers
- Direct peer-to-peer sync responses

**Usage**:
- Only for `SyncResponse` messages
- Direct peer-to-peer (no Harbor storage)

**Characteristics**:
- No size limit
- No PoW
- Used for initial state sync

Note: Sync *updates* and *requests* use the Send protocol. Only *responses* use this dedicated protocol.

---

## DhtService (`harbor/dht/0`)

Kademlia-style distributed hash table for peer discovery.

**Responsibilities**:
- Routing table maintenance
- Peer and Harbor Node lookups
- Bootstrap process

**Message Types**:
- `FindNode`: Look up nodes near an ID
- `FindValue`: Look up a specific value
- `Store`: Store a value in the DHT
- `Ping`: Keep-alive

**Characteristics**:
- Kademlia-style routing
- PoW required (18-28 bits, request-based scaling)

---

## Service Interaction

Services communicate directly, not through the Protocol layer:

```
User calls: protocol.send(topic, data)
                │
                ▼
        SendService.send()
                │
                ├─► Try direct delivery
                │
                └─► If offline → HarborService.store()
```

Example flow for sending a message:

1. `Protocol.send()` delegates to `SendService`
2. `SendService` encrypts and sends directly
3. If receipts are missing, `SendService` calls `HarborService.store()`
4. `HarborService` uses `DhtService` to find Harbor Nodes
5. Packets are stored on Harbor Nodes

## Connection Pools

Each service maintains its own connection pool for its ALPN:

```
┌─────────────────────────────────────────┐
│              Protocol                    │
└─────────────────────────────────────────┘
         │         │         │
         ▼         ▼         ▼
┌─────────┐ ┌─────────┐ ┌─────────┐
│  Send   │ │ Harbor  │ │   DHT   │
│  Pool   │ │  Pool   │ │  Pool   │
└─────────┘ └─────────┘ └─────────┘
         │         │         │
         ▼         ▼         ▼
┌─────────────────────────────────────────┐
│          QUIC Connections               │
│     (separate streams per ALPN)         │
└─────────────────────────────────────────┘
```

This allows:
- Independent rate limiting per protocol
- Separate PoW tracking per ALPN
- Connection reuse within a service

## Background Tasks

Services spawn background tasks for periodic operations:

| Task | Service | Purpose |
|------|---------|---------|
| Harbor Pull | HarborService | Pull missed packets |
| Harbor Replication | HarborService | Sync with other Harbor Nodes |
| DHT Persist | DhtService | Save routing table |
| DHT Refresh | DhtService | Maintain routing table |
