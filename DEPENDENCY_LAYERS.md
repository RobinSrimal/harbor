# Dependency Layers

This document maps the current dependency layers in the Harbor workspace so review can proceed layer by layer before testnet.

**Layer Map**
```text
L7 Apps and Integrations
  test-app/src-tauri
  core/src/main.rs (harbor-cli)
  ↓
L6 Public API and Runtime
  core/src/protocol
  core/src/tasks
  core/src/handlers
  core/src/events.rs
  ↓
L5 Network Services (ALPN protocols)
  core/src/network/send
  core/src/network/harbor
  core/src/network/share
  core/src/network/sync
  core/src/network/stream
  core/src/network/control
  core/src/network/dht
  ↓
L4 Network Primitives and Transport Helpers
  core/src/network/connect
  core/src/network/pool
  core/src/network/gate
  core/src/network/rpc
  core/src/network/process
  core/src/network/wire
  core/src/network/packet
  core/src/network/membership
  ↓
L3 Resilience
  core/src/resilience
  ↓
L2 Data and Storage
  core/src/data
  core/src/keychain
  ↓
L1 Security and Crypto
  core/src/security
  ↓
L0 External Crates and OS
  iroh, tokio, rusqlite, crypto crates, OS keychain
```

**Layer Details**

| Layer | Purpose | Key Paths | Depends On |
| --- | --- | --- | --- |
| L7 | Apps, UI, and external integrations | `test-app/src-tauri`, `core/src/main.rs` | `harbor_core` public API |
| L6 | Public API, orchestration, background tasks, incoming handlers | `core/src/protocol`, `core/src/tasks`, `core/src/handlers`, `core/src/events.rs` | L5, L4, L3, L2, L1 |
| L5 | Protocol services (one per ALPN) and service-to-service calls | `core/src/network/send`, `core/src/network/harbor`, `core/src/network/share`, `core/src/network/sync`, `core/src/network/stream`, `core/src/network/control`, `core/src/network/dht` | L4, L3, L2, L1 |
| L4 | Transport helpers and packet processing | `core/src/network/connect`, `core/src/network/pool`, `core/src/network/gate`, `core/src/network/rpc`, `core/src/network/process`, `core/src/network/wire`, `core/src/network/packet`, `core/src/network/membership` | L3, L2, L1 |
| L3 | Abuse resistance (PoW, rate limits, storage quotas) | `core/src/resilience` | L2 |
| L2 | Persistence, identity storage, blobs, keychain integration | `core/src/data`, `core/src/keychain` | L1 |
| L1 | Cryptography and key derivation | `core/src/security` | L0 |
| L0 | External crates and OS services | Cargo dependencies, OS keychain | n/a |

**Known Cross-Layer Couplings**

1. L5 and L4 depend upward on L6 for event and error types. Examples include `core/src/network/process.rs` and multiple `core/src/network/*/service.rs` files that construct `ProtocolEvent` or use `ProtocolError`.
2. L6 re-exports types from L5 and L2 in `core/src/protocol/mod.rs`, so app-facing types originate in lower layers by design.

**Suggested Review Order**

1. L1 Security and Crypto
2. L2 Data and Storage
3. L3 Resilience
4. L4 Network Primitives and Transport Helpers
5. L5 Network Services
6. L6 Public API and Runtime
7. L7 Apps and Integrations
