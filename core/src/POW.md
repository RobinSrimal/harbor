# Proof of Work - Generalized Design

## Current State

PoW is currently Harbor-specific. The challenge binds to `harbor_id` + `packet_id`:

```
BLAKE3(harbor_id || packet_id || timestamp || nonce)
```

This is hardcoded in `resilience/proof_of_work.rs` and only wired into `HarborService`. Configuration is a single global `PoWConfig` with one difficulty level.

## Goal

One PoW system, applicable to all ALPNs, with per-ALPN configuration for:
- Base difficulty
- Adaptive scaling curve
- Freshness window

## Generalized Challenge

Replace `harbor_id` + `packet_id` with a generic context:

```
BLAKE3(context || timestamp || nonce)
```

Where `context` is a variable-length byte slice provided by each ALPN. This prevents cross-ALPN replay (different context = different hash).

### Context per ALPN

| ALPN | Context | Rationale |
|------|---------|-----------|
| **harbor** | `harbor_id \|\| packet_id` | Same as current. Binds to specific topic + packet. |
| **control** | `sender_endpoint_id \|\| message_type_byte` | Binds to sender identity and action type. |
| **dht** | `sender_endpoint_id \|\| query_hash` | Binds to sender and specific query. |
| **send** | `sender_endpoint_id \|\| harbor_id` | Binds to sender and topic without exposing topic_id. |
| **share** | `sender_endpoint_id \|\| harbor_id` | Binds to sender and topic without exposing topic_id. |
| **sync** | `sender_endpoint_id \|\| harbor_id` | Binds to sender and topic without exposing topic_id. |
| **stream** | `sender_endpoint_id \|\| harbor_id` | Binds to sender and topic without exposing topic_id. |

For DM-scoped data plane messages, `harbor_id` would be replaced with `recipient_endpoint_id`.

> Note: `harbor_id = BLAKE3(topic_id)` so the context stays topic-bound without leaking the topic secret.

## Per-ALPN Configuration

```rust
struct PoWConfig {
    /// Base difficulty (leading zero bits)
    base_difficulty: u8,
    /// Maximum difficulty after scaling
    max_difficulty: u8,
    /// Freshness window in seconds
    max_age_secs: i64,
    /// Whether PoW is required
    enabled: bool,
    /// Scaling configuration
    scaling: ScalingConfig,
}

enum ScalingConfig {
    /// Scale based on request count (for control, dht)
    RequestBased {
        enabled: bool,
        /// Time window in seconds
        window_secs: u64,
        /// Requests before scaling kicks in
        threshold: u64,
        /// Difficulty bits added per request above threshold
        bits_per_request: u8,
    },
    /// Scale based on cumulative bytes (for harbor, send)
    ByteBased {
        enabled: bool,
        /// Time window in seconds
        window_secs: u64,
        /// Bytes before scaling kicks in
        byte_threshold: u64,
        /// Difficulty bits added per MB above threshold
        bits_per_mb: u8,
    },
}
```

### Presets

| ALPN | Base Difficulty | Max Difficulty | Scaling | Rationale |
|------|----------------|----------------|---------|-----------|
| **control** | 18 bits (~10-50ms) | 28 bits | Enabled, aggressive | Open to unknown peers, state-changing messages |
| **dht** | 18 bits (~10-50ms) | 28 bits | Enabled, moderate | Open to unknown peers, but queries are cheap |
| **harbor** | 18 bits (~10-50ms) | 32 bits | Enabled, aggressive | Open to unknown, writes to storage |
| **send** | 8 bits (~instant) | 20 bits | ByteBased, gentle (10MB threshold) | Already gated, high-frequency sync updates expected |
| **share** | None | None | None | Pull-based: receiver requests chunks they want. No cost imposed on receiver. Gated by connection list + membership. |
| **sync** | None | None | None | Receiver-initiated: large sync responses are requested by the receiver. Gated by connection list + membership. |
| **stream** | None | None | None | Real-time media. PoW latency unacceptable. Gated by connection list + membership. |

## Adaptive Scaling

### Harbor: Byte-Based Scaling

For Harbor, the concern is storage flooding. Scaling is based on **cumulative bytes stored per peer** within a time window, not request count. This naturally handles the difference between frequent small sync updates and large blob stores.

```
effective_difficulty = base_difficulty + scale_factor(bytes_stored_in_window)
capped at max_difficulty
```

Example:
- Window: 60 seconds
- Byte threshold: 1MB
- A peer doing 60 small sync updates (500 bytes each = 30KB/min) → base difficulty, no scaling
- A peer dumping 10MB in a minute → difficulty scales up significantly
- An agent flooding 500MB → hits max difficulty, effectively blocked

This uses a per-peer byte rate tracker (by endpoint ID), separate from `StorageManager`'s per-harbor-id quotas. Different dimensions: who's storing (peer rate) vs where it's stored (topic quota).

### Control / DHT: Request-Based Scaling

For control and DHT, request count is the right metric. These are infrequent by nature.

```
effective_difficulty = base_difficulty + (requests_above_threshold * bits_per_request)
capped at max_difficulty
```

Example for Control (aggressive):
- Window: 60 seconds
- Threshold: 10 requests
- Bits per request: 2
- Peer sends 15 requests in 60s → difficulty = 18 + (5 * 2) = 28 bits

### Send: Byte-Based Scaling (Light)

Send is the only data plane ALPN with PoW. It uses byte-based scaling with a high threshold since high-frequency small messages (sync updates per keystroke) are normal usage.

```
effective_difficulty = base_difficulty + scale_factor(bytes_sent_in_window)
capped at max_difficulty
```

Example:
- Window: 60 seconds
- Byte threshold: 10MB
- 60 sync updates at 500 bytes each (30KB/min) → base difficulty, no scaling
- Bulk data transfer at 50MB/min → mild scaling
- Agent flooding 500MB/min → hits max difficulty

### No PoW: Share, Sync, Stream

These ALPNs do not use PoW:
- **Share** - pull-based chunk transfer. Receiver requests data they want. No cost imposed on receiver.
- **Sync** - receiver-initiated large responses. The requester asked for the data.
- **Stream** - real-time media. Any PoW latency degrades the experience.

All three are already gated by connection list + topic membership. Rate limiting (without PoW) can still apply if needed.

## Rejection with Difficulty Hint

When a peer submits PoW with insufficient difficulty:

1. Verifier checks current effective difficulty for that peer
2. If PoW doesn't meet it, reject with the required difficulty in the response
3. Peer recomputes with the new difficulty and resubmits
4. No extra round trip in the common case (base difficulty is known from protocol spec)

## Replay Protection

PoW timestamp freshness (default 5 minutes) limits the replay window. For control ALPN where replays are more dangerous (state-changing), add request ID deduplication:

- Each control message includes a random request ID
- Receiver tracks seen IDs for the freshness window duration
- Duplicate IDs are dropped

Not needed for other ALPNs (Harbor stores are idempotent, DHT queries are stateless, data plane is gated).

## Topic Binding Without TopicID (Non-PoW)

Independently of PoW, protocols that must bind to a topic but should not
send the raw `topic_id` on the wire should include:

- `harbor_id` (BLAKE3(topic_id)) for topic lookup
- `membership_proof = BLAKE3(topic_id || harbor_id || sender_id)` to prove knowledge of the topic

This is used for topic-scoped control messages (TopicJoin, TopicLeave, RemoveMember)
and for SYNC_ALPN responses/requests. TopicInvite is the exception and must carry
`topic_id` so the recipient can learn the secret.

## Refactoring Required

### proof_of_work.rs

- Replace `harbor_id: [u8; 32]` + `packet_id: [u8; 32]` with `context: Vec<u8>`
- `PoWChallenge::new(context, difficulty_bits)` instead of `new(harbor_id, packet_id, difficulty_bits)`
- `ProofOfWork` stores `context: Vec<u8>` instead of two fixed fields
- Hash becomes `BLAKE3(context || timestamp || nonce)`
- All existing tests updated to pass context as `harbor_id || packet_id`

### Configuration

- `ProtocolConfig` holds a `HashMap<Alpn, PoWConfig>` or named fields per ALPN
- Each service receives its own `PoWConfig` on construction
- Scaling state (per-peer request tracking) lives in each service or in a shared `PoWScaler`

### Service Integration

- Each service verifies PoW on incoming connections using its own config
- Each service (or its callers) computes PoW before outgoing requests
- `HarborService` migration: replace `harbor_id`/`packet_id` fields with `context` built from `harbor_id || packet_id`
- New `ControlService`: uses `sender_endpoint_id || message_type_byte` as context
- Data plane services: use `sender_endpoint_id || topic_id` as context

### Backward Compatibility

The wire format changes (context replaces harbor_id + packet_id in ProofOfWork struct). This is a breaking change. Coordinate with protocol versioning.

## Open Questions

- Should scaling state persist across restarts or be in-memory only?
- Exact threshold and bits_per_request values per ALPN (needs tuning/benchmarking)
- Should the adaptive scaling tracker be shared across ALPNs or independent per ALPN?
- How to handle PoW for Harbor pull requests (currently only on store)?
- Wire format for the generalized ProofOfWork (context length prefix?)
