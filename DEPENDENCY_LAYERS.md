# Dependency Layers and Pre-Testnet Review Checklist

This document is the working playbook for final review before testnet.

## Review Log

Use `REVIEW_LOG.md` for day-to-day review tracking (layer status, per-file findings, approvals, and test outcomes). Keep this file focused on process and checklist guidance.

## Review Contract (Always Follow)

For every file, use this exact flow:

1. Analyze
2. Present possible improvements
3. Wait for explicit approval
4. Implement

No implementation happens during steps 1-3.

## Review Goals

- Clean outdated comments and stale docs.
- Ensure tests are sound and meaningful.
- Expand test coverage where gaps are real.
- Understand behavior and invariants before changing code.
- Reduce bugs and edge-case risk.
- Identify easy performance wins only (low-risk, high-confidence).

## Baseline Commands

Run once before starting:

```bash
cargo test -p harbor-core --quiet
```

Useful review commands:

```bash
# List files in a layer
rg --files core/src/<layer_path> | rg '\.rs$' | sort

# Find tests in a file/module
rg -n '#\[cfg\(test\)\]|#\[test\]|tokio::test' core/src/<path>

# Find panic-prone calls
rg -n 'unwrap\(|expect\(' core/src/<path>

# Find comment debt markers
rg -n 'TODO|FIXME|XXX|HACK|deprecated|outdated|stale|old' core/src/<path>
```

## Layer Map

```text
L7 Apps and Integrations
  test-app/src-tauri
  core/src/main.rs (harbor-cli)
  core/src/http
  core/src/events.rs
  ↓
L6 Public API and Runtime
  core/src/protocol
  core/src/tasks
  core/src/handlers
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

## Layer Details

| Layer | Purpose | Key Paths | Depends On |
| --- | --- | --- | --- |
| L7 | Apps, CLI, HTTP API, integration entry points | `test-app/src-tauri`, `core/src/main.rs`, `core/src/http`, `core/src/events.rs` | `harbor_core` public API |
| L6 | Public API, orchestration, background tasks, incoming handlers | `core/src/protocol`, `core/src/tasks`, `core/src/handlers` | L5, L4, L3, L2, L1 |
| L5 | Protocol services (one per ALPN) and service-to-service calls | `core/src/network/send`, `core/src/network/harbor`, `core/src/network/share`, `core/src/network/sync`, `core/src/network/stream`, `core/src/network/control`, `core/src/network/dht` | L4, L3, L2, L1 |
| L4 | Transport helpers and packet processing | `core/src/network/connect`, `core/src/network/pool`, `core/src/network/gate`, `core/src/network/rpc`, `core/src/network/process`, `core/src/network/wire`, `core/src/network/packet`, `core/src/network/membership` | L3, L2, L1 |
| L3 | Abuse resistance (PoW, rate limits, storage quotas) | `core/src/resilience` | L2 |
| L2 | Persistence, identity storage, blobs, keychain integration | `core/src/data`, `core/src/keychain` | L1 |
| L1 | Cryptography and key derivation | `core/src/security` | L0 |
| L0 | External crates and OS services | Cargo dependencies, OS keychain | n/a |

## Known Cross-Layer Couplings

1. L5 and L4 depend upward on L6 for event and error types. Examples include `core/src/network/process.rs` and multiple `core/src/network/*/service.rs` files that construct `ProtocolEvent` or use `ProtocolError`.
2. L6 re-exports types from L5 and L2 in `core/src/protocol/mod.rs`, so app-facing types originate in lower layers by design.

## Suggested Review Order

1. L1 Security and Crypto
2. L2 Data and Storage
3. L3 Resilience
4. L4 Network Primitives and Transport Helpers
5. L5 Network Services
6. L6 Public API and Runtime
7. L7 Apps and Integrations

## Per-File Checklist (Use for Every File)

### 1) Analyze

- [ ] Read the file top-to-bottom and summarize its responsibility in 1-3 lines.
- [ ] Identify key invariants (security, correctness, data consistency, protocol assumptions).
- [ ] Trace who calls this file and what it calls.
- [ ] Confirm layer boundaries are respected.

### 2) Present Possible Improvements (No Code Yet)

- [ ] List bugs/risks first (panic risk, race/lock issues, incorrect error handling, invalid input paths).
- [ ] List outdated comments/docs and exact lines that should be cleaned.
- [ ] Evaluate tests: what exists, what is missing, which edge cases are uncovered.
- [ ] Propose only concrete improvements with expected impact.
- [ ] Flag low-effort performance wins separately from bigger changes.

### 3) Approval Gate

- [ ] Pause and wait for explicit approval.
- [ ] Do not implement before approval.

### 4) Implement (Only After Approval)

- [ ] Apply approved changes only.
- [ ] Keep changes scoped to the file(s) under review unless a dependency requires a small companion edit.
- [ ] Add/adjust tests for behavior changes or newly covered edge cases.
- [ ] Re-run targeted tests first, then broader tests as needed.

### 5) File Sign-Off

- [ ] Behavior understood and documented.
- [ ] Outdated comments/docs cleaned.
- [ ] Tests are sound for current behavior.
- [ ] Coverage gaps either closed or explicitly documented with rationale.
- [ ] No new obvious panic paths or regressions introduced.
- [ ] Easy performance wins either applied or explicitly deferred.

## Improvement Proposal Template (Copy/Paste Per File)

```text
File: <path>

Current behavior summary:
- ...

Issues/Risks:
- [High] ...
- [Medium] ...
- [Low] ...

Outdated comments/docs to clean:
- <line / section> ...

Test assessment:
- Existing tests: ...
- Missing coverage: ...
- Proposed tests: ...

Easy performance wins (optional):
- ...

Proposed implementation plan (pending approval):
1. ...
2. ...
3. ...
```

## Layer Completion Checklist

### L1 Security and Crypto (`core/src/security`)

- [ ] All files reviewed with per-file checklist.
- [ ] Security invariants validated (authenticity, confidentiality, key derivation assumptions).
- [ ] Layer tests green.

### L2 Data and Storage (`core/src/data`, `core/src/keychain`)

- [ ] All files reviewed with per-file checklist.
- [ ] Data consistency and migration/schema assumptions validated.
- [ ] Layer tests green.

### L3 Resilience (`core/src/resilience`)

- [ ] All files reviewed with per-file checklist.
- [ ] Rate-limit, PoW, and storage-limit behavior validated.
- [ ] Layer tests green.

### L4 Network Primitives (`core/src/network/connect`, `core/src/network/pool`, `core/src/network/gate`, `core/src/network/rpc`, `core/src/network/process`, `core/src/network/wire`, `core/src/network/packet`, `core/src/network/membership`)

- [ ] All files reviewed with per-file checklist.
- [ ] Packet decode/verify and routing assumptions validated.
- [ ] Layer tests green.

### L5 Network Services (`core/src/network/send`, `core/src/network/harbor`, `core/src/network/share`, `core/src/network/sync`, `core/src/network/stream`, `core/src/network/control`, `core/src/network/dht`)

- [ ] All files reviewed with per-file checklist.
- [ ] Service-specific edge cases validated (timeouts, retries, partial failures).
- [ ] Layer tests green.

### L6 Public API and Runtime (`core/src/protocol`, `core/src/tasks`, `core/src/handlers`)

- [ ] All files reviewed with per-file checklist.
- [ ] API error surfaces and event semantics validated.
- [ ] Layer tests green.

### L7 Apps and Integrations (`core/src/main.rs`, `core/src/http`, `core/src/events.rs`, `test-app/src-tauri`)

- [ ] All files reviewed with per-file checklist.
- [ ] CLI/API parsing and integration behavior validated.
- [ ] Layer tests green.

## Final Pre-Testnet Gate

- [ ] All layers signed off.
- [ ] Full `harbor-core` test suite green.
- [ ] High-risk issues either fixed or explicitly accepted with rationale.
- [ ] Deferred performance items documented for post-testnet work.
