# Harbor Stream

Real-time streaming for Harbor - audio/video calls per topic.

## Status

🚧 **Planned** - This crate is not yet implemented.

## Overview

Harbor Stream will provide real-time audio/video streaming built on top of Harbor Core. It enables voice/video calls and other streaming use cases within topics.

## Planned Features

- Per-topic audio/video streams
- Low-latency peer-to-peer streaming
- Automatic quality adaptation based on network conditions
- Screen sharing support
- Built on [iroh-live](https://github.com/n0-computer/iroh-live) or WebRTC

## Usage (Future)

```rust
use harbor_core::{Protocol, ProtocolConfig};
use harbor_stream::{StreamSession, MediaTrack};

// Start core protocol
let protocol = Protocol::start(ProtocolConfig::default()).await?;

// Start a streaming session for a topic
let session = StreamSession::new(&protocol, &topic_id).await?;

// Add local audio/video tracks
session.add_track(MediaTrack::camera()).await?;
session.add_track(MediaTrack::microphone()).await?;

// Receive remote tracks
while let Some(track) = session.next_remote_track().await {
    // Handle incoming audio/video
}
```

## Dependencies

- `harbor-core`: Core messaging protocol (required)

