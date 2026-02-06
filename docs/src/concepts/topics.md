# Topics & Membership

Topics are the foundation of group communication in Harbor.

## What is a Topic?

A topic is a private group with:

- A unique 32-byte **TopicID**
- A list of **members** (by their EndpointID)
- Shared **encryption keys** derived from the TopicID
- A **HarborID** (hash of TopicID) for storage routing

## Creating a Topic

When you create a topic, you become the first member:

```rust
let invite = protocol.create_topic().await?;
println!("TopicID: {}", hex::encode(&invite.topic_id));
println!("Invite: {}", invite.to_hex()?);
```

## Invites

An invite contains everything needed to join:

- TopicID
- Current member list (with relay URLs)
- Epoch key for decryption

Invites are opaque hex strings that can be shared out-of-band:

```
746f70696331323334353637383930...
```

## Joining a Topic

```rust
let invite = TopicInvite::from_hex(&invite_string)?;
protocol.join_topic(invite).await?;
```

When you join:
1. You're added to your local member list
2. You announce yourself to existing members
3. Existing members add you to their lists

## Member Discovery

Members discover each other through:

1. **Invite**: Initial members come from the invite
2. **Join announcements**: New members broadcast `TopicJoin` messages
3. **Sync**: Members periodically sync membership state

## Inviting Peers

You can invite a **connected peer** to a topic:

```rust
// First, establish a connection with the peer
protocol.invite_to_topic(&peer_id, &topic_id).await?;
```

The peer receives a `TopicInviteReceived` event and can accept or decline.

## Leaving a Topic

```rust
protocol.leave_topic(&topic_id).await?;
```

This broadcasts a `TopicLeave` message to all members.

## Removing Members

Topic creators/admins can remove members:

```rust
protocol.remove_member(&topic_id, &member_id).await?;
```

This triggers **epoch key rotation** - the removed member can no longer decrypt new messages.

## Epoch Keys

Topics use epoch-based key rotation:

- **Epoch 0**: Initial key derived from TopicID
- **Epoch N+1**: New key after member removal

When a member is removed:
1. A new epoch key is generated
2. The key is distributed to remaining members
3. Future messages use the new epoch

This ensures forward secrecy - removed members can't read new messages.

## Topic vs DM

| Feature | Topic | DM |
|---------|-------|-----|
| Participants | Multiple | Two |
| Invites | Required | Connection-based |
| Key rotation | On member removal | N/A |
| Harbor routing | HarborID = hash of TopicID | HarborID = recipient's EndpointID |
