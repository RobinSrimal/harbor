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

### Build Location: Inside Core

**Decision:** Build sync directly inside `harbor-core`, not as a separate crate.

**Rationale:**
- Sync relies heavily on `Protocol.send()` - same packet/encrypt/replicate pipeline
- Persistence is a data operation that core already handles
- Direct access to `self.db`, `self.event_tx`, internal send methods
- Can auto-process sync messages in incoming handlers
- Single event channel (no separate wiring needed)
- Later: wrap in feature flag once stable

```
┌─────────────────────────────────────────────────────────┐
│                     Application                          │
├─────────────────────────────────────────────────────────┤
│                    harbor-core                           │
│  ┌─────────────────────────────────────────────────┐    │
│  │                    sync/                         │    │
│  │  ┌─────────────┐  ┌─────────┐  ┌─────────────┐  │    │
│  │  │ SyncManager │  │ LoroDoc │  │ SyncMessage │  │    │
│  │  │ (per topic) │  │         │  │ (wire fmt)  │  │    │
│  │  └─────────────┘  └─────────┘  └─────────────┘  │    │
│  └─────────────────────────────────────────────────┘    │
│         Protocol.send() / Protocol.events()              │
└─────────────────────────────────────────────────────────┘
```

### File Structure (in core)

Following core's existing module organization:

```
core/src/
├── data/
│   └── sync/
│       ├── mod.rs           # Re-exports
│       ├── persistence.rs   # WAL + snapshot file storage
│       └── version.rs       # Version vector tracking per member
├── network/
│   └── sync/
│       ├── mod.rs           # Re-exports
│       └── protocol.rs      # SyncMessage wire format
├── handlers/
│   ├── incoming/
│   │   └── sync.rs          # Handle incoming sync messages
│   └── outgoing/
│       └── sync.rs          # Send sync messages
├── tasks/
│   └── sync.rs              # Background: periodic snapshot, compaction
├── protocol/
│   └── sync.rs              # Public API: SyncManager, document ops
└── ...
```

**Follows existing patterns:**
- `data/sync/` - Persistence (like `data/share/`, `data/harbor/`)
- `network/sync/` - Wire format (like `network/share/`, `network/send/`)
- `handlers/incoming/sync.rs` - Process incoming (like `send.rs`, `share.rs`)
- `handlers/outgoing/sync.rs` - Send outgoing (like `send.rs`, `share.rs`)
- `tasks/sync.rs` - Background work (like `harbor_sync.rs`, `share_pull.rs`)
- `protocol/sync.rs` - Public API (like `protocol/share.rs`)

### Key Components

#### 1. `SyncManager`
- One per topic
- Holds the Loro `LoroDoc` instance
- Queues local changes for broadcast
- Applies incoming changes from other peers
- Manages persistence (WAL + snapshots)

#### 2. `LoroDoc` (one per topic)
- Single LoroDoc per topic (not one giant doc for all topics)
- Multiple containers within for different "documents"
- Load/unload as topics become active/inactive

#### 3. `SyncMessage`
- Wire format for sync updates
- Types: `Update`, `StateVector`, `RequestMissing`
- Serialized with postcard (matches core)

---

## Data Model

### Document Structure

**One LoroDoc per topic**, with multiple containers for different shared "documents":

```
Topic A (LoroDoc)
├── "readme"         → Text container (shared README)
├── "tasks"          → List container (shared task list)  
├── "settings"       → Map container (shared config)
├── "meeting-notes"  → Text container (another doc)
└── "files"          → Map container (file metadata index)
```

```rust
// One LoroDoc per topic
let doc = sync_manager.document();

// Multiple "documents" within the topic as containers
let readme = doc.get_text("readme");           // Shared README
let tasks = doc.get_list("tasks");             // Shared task list
let settings = doc.get_map("settings");        // Shared config
let notes = doc.get_text("meeting-notes");     // Another text doc

// Dynamic documents with naming convention
let doc_name = format!("docs/{}", user_provided_name);
let new_doc = doc.get_text(&doc_name);
```

### Why One LoroDoc Per Topic (Not Global)

| Model | Pros | Cons |
|-------|------|------|
| **One LoroDoc per topic** ✅ | Clear isolation; persist independently; load/unload individually; lighter memory | Manage multiple instances |
| **One global LoroDoc** ❌ | Single sync point | Heavy memory; can't unload parts; slow import/export |

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
Local Loro doc updated IMMEDIATELY (responsive UI)
    │
    ▼
Mark dirty, start/reset batch timer
    │
    ▼
[~1 second later, or on explicit flush]
    │
    ▼
Generate delta update (all changes since last send)
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

**Two different granularities:**

| Concern | Granularity | Why |
|---------|-------------|-----|
| **WAL persistence** | Per operation (immediate) | Durability - no data loss on crash |
| **Network sending** | Batched (~1 second) | Efficiency - reduce traffic |

```rust
impl SyncManager {
    dirty: bool,
    last_broadcast_version: VersionVector,
    wal_file: File,
    
    /// Called on every local change (keystroke, paste, etc.)
    fn on_local_change(&mut self, change: Change) {
        // 1. Apply locally immediately (responsive UI)
        self.doc.apply(&change);
        
        // 2. Persist to WAL immediately (DURABILITY - per operation)
        let delta = encode(&change);
        self.wal_file.write_all(&(delta.len() as u32).to_le_bytes())?;
        self.wal_file.write_all(&delta)?;
        self.wal_file.sync_data()?;  // Flush to disk NOW
        
        // 3. Mark for batched network send (EFFICIENCY - debounced)
        self.dirty = true;
        self.schedule_flush();  // Will send in ~1 second
    }
    
    /// Called by debounce timer (~1 second)
    fn flush(&mut self) {
        if !self.dirty { return; }
        
        // Export all changes since last broadcast as ONE network packet
        let batch = self.doc.export_from(&self.last_broadcast_version);
        self.broadcast(SyncMessage::Update { data: batch });
        
        self.last_broadcast_version = self.doc.oplog_version();
        self.dirty = false;
    }
}
```

**Crash scenario:**
```
Type "Hello World" (11 keystrokes over 0.8 seconds)
    ↓
Each keystroke → WAL write (immediate)
    ↓
Crash at 0.8s (before network batch at 1s)
    ↓
WAL has all 11 ops → recovered on restart ✅
    ↓
After recovery, flush sends all 11 to peers
```

**Why batch network but not WAL?**
- WAL: Disk write is fast, data loss is unacceptable
- Network: Sending is expensive, ~1s latency is acceptable for collab editing

**Key insight:** Harbor's replication handles offline delivery automatically. No explicit reconnection sync needed between existing members.

#### New Member Joins (Direct Connection)

```
User D joins topic
    │
    ▼
Send InitialSyncRequest via Send protocol (small, just version vector)
    │
    ▼
Existing member receives request
    │
    ▼
Opens DIRECT connection to User D (not via Send/Harbor)
    │
    ▼
Streams full snapshot over direct connection (no size limit)
    │
    ▼
User D applies snapshot, now in sync
    │
    ▼
Future UPDATES arrive via Send protocol (small deltas)
```

**Why direct connection for initial sync:**
- Full snapshots can exceed 512KB (Send limit)
- No need for Harbor Nodes to cache initial sync (it's point-to-point)
- Direct connection has no size limit
- Similar pattern to Share protocol for large files

**Why Send protocol for updates:**
- Deltas are small (typically < 1KB)
- Need offline delivery via Harbor Nodes
- Need broadcast to all members

### Wire Format

```rust
// Prefix byte for sync messages (via Send protocol)
const SYNC_MESSAGE_PREFIX: u8 = 0x53; // 'S'

// Messages via Send protocol (small, need offline delivery)
enum SyncMessage {
    Update { data: Vec<u8> },                 // Delta update (small)
    InitialSyncRequest { version: Vec<u8> },  // Request (tiny)
}

// Messages via Direct connection (large, point-to-point)
enum DirectSyncMessage {
    InitialSyncResponse { snapshot: Vec<u8> },  // Full state (large)
}
```

**Two transport modes:**

| Message | Transport | Why |
|---------|-----------|-----|
| `Update` | Send protocol | Small deltas, needs offline delivery via Harbor |
| `InitialSyncRequest` | Send protocol | Tiny, needs broadcast to find responders |
| `InitialSyncResponse` | **Direct connection** | Large snapshot, point-to-point only |

**Integration with Core:**
- Small messages via `Protocol.send()` (with prefix byte)
- Large initial sync via direct QUIC connection (like Share protocol)
- Core's Harbor Nodes only cache the small Update messages

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

**Add to core:**
```
core/src/
├── data/sync/mod.rs, persistence.rs
├── network/sync/mod.rs, protocol.rs
├── handlers/incoming/sync.rs
├── handlers/outgoing/sync.rs
├── protocol/sync.rs
```

**Tasks:**
1. Add `loro` dependency to `core/Cargo.toml`
2. Implement `SyncMessage` enum in `network/sync/protocol.rs`
3. Implement persistence (WAL + snapshots) in `data/sync/`
4. Implement incoming handler to auto-apply sync messages
5. Implement `SyncManager` public API in `protocol/sync.rs`
6. Add integration tests with two in-memory nodes

### Phase 2: Sync Protocol

**Tasks:**
1. Implement delta broadcasting on local changes
2. Implement initial sync request/response flow
3. Handle Harbor pull with ack-after-persist safety
4. Add version tracking for debugging
5. Test out-of-order update handling

### Phase 3: Durability & Polish

**Tasks:**
1. Implement WAL compaction (debounced snapshots)
2. Add crash recovery (snapshot + WAL replay)
3. Add history management (shallow snapshots when all caught up)
4. Add change subscription API (SyncEvent)
5. Performance optimization (batch updates)
6. Documentation and examples

### Phase 4: Feature Flag (Later)

Once stable, wrap in optional feature:
```toml
[features]
default = []
sync = ["loro"]
```

---

## Core Integration

### Uses Existing Core Infrastructure

Since sync is built INTO core, it has direct access to:

| Capability | Internal Access | Usage in Sync |
|------------|-----------------|---------------|
| Send messages | `send_raw_with_info()` | Broadcast updates |
| Receive messages | `event_tx` channel | Emit sync events |
| Topic membership | `data::membership` | Access control |
| Offline delivery | Harbor Nodes | Sync after disconnect |
| Database | `self.db` | Version tracking (optional) |
| Config | `self.config` | Sync path configuration |

### Auto-Processing Sync Messages

Incoming handler detects sync messages by prefix byte and processes automatically:

```rust
// In handlers/incoming/send.rs (or sync.rs)
fn handle_packet(&self, packet: Packet) {
    let payload = decrypt(packet)?;
    
    if payload.starts_with(&[SYNC_MESSAGE_PREFIX]) {
        // Route to sync handler
        self.handle_sync_message(&packet.topic_id, &payload[1..])?;
    } else {
        // Regular app message - emit to event channel
        self.event_tx.send(ProtocolEvent::Message(...))?;
    }
}
```

---

## Persistence

### Storage: File-Based (Not SQLite)

**Decision:** Use separate files per topic, not SQLite.

**Rationale:**
- Loro docs are opaque binary blobs (can't query with SQL anyway)
- Blobs in SQLite are awkward; files are natural fit
- Can memory-map if needed
- Easy to inspect/debug
- Matches existing `blob_path` pattern for Share

### File Structure

```
.harbor_sync/
├── {topic_id_hex}.loro      # Periodic snapshot
├── {topic_id_hex}.wal       # Write-ahead log (deltas)
└── ...
```

### Two-Tier Persistence: WAL + Snapshots

**Problem:** If app crashes between changes and periodic snapshot, data is lost.

**Solution:** Write-ahead log (WAL) pattern.

| Type | When | Size | Purpose |
|------|------|------|---------|
| **Delta (WAL)** | Every change | Tiny | Durability |
| **Snapshot** | Periodic (30s) / on close | Larger | Fast recovery |

```rust
impl SyncManager {
    /// On EVERY local change - append delta to WAL (fast, append-only)
    fn on_local_change(&mut self) -> Result<()> {
        let delta = self.doc.export_from(&self.last_version);
        
        // 1. Append to WAL immediately (durability)
        self.wal_file.write_all(&(delta.len() as u32).to_le_bytes())?;
        self.wal_file.write_all(&delta)?;
        self.wal_file.sync_data()?;  // Flush to disk
        
        self.last_version = self.doc.oplog_version();
        
        // 2. Broadcast to peers
        self.broadcast(SyncMessage::Update { data: delta }).await?;
        
        // 3. Mark dirty, schedule debounced snapshot
        self.dirty = true;
        self.schedule_snapshot();  // Will snapshot after 5s of no changes
        
        Ok(())
    }
    
    /// Periodic compaction (debounced, or on clean close)
    fn compact(&mut self) -> Result<()> {
        // Write full snapshot
        fs::write(&self.snapshot_path, self.doc.export_snapshot())?;
        
        // Clear WAL (all changes now in snapshot)
        fs::write(&self.wal_path, [])?;
        self.dirty = false;
        
        Ok(())
    }
    
    /// On startup - recover from crash
    fn load(&mut self) -> Result<()> {
        // 1. Load last snapshot
        if self.snapshot_path.exists() {
            self.doc.import(&fs::read(&self.snapshot_path)?)?;
        }
        
        // 2. Replay WAL (deltas since snapshot)
        if self.wal_path.exists() {
            let wal = fs::read(&self.wal_path)?;
            let mut cursor = 0;
            while cursor < wal.len() {
                let len = u32::from_le_bytes(wal[cursor..cursor+4].try_into()?) as usize;
                cursor += 4;
                let delta = &wal[cursor..cursor+len];
                self.doc.import(delta)?;  // Loro dedupes if already applied
                cursor += len;
            }
        }
        
        // 3. Compact now that we've recovered
        self.compact()?;
        
        Ok(())
    }
}
```

### Crash Recovery Matrix

| Failure Point | Outcome |
|--------------|---------|
| Crash before WAL write | Change lost (not yet persisted) |
| Crash after WAL, before doc import | WAL replay recovers it ✅ |
| Crash after import, before snapshot | WAL replay recovers it ✅ |
| Crash during compaction | Snapshot might be partial, WAL has everything ✅ |
| Clean close after compact | Fully persisted ✅ |

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

### Out-of-Order Updates (Before Initial Sync)

**Question:** Can a new member store updates that arrive before initial sync completes?

**Answer:** Yes! Loro's OpLog can store operations even if causal dependencies aren't met yet.

```
User D joins topic
    │
    ▼
Sends InitialSyncRequest
    │
    ├──► Meanwhile, receives Update messages
    │    (stores in OpLog, doc state incomplete)
    │
    ▼
InitialSyncResponse arrives (full snapshot)
    │
    ▼
Loro reconciles: buffered updates now apply
    │
    ▼
Document is consistent and readable
```

**Key insight:** You don't need to wait for initial sync before accepting updates. They can arrive, be persisted to WAL, and "activate" once the snapshot arrives. No data loss from racing.

### Harbor Pull Safety (Ack After Persist)

**Problem:** If app crashes after pulling from Harbor but before persisting locally, the update is lost forever (Harbor already deleted it).

**Solution:** Only acknowledge Harbor delivery AFTER writing to local WAL.

```rust
impl SyncManager {
    async fn handle_harbor_pull(&mut self, packets: Vec<Packet>) -> Result<()> {
        for packet in packets {
            // 1. Write to WAL FIRST (before ack)
            self.append_to_wal(&packet.payload)?;
            
            // 2. Apply to doc
            self.doc.import(&packet.payload)?;
            
            // 3. NOW send ack to Harbor (safe to delete)
            self.protocol.acknowledge_harbor_delivery(&packet.id).await?;
        }
        Ok(())
    }
}
```

**Crash recovery:**
- If crash before ack → Harbor re-sends on next pull
- Loro handles duplicate imports gracefully (idempotent)

**Principle:** Ack is the LAST step, after local persistence is guaranteed.

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

## History Management

### The Problem: Document Growth

Loro documents grow over time as operations accumulate. Without management:
- Snapshots get larger
- Initial sync takes longer
- Storage grows unbounded

### Loro's Tools

| Feature | Purpose |
|---------|---------|
| `export_snapshot()` | Full state + history |
| `export_shallow_snapshot(&version)` | State without history before version |
| `oplog_version()` | Current version vector |
| Garbage collection | Prune old operations |

### Compaction Strategy: Aggressive + Harbor

**Key insight:** Harbor Nodes handle offline sync, not your local doc history.

```
Local LoroDoc history → Only for YOUR undo/time-travel
Harbor Node cache     → For OTHER members' offline sync (90 days)
```

**Therefore:** Compact aggressively. Use shallow snapshots by default.

```rust
impl SyncManager {
    /// Save snapshot - always use shallow (compact)
    fn save_snapshot(&mut self) -> Result<()> {
        // Use shallow snapshot - discard old history
        let snapshot = self.doc.export_shallow_snapshot(&self.doc.oplog_version());
        fs::write(&self.snapshot_path, snapshot)?;
        
        // Clear WAL
        fs::write(&self.wal_path, [])?;
        
        Ok(())
    }
}
```

### Why This Works

| Scenario | How It's Handled |
|----------|------------------|
| Member offline < 90 days | Pulls updates from Harbor Nodes |
| Member offline > 90 days | Gets initial sync (full snapshot from active member) |
| New member joins | Gets initial sync (full snapshot) |
| You want undo/time-travel | Keep full history locally (opt-in, not default) |

**No version tracking needed.** No "is everyone caught up" logic. Simple.

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
# Add to core/Cargo.toml
[dependencies]
# ... existing deps ...
loro = "1.0"  # Check latest version - adds ~2MB to binary
```

**Note:** Since sync is built into core, no separate crate. Loro is the only new dependency.

---

## Open Questions

### Resolved

1. ~~**Should sync state persist in Core's SQLite or separate files?**~~
   - **Decision:** Separate files (`.harbor_sync/{topic_id}.loro` + `.wal`)
   - Rationale: Loro blobs are opaque, can't query with SQL, files are simpler

2. ~~**Should SyncManager auto-subscribe to events or require manual wiring?**~~
   - **Decision:** Auto - handlers/incoming/sync.rs processes sync messages automatically
   - Rationale: Built into core, so natural to auto-handle

### Still Open

1. ~~**How to handle very large initial sync (>512KB)?**~~
   - **Decision:** Initial sync uses direct P2P connection (like Share protocol), not Send
   - Send (512KB limit) is only for incremental updates (which are small deltas)
   - Direct connection has no size limit
   - No need to go through Harbor Nodes for initial sync (it's point-to-point)

2. ~~**Undo/redo support?**~~
   - **Decision:** Not supported at protocol level
   - Apps can implement their own undo via command pattern (track ops, generate inverses)
   - Keeps sync layer simple and avoids multi-user undo conflicts

3. ~~**Who responds to InitialSyncRequest?**~~
   - **Decision:** Any member can respond, accept multiple (up to a limit)
   - Members may be at different states - merging multiple responses is fine (Loro handles it)
   - Set upper limit (e.g., 3 responses) to avoid processing overhead
   - After limit reached, ignore further responses for that request
   ```rust
   const MAX_INITIAL_SYNC_RESPONSES: usize = 3;
   
   struct PendingInitialSync {
       request_id: [u8; 32],
       responses_received: usize,
   }
   
   fn handle_initial_sync_response(&mut self, response: InitialSyncResponse) {
       if self.pending.responses_received >= MAX_INITIAL_SYNC_RESPONSES {
           return; // Ignore, we have enough
       }
       self.doc.import(&response.snapshot)?;  // Loro merges automatically
       self.pending.responses_received += 1;
   }
   ```

4. ~~**When to trigger compaction?**~~
   - **Decision:** Compact aggressively (e.g., on every snapshot save)
   - Harbor Nodes cache updates for 90 days - that's the offline buffer
   - Local doc compaction doesn't affect others' ability to sync
   - Returning members pull from Harbor, not from your local history
   - Use shallow snapshots by default - full history only if you want local undo/time-travel

5. ~~**Should WAL be per-topic or global?**~~
   - **Decision:** Per-topic WAL
   - Simpler isolation, matches snapshot files
   - Easy cleanup when leaving topic (delete both files)
   - Matches "one LoroDoc per topic" pattern

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

