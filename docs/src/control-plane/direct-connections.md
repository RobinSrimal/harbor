# Direct Connections

Direct connections are the **1:1 control-plane relationship** between two peers. Once connected, you can use the data plane to send, stream, sync, or share.

## Create an Invite (Out-of-Band)

```rust
let invite = protocol.generate_connect_invite().await?;
let invite_string = invite.encode();
```

Share the invite via QR code, link, or any out-of-band channel.

## Connect With an Invite

```rust
let peer_id = protocol.connect_with_invite(&invite_string).await?;
```

## Request a Connection

```rust
protocol.request_connection(
    &peer_id,
    None,        // optional relay URL
    Some("Alice"),
    None         // optional token
).await?;
```

## Accept or Decline

```rust
protocol.accept_connection(&request_id).await?;
// or
protocol.decline_connection(&request_id, Some("Not accepting new contacts")) .await?;
```

## Block or Unblock

```rust
protocol.block_peer(&peer_id).await?;
protocol.unblock_peer(&peer_id).await?;
```

## Events

```rust
match event {
    ProtocolEvent::ConnectRequest { peer_id, request_id, .. } => {
        // Show UI prompt for accept/decline
    }
    _ => {}
}
```

## What Connections Enable

Once connected, you can use **all data-plane operations** with `Target::Dm(peer_id)`:
- [Send (Messages)](../data-plane/send.md)
- [Stream (Live Media)](../data-plane/stream.md)
- [Sync (CRDT)](../data-plane/sync.md)
- [Share (Files)](../data-plane/share.md)
