# Harbor Sync - Implementation Proposal

A CRDT synchronization layer for Harbor, enabling collaborative data structures per topic.

## Goal

Allow topic members to collaboratively edit shared documents that automatically merge without conflicts, even when members are offline.

---

## CRDT Library Evaluation

### Loro (Recommended)

[Loro](https://github.com/loro-dev/loro) is a high-performance CRDT library in Rust.

**Pros:**
- Native Rust with excellent performance
- Rich type support: Text, List, Map, Tree, Counter, Movable List
- Efficient binary encoding (compact wire format)
- Built-in version vectors for causality
- Delta sync (transmit only changes, not full state)
- Time travel / undo-redo support
- Actively maintained (2024-2025)
- Good documentation

**Cons:**
- Younger than Automerge (less battle-tested)
- Smaller community

### Alternatives Considered

| Library | Language | Types | Notes |
|---------|----------|-------|-------|
| **Automerge** | Rust | Text, List, Map, Table | Mature, heavier, slower sync |
| **y-crdt** | Rust (Yjs port) | Text, Array, Map | Good for text, less Rust-native |
| **diamond-types** | Rust | Text only | Fastest for text, too limited |

### Recommendation

**Use Loro.** It's the best fit for a Rust-native protocol with diverse data type needs. The performance and delta-sync capabilities align well with Harbor's packet-based messaging.

---

## Architecture

### Overview

```
┌─────────────────────────────────────────────────────────┐
│                     Application                          │
├─────────────────────────────────────────────────────────┤
│                    harbor-sync                           │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐  │
│  │ SyncManager │  │ SyncDoc     │  │ SyncMessage     │  │
│  │ (per topic) │  │ (Loro doc)  │  │ (wire format)   │  │
│  └─────────────┘  └─────────────┘  └─────────────────┘  │
├─────────────────────────────────────────────────────────┤
│                    harbor-core                           │
│         Protocol.send() / Protocol.events()              │
└─────────────────────────────────────────────────────────┘
```

### Key Components

#### 1. `SyncManager`
- One per topic
- Holds the Loro `LoroDoc` instance
- Queues local changes for broadcast
- Applies incoming changes from other peers

#### 2. `SyncDoc`
- Wrapper around `LoroDoc` with Harbor-specific functionality
- Tracks local vs. synced state
- Provides typed accessors for common patterns

#### 3. `SyncMessage`
- Wire format for sync updates
- Types: `Update`, `StateVector`, `RequestMissing`
- Serialized with postcard (matches core)

---

## Data Model

### Document Structure

Each topic can have **one sync document** with multiple containers:

```rust
// Application defines document schema
let doc = sync_manager.document();

// Access typed containers
let text = doc.get_text("content");      // Collaborative text
let items = doc.get_list("items");       // Ordered list
let meta = doc.get_map("metadata");      // Key-value pairs
let tree = doc.get_tree("hierarchy");    // Tree structure
```

### Loro Container Types

| Type | Use Case | Operations |
|------|----------|------------|
| `Text` | Rich text editing | insert, delete, mark |
| `List` | Ordered collections | insert, delete, get, move |
| `Map` | Key-value storage | set, delete, get |
| `Tree` | Hierarchies | create_node, move, delete |
| `Counter` | Numeric counters | increment, decrement |
| `MovableList` | Reorderable lists | move operations |

---

## Sync Protocol

### Message Types

```rust
enum SyncMessage {
    /// Delta update (encoded Loro changes)
    /// Broadcast to all members via Protocol.send()
    Update {
        data: Vec<u8>,  // Loro encoded update
    },
    
    /// Initial sync request from new member
    /// Sent when joining a topic
    InitialSyncRequest {
        state_vector: Vec<u8>,  // Loro encoded state vector (empty for new member)
    },
    
    /// Response to initial sync request
    /// Contains full document state or missing updates
    InitialSyncResponse {
        snapshot: Vec<u8>,  // Loro encoded snapshot
    },
}
```

### Sync Flow

#### Real-time (Online)

```
User A makes edit
    │
    ▼
Local Loro doc updated
    │
    ▼
Generate delta update
    │
    ▼
Wrap in SyncMessage::Update
    │
    ▼
Send via Protocol.send()
    │
    ├──► User B online: receives immediately
    │
    └──► User C offline: Harbor Nodes cache it
                │
                ▼
         User C comes online
                │
                ▼
         Harbor Nodes deliver cached updates
                │
                ▼
         User C applies, now in sync
```

**Key insight:** Harbor's replication handles offline delivery automatically. No explicit reconnection sync needed between existing members.

#### New Member Joins

```
User D joins topic
    │
    ▼
Send SyncMessage::InitialSyncRequest
    │
    ▼
Existing member responds with InitialSyncResponse
    │
    ▼
User D applies snapshot, now in sync
    │
    ▼
Future updates arrive via normal flow
```

**Why this is needed:** Harbor Nodes may not have complete history. New members need the full document state, not just recent updates.

### Wire Format

```rust
// Prefix byte for sync messages (to distinguish from regular content)
const SYNC_MESSAGE_PREFIX: u8 = 0x53; // 'S'

// Full payload structure
struct SyncPayload {
    prefix: u8,           // 0x53
    message: SyncMessage, // postcard encoded
}
```

**Integration with Core:**
- Sync messages are sent as regular topic messages via `Protocol.send()`
- The prefix byte distinguishes sync from application content
- Core's Harbor Nodes handle offline delivery automatically

---

## API Design

### Initialization

```rust
use harbor_core::Protocol;
use harbor_sync::SyncManager;

// Start core protocol
let protocol = Protocol::start(config).await?;

// Create sync manager for a topic
let sync = SyncManager::new(&protocol, &topic_id).await?;
```

### Document Operations

```rust
// Get document
let doc = sync.document();

// Text editing
let text = doc.get_text("content");
text.insert(0, "Hello ")?;
text.insert(6, "World")?;

// List operations
let items = doc.get_list("todos");
items.push("Buy groceries")?;
items.insert(0, "First item")?;

// Map operations  
let meta = doc.get_map("settings");
meta.set("theme", "dark")?;
meta.set("fontSize", 14)?;

// Commit changes (broadcasts to other members)
sync.commit().await?;
```

### Receiving Updates

```rust
// Option 1: Automatic (recommended)
// SyncManager subscribes to protocol.events() internally
// and auto-applies sync messages

// Option 2: Manual processing
let mut events = protocol.events().await.unwrap();
while let Some(msg) = events.recv().await {
    if sync.is_sync_message(&msg.payload) {
        sync.handle_message(&msg).await?;
    } else {
        // Regular application message
    }
}
```

### Change Notifications

```rust
// Subscribe to local + remote changes
let mut changes = sync.subscribe();
while let Some(change) = changes.recv().await {
    match change {
        SyncChange::Text { path, .. } => { /* text changed */ }
        SyncChange::List { path, .. } => { /* list changed */ }
        SyncChange::Map { path, .. } => { /* map changed */ }
    }
}
```

---

## Implementation Plan

### Phase 1: Foundation

**Files to create:**
```
sync/src/
├── lib.rs           # Public API exports
├── manager.rs       # SyncManager implementation
├── document.rs      # SyncDoc wrapper
├── message.rs       # SyncMessage types
└── error.rs         # Error types
```

**Tasks:**
1. Add `loro` dependency to `sync/Cargo.toml`
2. Implement `SyncMessage` enum with postcard serialization
3. Implement `SyncDoc` wrapper around `LoroDoc`
4. Implement `SyncManager` with basic sync flow
5. Add integration tests with two in-memory nodes

### Phase 2: Sync Protocol

**Tasks:**
1. Implement delta broadcasting on commit
2. Implement state vector exchange on reconnection
3. Handle concurrent edits (Loro handles merge automatically)
4. Add version tracking for debugging

### Phase 3: Polish

**Tasks:**
1. Add typed container accessors
2. Add change subscription API
3. Add persistence (save/load document state)
4. Performance optimization (batch updates)
5. Documentation and examples

---

## Core Dependencies

### Required from harbor-core

Harbor Sync uses these existing Core capabilities:

| Capability | Core API | Usage in Sync |
|------------|----------|---------------|
| Send messages | `Protocol.send()` | Broadcast updates |
| Receive messages | `Protocol.events()` | Receive updates |
| Topic membership | `TopicInvite` | Access control |
| Offline delivery | Harbor Nodes | Sync after disconnect |

### No Core Changes Required

The current Core API is sufficient. Sync is purely additive.

**Why this works:**
- Core's Send mode handles message delivery
- Harbor Nodes handle offline scenarios
- Topic encryption ensures only members can sync
- No special "sync mode" needed in Core

---

## Persistence

### Document State Storage

```rust
// Save document state
let snapshot = sync.document().export_snapshot();
save_to_disk(&topic_id, &snapshot)?;

// Load on startup
let snapshot = load_from_disk(&topic_id)?;
sync.document().import_snapshot(&snapshot)?;
```

**Storage location options:**
1. Separate file per topic (`sync_docs/{topic_id}.loro`)
2. SQLite table in Core's database (requires Core change)
3. Application-managed storage

**Recommendation:** Option 1 (separate files) for Phase 1. No Core changes needed.

---

## Edge Cases

### New Member (Initial Sync)

When a new member joins a topic:
1. New member sends `InitialSyncRequest` (state vector is empty or from stale cache)
2. Any existing member can respond with `InitialSyncResponse` containing full snapshot
3. New member applies snapshot and is now in sync
4. Future updates arrive via normal Harbor replication

**Note:** Only ONE member needs to respond. First response wins, duplicates are ignored (Loro handles idempotent applies).

### Returning Member (After Offline)

When an existing member comes back online:
1. Harbor Nodes automatically deliver any cached updates
2. No explicit sync request needed
3. Loro applies updates, member is in sync

**This is simpler than typical CRDTs** because Harbor Nodes act as reliable message buffers.

### Conflicting Edits

Handled automatically by Loro's CRDT algorithms:
- Text: Character-by-character merge
- List: Interleave concurrent inserts
- Map: Last-writer-wins per key (with causal ordering)

### Large Documents

For documents > 512KB (Core's packet limit):
- Loro's delta sync usually keeps updates small
- Full state sync may need chunking
- Consider: Add chunked transfer in Phase 3 if needed

---

## Testing Strategy

### Unit Tests

```rust
#[test]
fn test_concurrent_text_edits() {
    let doc1 = SyncDoc::new();
    let doc2 = SyncDoc::new();
    
    // Simulate concurrent edits
    doc1.get_text("content").insert(0, "Hello");
    doc2.get_text("content").insert(0, "World");
    
    // Merge
    let update1 = doc1.export_updates();
    let update2 = doc2.export_updates();
    doc1.import_updates(&update2);
    doc2.import_updates(&update1);
    
    // Both should converge
    assert_eq!(doc1.get_text("content").to_string(), 
               doc2.get_text("content").to_string());
}
```

### Integration Tests

Use `harbor_core::testing` module for multi-node simulation:

```rust
#[tokio::test]
async fn test_sync_between_nodes() {
    let mut network = TestNetwork::new();
    let node_a = network.add_node().await;
    let node_b = network.add_node().await;
    
    // Create topic and sync managers
    let topic = node_a.create_topic().await;
    node_b.join_topic(&topic).await;
    
    let sync_a = SyncManager::new(&node_a.protocol, &topic.topic_id).await;
    let sync_b = SyncManager::new(&node_b.protocol, &topic.topic_id).await;
    
    // Edit on A
    sync_a.document().get_text("doc").insert(0, "Hello");
    sync_a.commit().await;  // Broadcasts Update via Protocol.send()
    
    // Wait for delivery
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // Verify B received it
    assert_eq!(sync_b.document().get_text("doc").to_string(), "Hello");
}

#[tokio::test]
async fn test_new_member_initial_sync() {
    let mut network = TestNetwork::new();
    let node_a = network.add_node().await;
    
    // A creates topic and makes edits
    let topic = node_a.create_topic().await;
    let sync_a = SyncManager::new(&node_a.protocol, &topic.topic_id).await;
    sync_a.document().get_text("doc").insert(0, "Existing content");
    sync_a.commit().await;
    
    // B joins later - needs initial sync
    let node_b = network.add_node().await;
    node_b.join_topic(&topic).await;
    let sync_b = SyncManager::new(&node_b.protocol, &topic.topic_id).await;
    
    // B requests initial sync (happens automatically in SyncManager::new)
    sync_b.request_initial_sync().await;
    
    // Wait for response
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // B should have full document
    assert_eq!(sync_b.document().get_text("doc").to_string(), "Existing content");
}
```

---

## Dependencies

```toml
# sync/Cargo.toml
[dependencies]
harbor-core = { version = "0.1", path = "../core" }
loro = "1.0"  # Check latest version
postcard = { version = "1.0", features = ["alloc"] }
tokio = { version = "1", features = ["sync"] }
thiserror = "1.0"
```

---

## Open Questions

1. **Should sync state persist in Core's SQLite or separate files?**
   - Separate files = simpler, no Core changes
   - SQLite = atomic with other data, but requires Core schema change

2. **How to handle very large initial sync (>512KB)?**
   - Chunked transfer protocol
   - Or: Accept limitation, documents must stay under limit
   - Note: Regular updates are deltas, so usually small

3. **Should SyncManager auto-subscribe to events or require manual wiring?**
   - Auto = simpler API
   - Manual = more flexible, app controls event routing

4. **Undo/redo support?**
   - Loro supports it natively
   - Should we expose it in Phase 1 or defer?

5. **Who responds to InitialSyncRequest?**
   - Any member? First responder wins?
   - Designated "sync leader" per topic?
   - Or: broadcast request, accept first response

---

## Success Criteria

Phase 1 is complete when:
- [ ] Two nodes can create a shared document
- [ ] Edits on one node appear on the other (via Harbor replication)
- [ ] New member can request and receive full document state
- [ ] Offline member receives updates when returning (via Harbor Nodes)
- [ ] Concurrent edits merge without data loss
- [ ] Integration tests pass reliably

---

## References

- [Loro Documentation](https://loro.dev/docs)
- [Loro GitHub](https://github.com/loro-dev/loro)
- [CRDT Primer](https://crdt.tech/)
- [Harbor Core README](../core/README.md)

