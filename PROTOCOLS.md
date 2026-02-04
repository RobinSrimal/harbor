# Harbor Protocol Message Types

Overview of all message types across ALPNs - implemented and planned.

## ALPN Summary

| ALPN | Status | Purpose |
|------|--------|---------|
| `harbor/send/0` | Implemented | Direct message delivery |
| `harbor/store/0` | Implemented | Store-and-forward for offline peers |
| `harbor/dht/0` | Implemented | Peer discovery (Kademlia) |
| `harbor/share/0` | Implemented | File chunk distribution |
| `harbor/sync/0` | Implemented | Large sync responses |
| `harbor/stream/0` | Implemented | Live streaming (MOQ-based) |
| `harbor/control/0` | Planned | Lifecycle & relationship management |

---

## Send Protocol (`harbor/send/0`)

Direct message delivery with read receipts. RPC-based via irpc.

### RPC Messages

| Message | Direction | Purpose |
|---------|-----------|---------|
| DeliverTopic | A → B | Deliver topic message |
| DeliverDm | A → B | Deliver DM |
| Receipt | B → A | Read receipt |

### Topic Message Types (prefix 0x00-0x07)

| Byte | Type | Purpose |
|------|------|---------|
| 0x00 | Content | Raw application payload |
| 0x01 | Join | Announce topic membership |
| 0x02 | Leave | Announce departure |
| 0x03 | FileAnnouncement | Share new file metadata |
| 0x04 | CanSeed | Peer has complete file |
| 0x05 | SyncUpdate | CRDT delta |
| 0x06 | SyncRequest | Request full CRDT state |
| 0x07 | StreamRequest | Live stream request |

### DM Message Types (prefix 0x80+)

| Byte | Type | Purpose |
|------|------|---------|
| 0x80 | Content | Raw application payload |
| 0x83 | FileAnnouncement | Share file metadata |
| 0x85 | SyncUpdate | CRDT delta |
| 0x86 | SyncRequest | Request full state |
| 0x87 | StreamAccept | Accept stream request |
| 0x88 | StreamReject | Reject stream request |
| 0x89 | StreamQuery | Query stream status |
| 0x8A | StreamActive | Stream is active |
| 0x8B | StreamEnded | Stream has ended |
| 0x8C | StreamRequest | Request stream |

---

## Harbor Protocol (`harbor/store/0`)

Store-and-forward for offline peers. Harbor nodes cache packets.

### Messages

| Message | Direction | Purpose |
|---------|-----------|---------|
| Store | Client → Harbor | Store packet for recipients |
| StoreResponse | Harbor → Client | Confirm storage |
| Pull | Client → Harbor | Request packets for recipient |
| PullResponse | Harbor → Client | Return matching packets |
| Ack | Client → Harbor | Acknowledge delivery |
| SyncRequest | Harbor → Harbor | Sync packets between nodes |
| SyncResponse | Harbor → Harbor | Return packets for sync |

### Packet Types

| Type | Verification | Purpose |
|------|--------------|---------|
| Content (0) | Full (MAC + signature) | Regular messages |
| Join (1) | MAC-only | Join announcements (sender unknown) |
| Leave (2) | Full | Leave announcements |

---

## DHT Protocol (`harbor/dht/0`)

Kademlia-based peer discovery. RPC via irpc.

### Messages

| Message | Direction | Purpose |
|---------|-----------|---------|
| FindNode | A → B | Query for nodes closest to target |
| FindNodeResponse | B → A | Return closest known nodes |

### NodeInfo Fields

- `node_id` - 32-byte endpoint ID
- `relay_url` - Optional relay URL for NAT traversal

---

## Share Protocol (`harbor/share/0`)

Pull-based file distribution. Chunked transfer with BitTorrent-style swarming.

### Messages (prefix 0x01-0x07)

| Byte | Type | Purpose |
|------|------|---------|
| 0x01 | ChunkMapRequest | Request chunk availability |
| 0x02 | ChunkMapResponse | Return chunk map |
| 0x03 | ChunkRequest | Request specific chunks |
| 0x04 | ChunkResponse | Return chunk data |
| 0x05 | Bitfield | Exchange chunk availability |
| 0x06 | PeerSuggestion | Redirect to another peer |
| 0x07 | ChunkAck | Acknowledge chunk receipt |

### Related Send Messages

| Marker | Type | Purpose |
|--------|------|---------|
| 0xF1 | FileAnnouncement | Announce file via Send protocol |
| 0xF2 | CanSeed | Announce complete file via Send |

---

## Sync Protocol (`harbor/sync/0`)

CRDT synchronization. Small updates via Send, large responses via direct connection.

### Messages

| Byte | Type | Transport | Purpose |
|------|------|-----------|---------|
| 0x01 | SyncUpdate | Send ALPN | CRDT delta (≤512KB) |
| 0x02 | InitialSyncRequest | Send ALPN | Request full state |
| 0x03 | InitialSyncResponse | Sync ALPN | Full CRDT snapshot (unlimited) |

### Wire Format

- Via Send: `[0x53][type][length][payload]`
- Via Sync ALPN: `[type][length][payload]`

---

## Stream Protocol (`harbor/stream/0`)

Live streaming via MOQ (Media over QUIC). Signaling via DM messages.

### Signaling (via DM)

| Type | Purpose |
|------|---------|
| StreamRequest | Request to start stream |
| StreamAccept | Accept stream request |
| StreamReject | Reject stream request |
| StreamQuery | Query stream status |
| StreamActive | Stream is live |
| StreamEnded | Stream has ended |

### Data Transport

- Direct QUIC streams via Stream ALPN
- MOQ-based framing for media data
- Signaling packets stored on Harbor (6-hour TTL)

---

## Control Protocol (`harbor/control/0`) - PLANNED

Lifecycle and relationship management. One-off exchanges.

### Connection Messages

| Message | Direction | Purpose |
|---------|-----------|---------|
| Connect Request | B → C | Request peer connection |
| Connect Accept | C → B | Accept connection |
| Connect Decline | C → B | Decline connection |

### Topic Messages

| Message | Direction | Purpose |
|---------|-----------|---------|
| Topic Invite | A → B | Send topic key + metadata |
| Topic Join | B → members | Join topic (with valid key) |
| Topic Leave | B → members | Leave topic |

### Relationship Messages

| Message | Direction | Purpose |
|---------|-----------|---------|
| Suggest | A → B | Introduce third party C |
| Block | A → local | Block peer (local state) |
| Remove Member | Admin → members | Key rotation + removal |
| Admin Transfer | Admin → new | Transfer admin role |

### Harbor Replication

| Message Type | Harbor ID | Encryption |
|--------------|-----------|------------|
| Connect Request/Accept/Decline | `recipient_id` | DM shared key |
| Suggest | `recipient_id` | DM shared key |
| Topic Invite | `recipient_id` | DM shared key |
| Topic Join | `hash(topic_id)` | Topic epoch key |
| Topic Leave | `hash(topic_id)` | Topic epoch key |
| Remove Member | `hash(topic_id)` | DM shared key* |

*Remove Member uses DM shared key because it delivers the new epoch key.

---

## Cross-Protocol Patterns

### Wire Format

- **RPC-based**: Send, DHT (via irpc)
- **Type-prefixed**: Share (0x01-0x07), Sync, Harbor

### Harbor Routing

| Scope | Harbor ID |
|-------|-----------|
| Topic messages | `hash(topic_id)` |
| DM messages | `recipient_id` |
| Point-to-point control | `recipient_id` |
| Topic-scoped control | `hash(topic_id)` |

### Encryption

| Context | Encryption |
|---------|------------|
| Direct QUIC delivery | Transport only (QUIC) |
| Harbor storage | Packet encrypted (topic key or DM shared key) |

### Proof of Work

| ALPN | Base Difficulty |
|------|-----------------|
| control | High |
| dht | High |
| harbor | High (adaptive per peer) |
| send | Low |
| share/sync/stream | Low |

---

## File Locations

| Protocol | Implementation |
|----------|----------------|
| Send | `core/src/network/send/` |
| Harbor | `core/src/network/harbor/` |
| DHT | `core/src/network/dht/` |
| Share | `core/src/network/share/` |
| Sync | `core/src/network/sync/` |
| Stream | `core/src/network/stream/` |
| Control | `core/src/Control.md` (design doc) |
