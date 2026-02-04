# Control ALPN (`harbor/control/0`)

## Overview

A dedicated ALPN for all lifecycle and relationship management. One-off exchanges: open connection, send message, get response, close. Not part of the data plane - control is infrastructure.

Implemented as `ControlService` under `network/control/service.rs`, following the existing service pattern. Owns no connection pool.

## Message Types

| Message | Direction | Purpose |
|---------|-----------|---------|
| Connect Request | B → C | Request peer-level connection |
| Connect Accept | C → B | Accept connection |
| Connect Decline | C → B | Decline connection |
| Topic Invite | A → B | Send topic key + metadata |
| Topic Join | B → topic members | Join a topic (with valid topic key) |
| Topic Leave | B → topic members | Voluntarily leave a topic |
| Suggest | A → B | Send endpoint ID + relay URL of a third party C |
| Block | A → (local) | Block a peer (may not need a message, local state change) |

| Remove Member | Admin → remaining members (via control) | Key rotation + new epoch key distribution |
| Admin Transfer | Admin → new admin | Transfer admin role (future) |

## Member Removal (Key Rotation)

No MLS. Admin-broadcast key rotation instead.

### Design
- **Harbor ID** (topic ID) stays the same
- **Epoch** increments with each key rotation (counter for identifying which key to use)
- **Key** is fresh randomness per epoch (no derivation from previous key)
- Messages tagged with epoch number in header
- Members store all epoch keys they've received
- Only admin can rotate keys / remove members

### Removal Flow
1. Admin generates new random key for epoch N+1
2. Admin sends new key via control ALPN to each remaining member (not the removed member)
3. Admin starts encrypting with epoch N+1 immediately
4. Offline members receive the new key via Harbor when they come online

### Offline Key Delivery
Members may pull topic messages from Harbor before receiving the epoch key:
1. Member pulls message with unknown epoch N+1
2. Stores in pending decryption buffer
3. Control message with epoch N+1 key arrives (also via Harbor)
4. Buffered messages decrypted

No ordering dependency, just eventual delivery of both key and messages.

### Epoch Key Retention
- Keys retained for 3 months
- After 3 months, old epoch keys are pruned
- Messages encrypted with pruned keys become undecryptable

### Open Concerns
- **Admin as single point of failure** - if admin goes permanently offline, no removal possible. Admin transfer (control message) mitigates this.
- **Admin key compromise** - attacker could remove legitimate members. No easy fix without multi-party authority.

## Connection List (Peer-Level)

Tracks peer relationships. Governs DM access across send, share, sync, and stream ALPNs.

| State | DM Access | Topic Access |
|-------|-----------|--------------|
| **Connected** | Full access to send/share/sync/stream for DMs | Unaffected |
| **Pending** | No access | Unaffected |
| **Declined/Blocked** | No access | Unaffected |

- Peers are added via connect handshake or topic join.
- Block moves a peer from connected to declined.
- Block only affects data ALPNs (send, share, sync, stream) at the DM level.
- Block does not affect control, dht, or harbor.
- Block and topic membership are fully independent. A blocked peer can still participate in shared topics.

## Connect Handshake Flow

1. B sends connect request to C on control ALPN
2. C's app receives event, decides accept/decline
3. C sends accept/decline response on control ALPN
4. Both sides update connection state

Verification: endpoint ID in message must match the QUIC connection peer ID. Drop if mismatch.

## Token Connect Flow (QR Code / Invite String)

1. A calls protocol to generate a connect token
2. Protocol creates a one-time secret, stores it in the database
3. A receives: endpoint ID + relay URL + secret (the invite string, app displays as QR code or text)
4. B scans/pastes invite string, sends connect request with the secret on control ALPN
5. A's node validates secret against stored tokens:
   - Valid: auto-accept, delete token, both sides move to connected
   - Invalid or already consumed: decline
6. Tokens are single-use, no expiry, live until consumed

## Introduction Flow

1. A sends a Suggest message (C's endpoint ID + relay URL) to B on control ALPN
2. B's app receives event, decides whether to reach out
3. B initiates connect handshake with C on control ALPN (steps above)
4. One-sided: only B acts initially, C receives and decides

## Topic Invite Flow

1. A sends topic invite (topic key + metadata) to B on control ALPN
2. B's app receives event, decides accept/reject
3. If accepted, B sends join message (encrypted with topic key) on control ALPN

## Access Control Per ALPN

| ALPN | Rule |
|------|------|
| **control** | Always accept. PoW required (high base difficulty). |
| **send** | Topic messages: check topic membership. DMs: check connection state (connected only). PoW required (low base difficulty). |
| **share/sync/stream** | Topic-scoped: check topic membership. DM-scoped: check connection state (connected only). PoW required (low base difficulty). |
| **dht** | PoW required (high base difficulty). |
| **harbor** | PoW required (high base difficulty, adaptive scaling per peer). |

## Harbor Replication

Control messages use harbor for offline delivery. Two harbor_id schemes based on message type:

| Message Type | Harbor ID | Rationale |
|--------------|-----------|-----------|
| Connect Request/Accept/Decline | `recipient_id` | Point-to-point |
| Suggest | `recipient_id` | Point-to-point |
| Topic Invite | `recipient_id` | Point-to-point |
| Topic Join | `hash(topic_id)` | Topic-scoped |
| Topic Leave | `hash(topic_id)` | Topic-scoped |
| Remove Member | `hash(topic_id)` | Topic-scoped |

### Topic-Scoped Messages

- Use `hash(original_topic_id)` - the topic ID never changes
- Recipients list in StoreRequest specifies who can pull
- Harbor nodes filter by recipient - only return packets to authorized recipients
- Removed members cannot pull Remove Member messages (not in recipients list)

### Point-to-Point Messages

- Use `recipient_id` as harbor_id
- Same pattern as DMs in the send ALPN
- Recipient pulls from their own endpoint ID

### Recipient Filtering

Harbor nodes enforce strict recipient filtering:
1. StoreRequest includes explicit `recipients` list
2. PullRequest includes `recipient_id` (verified via QUIC authentication)
3. Database query joins with `harbor_recipients` table
4. Only packets where requester is in recipients list are returned

This ensures topic-scoped messages at `hash(topic_id)` are only delivered to authorized members.

### Packet Encryption (for Harbor Storage)

Control messages follow the same pattern as Send messages:
- Direct delivery via QUIC uses transport encryption (no packet encryption needed)
- All control messages also land in `outgoing_packets` table for Harbor replication
- Packet encryption is only for "at rest" storage in Harbor

Encryption key by message type:

| Message Type | Encryption Key | Rationale |
|--------------|----------------|-----------|
| Connect Request/Accept/Decline | DM shared key | Point-to-point, same as DMs |
| Suggest | DM shared key | Point-to-point |
| Topic Invite | DM shared key | Point-to-point (topic key is the payload) |
| Topic Join | Topic epoch key | Topic-scoped |
| Topic Leave | Topic epoch key | Topic-scoped |
| Remove Member (key delivery) | DM shared key | Must use DM key since topic key is being rotated |

**Key delivery bootstrap**: Remove Member messages deliver the new epoch key. They cannot be encrypted with the topic key (that's what's being rotated). Instead, admin sends a separate DM-encrypted message to each remaining member containing the new epoch key.

### Pending Decryption

Members may pull packets from Harbor before receiving the corresponding epoch key:

1. Packet arrives with unknown epoch N+1
2. Packet stored in `pending_decryption` table (not discarded)
3. Epoch key arrives via control message (also pulled from Harbor)
4. Pending packets for that epoch are decrypted and processed
5. Successfully decrypted packets removed from pending table

Cleanup: Pending packets for expired epochs (>3 months) are deleted since the key will never arrive.

## Open Questions

- Connection list schema in data layer
- When does a topic join add a peer to the connection list? Automatically or app-level decision?
- Can a blocked peer re-request connection, or is it permanent until the blocker reverses it?
- Control ALPN message format/encoding
- Replay protection: promote from MLS archive or reimplement?
- Message size limits: specific values per ALPN
- Connection throttling: thresholds and time windows
