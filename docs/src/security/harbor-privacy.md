# Harbor Node Privacy

A critical design goal of Harbor is that **Harbor Nodes cannot read the messages they store**. This page explains what Harbor Nodes can and cannot see.

## What Harbor Nodes CAN See

| Information | Why |
|-------------|-----|
| **HarborID** | Needed for routing (but this is a hash, not the TopicID) |
| **Sender EndpointID** | Public key in the packet (proves authenticity) |
| **Packet size** | Can't be hidden |
| **Timing** | When packets arrive |
| **Encrypted ciphertext** | Opaque blob they store |

## What Harbor Nodes CANNOT See

| Information | Why |
|-------------|-----|
| **TopicID** | They only see `BLAKE3(TopicID)`, a one-way hash |
| **Message content** | Encrypted with topic key they don't have |
| **Recipient identities** | Not included in packets |
| **Topic membership** | Can't map HarborID back to members |
| **Message type** | Encrypted inside the payload |

## Why This Matters

```
Alice sends "Hello Bob!" to TopicXYZ
              │
              ▼
┌─────────────────────────────────────────┐
│            Harbor Node sees:             │
│                                          │
│  HarborID: 7f3a2b...  (hash, not topic) │
│  Sender: a1b2c3...    (Alice's pubkey)  │
│  Size: 156 bytes                        │
│  Ciphertext: [encrypted blob]           │
│                                          │
│  CANNOT see:                            │
│  - "Hello Bob!"                         │
│  - That it's going to Bob               │
│  - What topic it belongs to             │
└─────────────────────────────────────────┘
```

## How Privacy is Achieved

### 1. HarborID is a Hash

The Harbor Node routes by HarborID, which is:

```
HarborID = BLAKE3(TopicID)
```

This is a **one-way function**. Given only the HarborID, you cannot compute the TopicID. Without the TopicID, you cannot derive the encryption key.

### 2. Encryption Keys Derive from TopicID

```
encryption_key = BLAKE3(TopicID || "topic-key")
```

Since Harbor Nodes don't know the TopicID, they cannot compute the encryption key.

### 3. No Recipient Information in Packets

Packets don't contain recipient identities. Harbor Nodes store packets for a HarborID and serve them to anyone who requests that HarborID.

Recipients prove they're authorized by:
1. Knowing the HarborID (requires knowing TopicID)
2. Being able to decrypt the packet (requires topic key)

### 4. HMAC Verification Without Knowledge

The HMAC proves the sender knows the TopicID:

```
hmac = HMAC(topic_key, packet_data)
```

Harbor Nodes can verify the HMAC is **consistent** (same key was used) without knowing what key that is.

## Metadata Considerations

While content is private, some metadata is visible:

### Traffic Analysis

An observer could potentially correlate:
- Timing of stores and pulls
- Packet sizes
- Frequency of access

### Mitigation Strategies

1. **Padding**: Fixed-size packets (future enhancement)
2. **Batching**: Group operations together
3. **Cover traffic**: Background noise (future enhancement)

## Comparison with Alternatives

| System | Server sees content? | Server sees participants? |
|--------|---------------------|--------------------------|
| Email | Yes | Yes |
| WhatsApp | No (E2E) | Yes (metadata) |
| Signal | No (E2E) | Minimal (sealed sender) |
| **Harbor** | No | No (only HarborID hash) |

## Trust Model

You don't need to trust Harbor Nodes with your data:

- They **cannot** read your messages
- They **cannot** identify your contacts
- They **can** refuse to store your packets (mitigated by redundancy)
- They **can** delete your packets (mitigated by replication)

The worst a malicious Harbor Node can do is refuse service - they cannot compromise privacy.
