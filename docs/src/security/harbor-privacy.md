# Harbor Node Privacy

A critical design goal of Harbor is that **Harbor Nodes cannot read the messages they store**. However, they do see some metadata. This page explains what Harbor Nodes can and cannot see.

## What Harbor Nodes CAN See

| Information | Why |
|-------------|-----|
| **HarborID** | Needed for routing (but this is a hash, not the TopicID) |
| **Sender EndpointID** | Public key in the packet (proves authenticity) |
| **Recipient EndpointIDs** | Needed for per-recipient delivery tracking |
| **Packet size** | Can't be hidden |
| **Timing** | When packets arrive |
| **Encrypted ciphertext** | Opaque blob they store |

## What Harbor Nodes CANNOT See

| Information | Why |
|-------------|-----|
| **TopicID** | They only see `BLAKE3(TopicID)`, a one-way hash |
| **Message content** | Encrypted with topic key they don't have |
| **Topic membership** | Can't map HarborID back to full member list |
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
│  Recipients: [d4e5f6..., g7h8i9...]     │
│  Size: 156 bytes                        │
│  Ciphertext: [encrypted blob]           │
│                                          │
│  CANNOT see:                            │
│  - "Hello Bob!"                         │
│  - What topic it belongs to             │
└─────────────────────────────────────────┘
```

## How Privacy is Achieved

### 1. HarborID for Routing

The Harbor Node routes by HarborID:

- **Topics**: `HarborID = BLAKE3(TopicID)` - a one-way hash, so Harbor Nodes cannot derive the TopicID or encryption keys
- **DMs**: `HarborID = recipient's EndpointID` - this is already public (the recipient's public key)

### 2. Encryption Keys

- **Topics**: `encryption_key = BLAKE3(TopicID || "topic-key")` - Harbor Nodes don't know the TopicID
- **DMs**: ECDH shared secret from sender + recipient keypairs - Harbor Nodes don't have the private keys

### 3. Recipient Tracking for Delivery

Packets include recipient EndpointIDs so Harbor Nodes can:
- Track per-recipient delivery status
- Only serve packets to the intended recipients

Recipients prove they're authorized via the QUIC connection - the authenticated node ID from the connection handshake must match the stored recipient.

## Metadata Considerations

While content is private, some metadata is visible:

### Traffic Analysis

An observer could potentially correlate:
- Timing of stores and pulls
- Packet sizes
- Frequency of access

## Trust Model

You don't need to trust Harbor Nodes with your message content:

- They **cannot** read your messages
- They **can** see sender and recipient EndpointIDs (metadata)
- They **cannot** see the full topic membership list
- They **can** refuse to store your packets (mitigated by redundancy)
- They **can** delete your packets (mitigated by replication)

The worst a malicious Harbor Node can do is refuse service or observe communication metadata - they cannot read message content.
