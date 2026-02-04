# Unified Packet Type Implementation Plan

## Overview

Consolidate `MessageType`, `DmMessageType`, `ControlPacketType`, and `HarborPacketType` into a single `PacketType` enum. Harbor stores opaque encrypted bytes; recipients derive decryption key from harbor_id.

## Phase 1: Create Unified PacketType

### 1.1 Create `core/src/data/packet_type.rs`

New unified enum with:
- Topic-scoped (0x00-0x05)
- DM-scoped (0x40-0x44)
- Stream signaling (0x50-0x54)
- Control (0x80-0x87)

Methods:
- `from_byte(u8) -> Option<Self>`
- `as_byte() -> u8`
- `scope() -> Scope`
- `verification_mode() -> VerificationMode`
- `harbor_id_source() -> HarborIdSource`
- `encryption_key_type() -> EncryptionKeyType`
- `is_topic()`, `is_dm()`, `is_control()`, `is_stream_signaling()`
- `emits_event() -> bool`

### 1.2 Update `core/src/data/mod.rs`

- Add `pub mod packet_type;`
- Re-export `PacketType`, `Scope`, `HarborIdSource`, `EncryptionKeyType`

## Phase 2: Update Harbor Protocol (Opaque Storage)

### 2.1 Update `core/src/network/harbor/protocol.rs`

Remove `HarborPacketType` enum entirely. Update:

```rust
// OLD
pub struct StoreRequest {
    pub packet_type: HarborPacketType,
    ...
}

pub struct PacketInfo {
    pub packet_type: HarborPacketType,
    ...
}

// NEW - no packet_type field at all
pub struct StoreRequest {
    pub packet_data: Vec<u8>,
    pub packet_id: [u8; 32],
    pub harbor_id: [u8; 32],
    pub sender_id: [u8; 32],
    pub recipients: Vec<[u8; 32]>,
    // NO packet_type - harbor doesn't need it
}

pub struct PacketInfo {
    pub packet_id: [u8; 32],
    pub sender_id: [u8; 32],
    pub packet_data: Vec<u8>,
    pub created_at: i64,
    // NO packet_type - recipient derives from harbor_id + plaintext[0]
}
```

### 2.2 Update `core/src/data/harbor/cache.rs`

Remove `packet_type` column from schema and queries:
- `cache_packet()` - remove packet_type parameter
- `get_cached_packet()` - remove packet_type from return
- `get_packets_for_recipient()` - remove packet_type from PacketInfo

## Phase 3: Update Processing Logic

### 3.1 Update `core/src/tasks/harbor_pull.rs`

Replace current verification mode logic:

```rust
// OLD
let mode = match packet_info.packet_type {
    HarborPacketType::Join => VerificationMode::MacOnly,
    HarborPacketType::Content | HarborPacketType::Leave => VerificationMode::Full,
};

// NEW - derive from harbor_id context + plaintext[0] after decryption
// If pulling from topic harbor_id -> use topic key
// If pulling from own endpoint_id -> use DM key
// After decryption, read plaintext[0] to get PacketType
```

Key insight: The recipient KNOWS which harbor_id they're pulling from:
- `harbor_id == hash(topic_id)` → try topic keys → `plaintext[0]` gives PacketType
- `harbor_id == our_endpoint_id` → try DM keys → `plaintext[0]` gives PacketType

### 3.2 Create `core/src/network/process.rs`

New module at network root for unified packet processing:

```rust
use crate::data::PacketType;

fn process_packet(
    packet_type: PacketType,
    payload: &[u8],  // everything after type byte
    sender_id: [u8; 32],
    context: &ProcessContext,
) -> Result<(), ProcessError> {
    match packet_type.scope() {
        Scope::Topic => process_topic_packet(packet_type, payload, sender_id, context),
        Scope::Dm => process_dm_packet(packet_type, payload, sender_id, context),
        Scope::Control => process_control_packet(packet_type, payload, sender_id, context),
        Scope::Reserved => Ok(()), // ignore unknown
    }
}
```

Services import and use `process_packet()`:
```rust
// In HarborService (handles harbor pull)
use crate::network::process::{process_packet, ProcessContext};

// In SendService (handles direct delivery)
use crate::network::process::{process_packet, ProcessContext};
```

Note: `tasks/harbor_pull.rs` uses `HarborService`, not `process_packet()` directly.

### 3.3 Update `core/src/network/mod.rs`

Add `pub mod process;`

### 3.4 Update `core/src/network/send/outgoing.rs`

Use `PacketType` when encoding messages instead of `MessageType` or `DmMessageType`.

## Phase 4: Update Message Encoding

### 4.1 Update `core/src/network/send/topic_messages.rs`

- Remove `MessageType::Join` (0x01) and `MessageType::Leave` (0x02)
- Remove `TopicMessage::Join` and `TopicMessage::Leave` variants
- Update byte values to match new PacketType:
  - Content: 0x00 (same)
  - FileAnnouncement: 0x01 (was 0x03)
  - CanSeed: 0x02 (was 0x04)
  - SyncUpdate: 0x03 (was 0x05)
  - SyncRequest: 0x04 (was 0x06)
  - StreamRequest: 0x05 (was 0x07)

### 4.2 Update `core/src/network/send/dm_messages.rs`

- Update byte values to match new PacketType:
  - DmContent: 0x40 (was 0x80)
  - DmFileAnnounce: 0x41 (was 0x83)
  - DmSyncUpdate: 0x42 (was 0x85)
  - DmSyncRequest: 0x43 (was 0x86)
  - DmStreamRequest: 0x44 (was 0x8C)
  - StreamAccept: 0x50 (was 0x87)
  - StreamReject: 0x51 (was 0x88)
  - StreamQuery: 0x52 (was 0x89)
  - StreamActive: 0x53 (was 0x8A)
  - StreamEnded: 0x54 (was 0x8B)

- Remove `topic_id` from stream signaling payloads (use `request_id` only)

### 4.3 Update `core/src/network/control/protocol.rs`

- Update `ControlPacketType` byte values:
  - ConnectRequest: 0x80 (was 0x01)
  - ConnectAccept: 0x81 (was 0x02)
  - ConnectDecline: 0x82 (was 0x03)
  - TopicInvite: 0x83 (was 0x10)
  - TopicJoin: 0x84 (was 0x11)
  - TopicLeave: 0x85 (was 0x12)
  - RemoveMember: 0x86 (was 0x20)
  - Suggest: 0x87 (was 0x30)

## Phase 5: Update Handlers

### 5.1 Update `core/src/handlers/send.rs`

Use unified `PacketType` for message type detection.

### 5.2 Update `core/src/handlers/control.rs`

Use unified `PacketType` for control message handling.

### 5.3 Update `core/src/network/harbor/incoming.rs`

Remove packet_type handling - harbor just stores opaque bytes.

## Phase 6: Cleanup

### 6.1 Files to potentially remove or significantly simplify

After migration, evaluate whether these can be removed:

1. **Remove `HarborPacketType`** from `harbor/protocol.rs` (done in Phase 2)

2. **Consider consolidating** `topic_messages.rs` and `dm_messages.rs`:
   - Keep payload structs (FileAnnouncementMessage, CanSeedMessage, etc.)
   - Remove `MessageType` and `DmMessageType` enums (replaced by `PacketType`)
   - Keep `TopicMessage` and `DmMessage` enums for high-level message handling
   - But update their encode/decode to use `PacketType` bytes

### 6.2 Update imports across codebase

Search and replace:
- `use crate::network::harbor::protocol::HarborPacketType` → remove
- `use crate::network::send::topic_messages::MessageType` → `use crate::data::PacketType`
- `use crate::network::send::dm_messages::DmMessageType` → `use crate::data::PacketType`
- `use crate::network::control::protocol::ControlPacketType` → `use crate::data::PacketType`

### 6.3 Update tests

- Remove tests for `HarborPacketType`
- Update tests that use old byte values
- Add comprehensive tests for `PacketType`

### 6.4 Database migration

Since we're in dev mode, clear harbor cache:
- Remove `packet_type` column from `harbor_cache` table
- Or just drop and recreate the table

## Phase 7: Verification

### 7.1 Run all tests

```bash
cargo test -p harbor-core
```

### 7.2 Run simulations

```bash
cd simulation
./simulate.sh suites basic
```

### 7.3 Verify no old types remain

```bash
# Should return no results after cleanup
rg "HarborPacketType" core/src/
rg "MessageType::Join" core/src/
rg "MessageType::Leave" core/src/
```

## Execution Order

1. **Phase 1**: Create PacketType (no breaking changes yet)
2. **Phase 4**: Update message encoding (update byte values)
3. **Phase 2**: Update harbor protocol (remove HarborPacketType)
4. **Phase 3**: Update processing logic
5. **Phase 5**: Update handlers
6. **Phase 6**: Cleanup old types
7. **Phase 7**: Verification

## Key Insight: Harbor Opacity

The key simplification is that harbor nodes don't need packet_type at all:
- They store opaque encrypted bytes
- Routing is done by harbor_id (already provided)
- Recipient knows what key to use based on which harbor_id they pulled from
- After decryption, `plaintext[0]` gives the PacketType

This maximizes privacy (harbor learns nothing about message types) and simplifies the protocol.
