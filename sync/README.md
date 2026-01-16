# Harbor Sync

CRDT synchronization for Harbor - collaborative data structures per topic.

## Status

🚧 **Planned** - This crate is not yet implemented.

## Overview

Harbor Sync will provide CRDT (Conflict-free Replicated Data Types) synchronization built on top of Harbor Core. It enables collaborative data structures that automatically merge changes from multiple peers without conflicts.

## Planned Features

- Per-topic CRDT documents
- Automatic sync over Harbor Core's Send protocol
- Support for common CRDT types (text, lists, maps, counters)
- Offline-first with automatic reconciliation
- Built on [Loro](https://github.com/loro-dev/loro) CRDT library

## Usage (Future)

```rust
use harbor_core::{Protocol, ProtocolConfig};
use harbor_sync::SyncDocument;

// Start core protocol
let protocol = Protocol::start(ProtocolConfig::default()).await?;

// Create a synced document for a topic
let doc = SyncDocument::new(&protocol, &topic_id).await?;

// Make changes - automatically synced to other peers
doc.text("content").insert(0, "Hello, world!");
```

## Dependencies

- `harbor-core`: Core messaging protocol (required)

