# Unified Packet Type Design

## Goals

1. Single type enum for all messages (topic, DM, control)
2. Type byte encodes scope, verification mode, and exact type
3. Same decision logic for direct delivery and harbor pull
4. Harbor stores exact type (not lossy 3-value enum)
5. Clean bit layout for fast scope/mode checks

## Type Byte Layout

```
Ranges:
  0x00-0x3F = Topic-scoped (encrypted with topic key)
  0x40-0x4F = DM-scoped (encrypted with DM key, DM context)
  0x50-0x5F = Stream Signaling (DM encrypted, but serves topic & DM streams)
  0x60-0x7F = Reserved
  0x80-0xBF = Control (encrypted with DM key, special routing)
  0xC0-0xFF = Reserved
```

Scope derivation:
- `0x00-0x3F`: Topic
- `0x40-0x5F`: Dm (includes stream signaling - all DM encrypted)
- `0x80-0xBF`: Control

## Complete Type Assignment

### Topic-scoped (0x00-0x3F)

| Byte | Name | Verification | Payload | Notes |
|------|------|--------------|---------|-------|
| 0x00 | TopicContent | Full | Raw bytes | User content |
| 0x01 | TopicFileAnnounce | Full | FileAnnouncePayload | New file |
| 0x02 | TopicCanSeed | Full | CanSeedPayload | Has complete file |
| 0x03 | TopicSyncUpdate | Full | Raw CRDT bytes | Delta sync |
| 0x04 | TopicSyncRequest | Full | None | Request state |
| 0x05 | TopicStreamRequest | Full | StreamRequestPayload | Live stream |
| 0x06-0x3F | Reserved | - | - | Future topic types |

Note: Join/Leave removed from topic range - use Control TopicJoin/TopicLeave (with epoch tracking).

### DM-scoped (0x40-0x4F)

These are DM-encrypted AND DM-context messages (true peer-to-peer):

| Byte | Name | Verification | Payload | Notes |
|------|------|--------------|---------|-------|
| 0x40 | DmContent | Full | Raw bytes | User content |
| 0x41 | DmFileAnnounce | Full | FileAnnouncePayload | Same struct as topic |
| 0x42 | DmSyncUpdate | Full | Raw CRDT bytes | Delta sync |
| 0x43 | DmSyncRequest | Full | None | Request state |
| 0x44 | DmStreamRequest | Full | StreamRequestPayload | P2P stream request |
| 0x45-0x4F | Reserved | - | - | Future DM types |

### Stream Signaling (0x50-0x5F)

DM-encrypted but used for BOTH topic and DM streams (point-to-point responses):

| Byte | Name | Verification | Payload | Notes |
|------|------|--------------|---------|-------|
| 0x50 | StreamAccept | Full | StreamAcceptPayload | Accept stream |
| 0x51 | StreamReject | Full | StreamRejectPayload | Reject stream |
| 0x52 | StreamQuery | Full | StreamQueryPayload | Liveness check |
| 0x53 | StreamActive | Full | StreamActivePayload | Still active |
| 0x54 | StreamEnded | Full | StreamEndedPayload | Stream ended |
| 0x55-0x5F | Reserved | - | - | Future signaling types |

Note: Stream signaling payloads use `request_id` only (no topic_id on wire). DM encryption for point-to-point delivery.

### Control (0x80-0xBF)

| Byte | Name | Verification | HarborID | Payload | Notes |
|------|------|--------------|----------|---------|-------|
| 0x80 | ConnectRequest | Full | recipient | ConnectRequestPayload | Peer handshake |
| 0x81 | ConnectAccept | Full | requester | ConnectAcceptPayload | Accept peer |
| 0x82 | ConnectDecline | Full | requester | ConnectDeclinePayload | Decline peer |
| 0x83 | TopicInvite | Full | recipient | TopicInvitePayload | Send topic key |
| 0x84 | TopicJoin | **MacOnly** | topic_hash | TopicJoinPayload | Join w/ epoch |
| 0x85 | TopicLeave | Full | topic_hash | TopicLeavePayload | Leave w/ epoch |
| 0x86 | RemoveMember | Full | recipient | RemoveMemberPayload | Key rotation |
| 0x87 | Suggest | Full | recipient | SuggestPayload | Peer intro |
| 0x88-0xBF | Reserved | - | - | - | Future control types |

## Verification Mode Derivation

```rust
impl PacketType {
    pub fn verification_mode(&self) -> VerificationMode {
        match self {
            // Only TopicJoin uses MacOnly (sender may be unknown to some recipients)
            PacketType::TopicJoin => VerificationMode::MacOnly,
            // Everything else uses Full
            _ => VerificationMode::Full,
        }
    }
}
```

## Scope Derivation

```rust
impl PacketType {
    pub fn scope(&self) -> Scope {
        let b = *self as u8;
        match b {
            0x00..=0x3F => Scope::Topic,
            0x40..=0x5F => Scope::Dm,  // Includes stream signaling
            0x80..=0xBF => Scope::Control,
            _ => Scope::Reserved,
        }
    }

    pub fn is_topic(&self) -> bool {
        (*self as u8) < 0x40
    }

    /// True for DM-scoped AND stream signaling (all DM-encrypted)
    pub fn is_dm(&self) -> bool {
        let b = *self as u8;
        b >= 0x40 && b < 0x60
    }

    /// True for stream signaling specifically (subset of DM-encrypted)
    pub fn is_stream_signaling(&self) -> bool {
        let b = *self as u8;
        b >= 0x50 && b < 0x60
    }

    pub fn is_control(&self) -> bool {
        let b = *self as u8;
        b >= 0x80 && b < 0xC0
    }
}
```

## HarborID Derivation

```rust
impl PacketType {
    /// How to derive the harbor_id for storage/routing
    pub fn harbor_id_source(&self) -> HarborIdSource {
        match self {
            // Topic-scoped: harbor_id = hash(topic_id)
            PacketType::TopicContent
            | PacketType::TopicFileAnnounce
            | PacketType::TopicCanSeed
            | PacketType::TopicSyncUpdate
            | PacketType::TopicSyncRequest
            | PacketType::TopicStreamRequest => HarborIdSource::TopicHash,

            // Control with topic scope: harbor_id = hash(topic_id)
            PacketType::TopicJoin
            | PacketType::TopicLeave => HarborIdSource::TopicHash,

            // DM and most Control: harbor_id = recipient_id
            _ => HarborIdSource::RecipientId,
        }
    }
}

pub enum HarborIdSource {
    TopicHash,    // BLAKE3(topic_id)
    RecipientId,  // Direct recipient endpoint ID
}
```

## Payload Structures

Stream signaling payloads (0x50-0x54) use `request_id` only - no topic_id on wire:

```rust
pub struct StreamAcceptPayload {
    pub request_id: [u8; 32],  // Correlates to original StreamRequest
}

pub struct StreamRejectPayload {
    pub request_id: [u8; 32],
    pub reason: Option<String>,
}

pub struct StreamQueryPayload {
    pub request_id: [u8; 32],
}

pub struct StreamActivePayload {
    pub request_id: [u8; 32],
}

pub struct StreamEndedPayload {
    pub request_id: [u8; 32],
}
```

## Is User Content?

```rust
impl PacketType {
    /// Returns true if this is user-facing content (vs internal control)
    pub fn is_user_content(&self) -> bool {
        matches!(self, PacketType::TopicContent | PacketType::DmContent)
    }

    /// Returns true if app should receive an event for this message
    pub fn emits_event(&self) -> bool {
        match self {
            // User content always emits
            PacketType::TopicContent | PacketType::DmContent => true,
            // Sync emits for app to handle CRDT
            PacketType::TopicSyncUpdate | PacketType::DmSyncUpdate => true,
            PacketType::TopicSyncRequest | PacketType::DmSyncRequest => true,
            // File announcements emit
            PacketType::TopicFileAnnounce | PacketType::DmFileAnnounce => true,
            // Stream signaling emits
            PacketType::TopicStreamRequest | PacketType::DmStreamRequest => true,
            PacketType::StreamAccept
            | PacketType::StreamReject => true,
            // Control messages emit specific events
            PacketType::ConnectRequest
            | PacketType::ConnectAccept
            | PacketType::ConnectDecline => true,
            PacketType::TopicInvite => true,
            PacketType::RemoveMember => true,
            PacketType::Suggest => true,
            // Join/Leave/CanSeed are handled internally, no app event
            _ => false,
        }
    }
}
```

## Encryption Key Selection

```rust
impl PacketType {
    pub fn encryption_key_type(&self) -> EncryptionKeyType {
        match self {
            // Topic-scoped messages use topic key
            PacketType::TopicContent
            | PacketType::TopicFileAnnounce
            | PacketType::TopicCanSeed
            | PacketType::TopicSyncUpdate
            | PacketType::TopicSyncRequest
            | PacketType::TopicStreamRequest => EncryptionKeyType::TopicKey,

            // TopicJoin/TopicLeave are Control but use topic key
            // (broadcast to all topic members)
            PacketType::TopicJoin
            | PacketType::TopicLeave => EncryptionKeyType::TopicKey,

            // Everything else (DM, Stream Signaling, other Control) uses DM key
            _ => EncryptionKeyType::DmKey,
        }
    }
}

pub enum EncryptionKeyType {
    TopicKey,  // Symmetric key from topic epoch
    DmKey,     // ECDH shared secret with recipient
}
```

Note: TopicJoin/TopicLeave are in Control range (0x84-0x85) but use topic encryption because they're broadcast to all topic members. This is intentional - they're control messages conceptually but topic-scoped for delivery.

## Harbor Storage

Replace `HarborPacketType` with just storing the `PacketType` byte directly:

```rust
// OLD (lossy)
pub enum HarborPacketType {
    Content,  // Loses FileAnnounce, CanSeed, Sync, Stream info
    Join,
    Leave,
}

// NEW (lossless)
pub struct PacketInfo {
    pub packet_id: [u8; 32],
    pub sender_id: [u8; 32],
    pub packet_data: Vec<u8>,
    pub created_at: i64,
    pub packet_type: u8,  // Store actual PacketType byte
}
```

## Unified Processing Logic

```rust
fn process_packet(packet_type: PacketType, plaintext: &[u8], sender_id: [u8; 32]) {
    match packet_type.scope() {
        Scope::Topic => process_topic_packet(packet_type, plaintext, sender_id),
        Scope::Dm => process_dm_packet(packet_type, plaintext, sender_id),
        Scope::Control => process_control_packet(packet_type, plaintext, sender_id),
        Scope::Reserved => { /* ignore */ }
    }
}

fn process_topic_packet(packet_type: PacketType, plaintext: &[u8], sender_id: [u8; 32]) {
    match packet_type {
        PacketType::TopicContent => emit_message_event(plaintext, sender_id),
        PacketType::TopicFileAnnounce => {
            let ann: FileAnnouncePayload = decode(plaintext);
            store_file_metadata(ann);
        }
        PacketType::TopicCanSeed => {
            let seed: CanSeedPayload = decode(plaintext);
            record_seeder(seed);
        }
        PacketType::TopicSyncUpdate => emit_sync_update_event(plaintext, sender_id),
        PacketType::TopicSyncRequest => emit_sync_request_event(sender_id),
        PacketType::TopicStreamRequest => {
            let req: StreamRequestPayload = decode(plaintext);
            handle_stream_request(req, sender_id);
        }
        _ => { /* unknown topic type, ignore */ }
    }
}

fn process_control_packet(packet_type: PacketType, plaintext: &[u8], sender_id: [u8; 32]) {
    match packet_type {
        PacketType::ConnectRequest => { /* peer handshake */ }
        PacketType::ConnectAccept => { /* accept peer */ }
        PacketType::ConnectDecline => { /* decline peer */ }
        PacketType::TopicInvite => { /* receive topic key */ }
        PacketType::TopicJoin => {
            let join: TopicJoinPayload = decode(plaintext);
            add_topic_member(join.sender_id, join.relay_url);
        }
        PacketType::TopicLeave => {
            let leave: TopicLeavePayload = decode(plaintext);
            remove_topic_member(leave.sender_id);
        }
        PacketType::RemoveMember => { /* key rotation */ }
        PacketType::Suggest => { /* peer intro */ }
        _ => { /* unknown control type, ignore */ }
    }
}
```

## Migration Path

Clean break (dev mode):

1. Define new `PacketType` enum with new byte values
2. Update encode/decode to use new values
3. Update harbor storage schema to use `packet_type: u8`
4. Remove `TopicMessage::Join` and `MessageType::Join`
5. Clear any existing harbor cache data

## Summary of Changes

| Current | New |
|---------|-----|
| `MessageType` (0x00-0x07) | `PacketType::Topic*` (0x00-0x05) |
| `DmMessageType` (0x80-0x8C) | `PacketType::Dm*` (0x40-0x44) + `StreamSignaling` (0x50-0x54) |
| `ControlPacketType` (0x01-0x30) | `PacketType::*` (0x80-0x87) |
| `HarborPacketType` (3 values) | `packet_type: u8` (full fidelity) |
| 3 verification methods | 1 method: `verification_mode()` |
| `plaintext[0] < 0x80` check | `packet_type.scope()` |
| Type collision (0x01-0x03) | No collisions |
| `TopicMessage::Join/Leave` | Removed - use `TopicJoin/TopicLeave` in Control |

## Edge Cases Resolved

1. **Type collision**: No overlap between ranges
2. **Harbor lossy storage**: Store exact type byte
3. **Control inside DM**: Control has its own range (0x80+)
4. **One Join type**: Only TopicJoin (0x84) with epoch tracking
5. **One Leave type**: Only TopicLeave (0x85) with epoch tracking
6. **Verification fragmentation**: Single `verification_mode()` method
7. **DM gaps**: Sequential assignment within range
8. **Stream signaling**: Separate range (0x50-0x5F), no topic_id in payload (use request_id), serves both topic and DM streams
9. **SyncUpdate encoding**: Standardize to raw bytes (no wrapper)

## Files to Modify

1. **Create**: `core/src/data/packet_type.rs` - New unified PacketType enum
2. **Remove**: `TopicMessage::Join` and `TopicMessage::Leave` from `topic_messages.rs`
3. **Remove**: `MessageType::Join` and `MessageType::Leave` from `topic_messages.rs`
4. **Update**: `HarborPacketType` → use `u8` packet_type directly
5. **Update**: `harbor/protocol.rs` - PacketInfo uses `packet_type: u8`
6. **Update**: `harbor_pull.rs` - Use unified processing logic
7. **Update**: `send/outgoing.rs` - Use PacketType for sends
8. **Update**: All encode/decode to use new byte values
