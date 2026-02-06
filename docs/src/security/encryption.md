# Encryption

Harbor uses different encryption strategies depending on the communication path.

## Direct Connections (QUIC)

When peers communicate directly, encryption is handled by the transport layer:

```
┌─────────────────────────────────────────┐
│              Peer A                      │
└─────────────────────────────────────────┘
                    │
                    │  QUIC / TLS 1.3
                    │  (transport encryption)
                    │
                    ▼
┌─────────────────────────────────────────┐
│              Peer B                      │
└─────────────────────────────────────────┘
```

**QUIC provides:**
- TLS 1.3 encryption
- Perfect forward secrecy
- Mutual authentication via Ed25519 keys
- Protection against eavesdropping

For direct connections, **no additional encryption is applied**. QUIC's transport security is sufficient since there's no untrusted intermediary.

## Harbor Node Storage (Packet Encryption)

When messages are stored on Harbor Nodes for offline delivery, **additional encryption is required** because Harbor Nodes are untrusted:

```
┌─────────────────────────────────────────┐
│              Peer A                      │
│                                          │
│  1. Encrypt payload with topic key       │
│  2. Sign packet                          │
│  3. Add HMAC (membership proof)          │
└─────────────────────────────────────────┘
                    │
                    │  QUIC (transport)
                    ▼
┌─────────────────────────────────────────┐
│           Harbor Node                    │
│                                          │
│  Stores encrypted packet                 │
│  CANNOT read content                     │
│  CANNOT identify recipients              │
└─────────────────────────────────────────┘
                    │
                    │  QUIC (transport)
                    ▼
┌─────────────────────────────────────────┐
│              Peer B                      │
│                                          │
│  1. Verify signature                     │
│  2. Decrypt with topic key               │
└─────────────────────────────────────────┘
```

### Packet Structure

Packets stored on Harbor Nodes contain:

```
Packet = {
    packet_id:   [u8; 32],   // Unique identifier
    harbor_id:   [u8; 32],   // Hash of TopicID (for routing)
    endpoint_id: [u8; 32],   // Sender's public key
    nonce:       [u8; 12],   // AEAD nonce
    ciphertext:  Vec<u8>,    // Encrypted payload
    tag:         [u8; 16],   // AEAD authentication tag
    mac:         [u8; 32],   // HMAC (topic membership proof)
    signature:   [u8; 64],   // Ed25519 signature
}
```

### Security Properties

| Property | How it's achieved |
|----------|-------------------|
| **Confidentiality** | Payload encrypted with ChaCha20-Poly1305, key derived from TopicID |
| **Integrity** | AEAD tag prevents tampering |
| **Authenticity** | Ed25519 signature proves sender identity |
| **Membership proof** | HMAC proves sender knows TopicID (without revealing it) |

## Key Derivation

### Topic Keys

Used to encrypt packets for Harbor storage:

```
topic_key = BLAKE3(TopicID || "topic-key")
```

Only topic members know the TopicID, so only they can derive the encryption key.

### HarborID

Used for routing (Harbor Nodes use this to organize storage):

```
harbor_id = BLAKE3(TopicID || "harbor-id")
```

This is a one-way hash. Harbor Nodes cannot reverse it to learn the TopicID.

### DM Keys

For direct messages between two peers:

```
dm_key = BLAKE3(sort(EndpointID_A, EndpointID_B) || "dm-key")
harbor_id = recipient's EndpointID
```

The encryption key is derived from both peers' EndpointIDs (sorted for consistency). The HarborID is simply the recipient's EndpointID - each peer pulls packets addressed to their own EndpointID.

## Epoch Keys (Forward Secrecy)

When a member is removed from a topic, the topic key is rotated:

```
epoch_n+1 = random()
```

The new key is distributed to remaining members via encrypted Control messages. This provides **forward secrecy** - removed members cannot decrypt messages sent after their removal.

## Identity Keys

Each node has an Ed25519 keypair:

- **Private key**: Stored encrypted in SQLCipher, never leaves the device
- **Public key**: Used as EndpointID, shared with peers

## Summary

| Path | Encryption | Why |
|------|------------|-----|
| Direct peer-to-peer | QUIC (TLS 1.3) | No untrusted intermediary |
| Via Harbor Nodes | QUIC + Packet encryption | Harbor Nodes are untrusted storage |

The additional packet encryption layer exists specifically to protect data stored on Harbor Nodes. For direct communication, QUIC's transport security is sufficient.
