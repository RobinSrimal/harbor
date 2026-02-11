# Topics

Topics are **invite-only groups**. The control plane manages who belongs to a topic; the data plane handles what members do inside it.

## Create a Topic

```rust
let invite = protocol.create_topic().await?;
println!("Share this invite: {}", invite.to_hex()?);
```

## Join a Topic

```rust
let invite = TopicInvite::from_hex(&invite_string)?;
protocol.join_topic(invite).await?;
```

## Invite a Connected Peer

```rust
protocol.invite_to_topic(&peer_id, &topic_id).await?;
```

The peer receives a `TopicInviteReceived` event and can accept or decline.

## Leave or Remove Members

```rust
protocol.leave_topic(&topic_id).await?;
protocol.remove_topic_member(&topic_id, &member_id).await?;
```

Removing a member rotates the topic key so they can't read future messages.

## Events

```rust
match event {
    ProtocolEvent::TopicInviteReceived { .. } => {
        // Prompt user to accept or decline
    }
    _ => {}
}
```

## What Topics Enable

Once a peer is a member, you can use **all data-plane operations** inside the topic:

- [Send (Messages)](../data-plane/send.md)
- [Stream (Live Media)](../data-plane/stream.md)
- [Sync (CRDT)](../data-plane/sync.md)
- [Share (Files)](../data-plane/share.md)
