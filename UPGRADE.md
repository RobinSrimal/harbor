# Iroh Upgrade Guide: 0.93 → 0.95.1

> **Status: COMPLETED** - Upgrade performed successfully on 2026-01-26
> - All 679 tests pass
> - Code compiles without errors

This document outlines the breaking changes and required code modifications when upgrading from iroh 0.93 to 0.95.1.

## Summary

| Category | Impact | Files Affected |
|----------|--------|----------------|
| Type Renames (NodeId → EndpointId) | **High** | 19 files, 71 occurrences |
| Type Renames (NodeAddr → EndpointAddr) | **High** | 9 files, 29 occurrences |
| API Changes (direct_addresses) | **Medium** | 5 occurrences in DHT code |
| Error Type Changes | **Low** | No current usage found |
| ed25519_dalek Changes | **Low** | 2 files (independent usage) |

---

## Breaking Changes by Version

### v0.94.0 Breaking Changes

#### 1. Major Type Renames

All "Node" terminology has been renamed to "Endpoint":

| Old Name | New Name |
|----------|----------|
| `NodeId` | `EndpointId` |
| `NodeAddr` | `EndpointAddr` |
| `NodeTicket` | `EndpointTicket` |
| `RelayNode` | `RelayConfig` |

**Harbor Impact**: 71 occurrences of `NodeId` and 29 occurrences of `NodeAddr` across the codebase.

**Affected Files**:
- `core/src/network/dht/internal/pool.rs` (12 NodeId, 4 NodeAddr)
- `core/src/network/dht/internal/bootstrap.rs` (9 NodeId)
- `core/src/network/pool.rs` (7 NodeId, 3 NodeAddr)
- `core/src/network/send/outgoing.rs` (5 NodeId, 8 NodeAddr)
- `core/src/handlers/outgoing/share.rs` (6 NodeId, 4 NodeAddr)
- `core/src/handlers/outgoing/sync.rs` (5 NodeId, 2 NodeAddr)
- `core/src/handlers/outgoing/harbor.rs` (5 NodeId)
- `core/src/network/dht/actor.rs` (4 NodeId)
- `core/src/network/share/service.rs` (3 NodeId, 2 NodeAddr)
- `core/src/network/harbor/incoming.rs` (3 NodeId)
- And 9 more files...

#### 2. Method Renames on Endpoint

| Old Method | New Method |
|------------|------------|
| `Endpoint::node_addr()` | `Endpoint::addr()` |
| `Endpoint::watch_node_addr()` | `Endpoint::watch_addr()` |
| `Endpoint::node_id()` | `Endpoint::id()` |

**Action**: Search for these method calls and rename them.

#### 3. EndpointAddr Structure Changes

The structure of `EndpointAddr` (formerly `NodeAddr`) has changed:

```rust
// Old (0.93)
addr.direct_addresses  // Direct field access
addr.with_direct_addresses([socket_addr])

// New (0.95)
addr.ip_addrs()  // Method call
addr.relay_urls()  // Method call for relay URLs
```

**Harbor Impact**: Found in DHT code:
- `core/src/network/dht/protocol.rs:76` - `addr.direct_addresses.iter()`
- `core/src/network/dht/internal/pool.rs:102` - `addr.with_direct_addresses([socket_addr])`
- `core/src/network/dht/internal/pool.rs:234,245,393` - Test assertions on `direct_addresses`

#### 4. Removed APIs

| Removed | Replacement |
|---------|-------------|
| `Endpoint::add_node_addr_with_source()` | Use a discovery service |
| `Into`/`From` for `PublicKey`/`SecretKey` (ed25519_dalek) | Use `iroh_base::Signature` |

**Harbor Impact**: No direct usage of `add_node_addr_with_source()` found.

#### 5. New APIs Available

- `Endpoint::insert_relay()` and `Endpoint::remove_relay()`
- `iroh_base::Signature` type

---

### v0.95.0 Breaking Changes

#### 1. Error Handling Migration

Error types have been refactored from `snafu` to `n0-error` framework:

- `ConnectError::Connection` - field structure changed
- `AcceptError::Connection`, `::MissingRemoteEndpointId`, `::NotAllowed`, `::User` - all fields modified

**Harbor Impact**: No current usage of these error types found. Low risk.

#### 2. Protocol Handler Changes

| Removed | Replacement |
|---------|-------------|
| `ProtocolHandler::on_connecting()` | `on_accepting(Accepting)` |
| `DynProtocolHandler::on_connecting()` | `on_accepting(Accepting)` |
| `iroh::endpoint::IncomingFuture` | `Accepting` type |
| `iroh::endpoint::ZeroRttAccepted` | Explicit 0-RTT connection types |

**Harbor Impact**: No usage of `ProtocolHandler` or `on_connecting` found. Low risk.

#### 3. Method Signature Changes

```rust
// Old
fn into_0rtt(self) -> Result<(Connection, ZeroRttAccepted), Self>;

// New
fn into_0rtt(self) -> Result<OutgoingZeroRttConnection, Connecting>;
```

#### 4. Infallible Methods (Simplification)

These methods no longer return `Result`:
- `Connection::remote_id` - now infallible
- `Connection::alpn` - now infallible

**Action**: Remove `.unwrap()` or `?` from these calls if present.

---

### v0.95.1 Changes

Patch release for `iroh-base`. No additional breaking changes.

---

## Database Migration Warning

**Important**: If using `iroh-dns-server`, you must run version 0.93 at least once before upgrading to 0.95.0 due to redb database upgrade (v2 → v3).

---

## Migration Checklist

### Phase 1: Update Dependencies

```toml
# Cargo.toml
iroh = "0.95"
iroh-base = "0.95"
```

### Phase 2: Type Renames (Automated)

These can be done with find-and-replace:

- [ ] `NodeId` → `EndpointId` (71 occurrences, 19 files)
- [ ] `NodeAddr` → `EndpointAddr` (29 occurrences, 9 files)
- [ ] `use iroh::NodeId` → `use iroh::EndpointId`
- [ ] `use iroh::NodeAddr` → `use iroh::EndpointAddr`

### Phase 3: Method Renames

- [ ] `.node_id()` → `.id()`
- [ ] `.node_addr()` → `.addr()`
- [ ] `.watch_node_addr()` → `.watch_addr()`

### Phase 4: EndpointAddr API Changes

Update DHT code to use new accessor methods:

- [ ] `core/src/network/dht/protocol.rs:76`
  ```rust
  // Old
  addr.direct_addresses.iter()
  // New
  addr.ip_addrs()
  ```

- [ ] `core/src/network/dht/internal/pool.rs:102`
  ```rust
  // Old
  addr.with_direct_addresses([socket_addr])
  // New - check new API for adding addresses
  ```

- [ ] Update test assertions in `pool.rs` (lines 234, 245, 393)

### Phase 5: Error Handling (if applicable)

- [ ] Update any error matching on `ConnectError` or `AcceptError`

### Phase 6: Infallible Methods

- [ ] Remove error handling from `Connection::remote_id` calls
- [ ] Remove error handling from `Connection::alpn` calls

---

## Potential Issues & Risks

### High Risk

1. **Semantic meaning of EndpointId vs NodeId**: Ensure all internal type aliases and documentation are updated consistently. The codebase uses `NodeId` extensively for peer identification.

2. **EndpointAddr structure changes**: The DHT code directly accesses `direct_addresses`. Need to verify the new `ip_addrs()` method returns the same type/format.

### Medium Risk

3. **iroh-base ticket changes**: Tickets moved to separate `iroh-ticket` crate. Check if Harbor uses any ticket functionality.

4. **Relay URL handling**: `EndpointAddr::relay_urls()` is plural but currently supports only one relay URL. Verify this matches Harbor's relay usage.

### Low Risk

5. **ed25519_dalek independence**: Harbor's `security/` module uses `ed25519_dalek` directly (not through iroh). This should remain unaffected, but verify no implicit conversions were relied upon.

6. **Wire protocol compatibility**: iroh-relay has wire-level changes affecting pre-0.93 clients. Not relevant for Harbor since we're already on 0.93.

---

## Testing Strategy

1. **Compile first**: Update deps and fix all compilation errors
2. **Unit tests**: Run `cargo test` and fix any failures
3. **Integration tests**: Test peer connections, DHT lookups, and relay connectivity
4. **Simulation tests**: Run full network simulations to verify end-to-end functionality

---

## Rollback Plan

If issues are discovered post-upgrade:

1. Revert `Cargo.toml` changes
2. Run `cargo update` to restore lock file
3. No database migration concerns for Harbor (iroh-dns-server specific)

---

## References

- [iroh v0.94.0 Release Notes](https://github.com/n0-computer/iroh/releases/tag/v0.94.0)
- [iroh v0.95.0 Release Notes](https://github.com/n0-computer/iroh/releases/tag/v0.95.0)
- [iroh v0.95.1 Release Notes](https://github.com/n0-computer/iroh/releases/tag/v0.95.1)
- [iroh on crates.io](https://crates.io/crates/iroh)
