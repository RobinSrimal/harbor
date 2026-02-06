# Protocol Overview

Harbor uses multiple protocols, each identified by an ALPN (Application-Layer Protocol Negotiation) string.

## Protocol Stack

```
┌─────────────────────────────────────────┐
│           Application Layer              │
│  (Topics, DMs, Files, Sync, Streams)    │
└─────────────────────────────────────────┘
                    │
┌─────────────────────────────────────────┐
│          Harbor Protocols                │
│  Send, Control, Share, Sync, Stream     │
└─────────────────────────────────────────┘
                    │
┌─────────────────────────────────────────┐
│           QUIC Transport                 │
│     (Encryption, Multiplexing)          │
└─────────────────────────────────────────┘
                    │
┌─────────────────────────────────────────┐
│           UDP / Relay                    │
└─────────────────────────────────────────┘
```

## Protocols Summary

| Protocol | ALPN | Purpose |
|----------|------|---------|
| Send | `harbor/send/0` | Message delivery & receipts |
| Harbor | `harbor/harbor/0` | Offline storage & sync |
| Control | `harbor/control/0` | Connections, invites, membership |
| Share | `harbor/share/0` | File chunk distribution |
| Sync | `harbor/sync/0` | Large CRDT snapshots |
| DHT | `harbor/dht/0` | Peer discovery |

## Send Protocol

The primary messaging protocol. Handles:

- Encrypted message delivery
- Delivery receipts
- CRDT sync updates and requests

**Size limit**: 512 KB per message

**Flow**:
1. Encrypt and sign the packet
2. Send directly to all online recipients
3. Collect receipts
4. Replicate to Harbor Nodes for missing receipts

## Control Protocol

Manages peer lifecycle and topic membership:

- Connection requests/accepts/declines
- Topic invites
- Member join/leave announcements
- Peer suggestions

All control messages require Proof of Work to prevent spam.

## Share Protocol

BitTorrent-style file distribution:

1. File is split into 512KB chunks
2. Chunks are content-addressed (BLAKE3 hash)
3. Announcement is broadcast via Send protocol
4. Recipients pull chunks from seeders
5. Completed downloads become seeders

**No size limit** - designed for large files.

## Sync Protocol

For large CRDT state transfers:

- Used for `SyncResponse` (full state snapshots)
- Direct peer-to-peer, no Harbor storage
- **No size limit**

Sync *updates* and *requests* use the Send protocol.

## Harbor Protocol

Store-and-forward for offline peers:

- `Store`: Save a packet for later delivery
- `Pull`: Retrieve packets by HarborID
- `HarborSync`: Sync between Harbor Nodes

See [Harbor Nodes](../architecture/harbor-nodes.md) for details.

## DHT Protocol

Kademlia-style distributed hash table:

- Peer discovery by EndpointID
- Harbor Node discovery by HarborID
- Routing table maintenance

## Transport Layer

All protocols run over QUIC (via the [iroh](https://iroh.computer/) library):

- **Encryption**: TLS 1.3 built into QUIC
- **Multiplexing**: Multiple streams per connection
- **NAT traversal**: Via relay servers when needed
- **Connection migration**: Survives IP changes
