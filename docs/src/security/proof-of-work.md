# Proof of Work

Harbor uses adaptive Proof of Work (PoW) to prevent abuse and spam.

## Why PoW?

In a decentralized system without authentication servers, bad actors could:

- Flood Harbor Nodes with garbage data
- Spam peers with connection requests
- Pollute the DHT with fake entries
- Overwhelm nodes with requests

PoW makes these attacks expensive.

## How It Works

### Computing PoW

To send a request, the sender must find a nonce where:

```
BLAKE3(challenge || nonce) has N leading zero bits
```

The difficulty `N` determines how much work is required:

| Difficulty | Average attempts | Time (rough) |
|------------|-----------------|--------------|
| 16 bits | 65,536 | ~1ms |
| 20 bits | 1,048,576 | ~10ms |
| 24 bits | 16,777,216 | ~100ms |
| 28 bits | 268,435,456 | ~1s |
| 32 bits | 4,294,967,296 | ~10s |

### Verification

Receivers verify PoW instantly (single hash computation).

## Adaptive Scaling

Difficulty scales based on **per-peer volume**:

```
┌──────────────────────────────────────────────────┐
│                                                  │
│  Difficulty                                      │
│      ▲                                           │
│      │                         ┌────── max_bits  │
│      │                    ────/                  │
│      │               ────/                       │
│      │          ────/                            │
│      │     ────/                                 │
│  ────┼────/                                      │
│  min │                                           │
│      └──────────────────────────────────►        │
│                                    Volume        │
└──────────────────────────────────────────────────┘
```

- **Low volume**: Minimum difficulty (easy, fast)
- **High volume**: Difficulty scales up
- **Very high volume**: Maximum difficulty (expensive)

This allows:
- Normal users to operate without noticeable delay
- High-volume senders to prove they're investing resources
- Attackers to face exponentially increasing costs

## Per-Protocol Settings

| Protocol | Min Bits | Max Bits | Scaling |
|----------|----------|----------|---------|
| Harbor | 18 | 32 | Byte-based |
| Control | 18 | 28 | Request-based |
| DHT | 18 | 28 | Request-based |
| Send | 8 | 20 | Byte-based |
| Share | Disabled | - | - |
| Sync | Disabled | - | - |

### Why Different Settings?

- **Harbor**: Higher difficulty - stores data long-term
- **Control**: Medium - prevents spam connections
- **DHT**: Medium - prevents Sybil attacks
- **Send**: Lower - high-frequency messaging needs to be fast
- **Share/Sync**: Disabled - already flow-controlled by protocol design

## Implementation

PoW is computed automatically by the protocol:

```rust
// The protocol handles PoW internally
protocol.send(target, payload).await?;
// ↑ Computes PoW before sending
```

For Harbor storage:
```rust
// PoW difficulty scales with how much you've stored
harbor_service.store(packet).await?;
```

## Security Properties

### Sybil Resistance

Creating many fake identities is expensive because each requires PoW for every operation.

### Spam Prevention

Flooding a node requires computing PoW for each message. At scale, this becomes prohibitively expensive.

### Fairness

Difficulty scales per-peer, so:
- One peer sending 1000 messages faces high difficulty
- 1000 peers sending 1 message each face low difficulty

## Limitations

PoW doesn't prevent:
- Well-resourced attackers (but makes attacks expensive)
- Low-volume abuse (below scaling threshold)

It's one layer of defense, combined with rate limiting and storage quotas.
