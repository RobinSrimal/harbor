//! Protocol events for the application layer
//!
//! These events are emitted by the protocol and can be consumed by
//! applications to update their UI (e.g., show new messages, file progress).

/// Events emitted by the protocol for the application layer
///
/// Applications can subscribe to these events to update their UI
/// (e.g., show new messages, file progress bars, notifications)
#[derive(Debug, Clone)]
pub enum ProtocolEvent {
    /// A text message was received in a topic
    Message(IncomingMessage),
    /// A file was shared to a topic (announcement received)
    FileAnnounced(FileAnnouncedEvent),
    /// File download progress update
    FileProgress(FileProgressEvent),
    /// File download completed
    FileComplete(FileCompleteEvent),
    /// Sync update received from a peer (raw CRDT bytes)
    SyncUpdate(SyncUpdateEvent),
    /// A peer is requesting sync state
    SyncRequest(SyncRequestEvent),
    /// Response to our sync request (full CRDT state)
    SyncResponse(SyncResponseEvent),
    /// A peer wants to stream to us (app should accept/reject)
    StreamRequest(StreamRequestEvent),
    /// Our stream request was accepted
    StreamAccepted(StreamAcceptedEvent),
    /// Our stream request was rejected
    StreamRejected(StreamRejectedEvent),
    /// An active stream has ended
    StreamEnded(StreamEndedEvent),
}

/// An incoming message from the event bus
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// The topic this message belongs to
    pub topic_id: [u8; 32],
    /// The sender's EndpointID
    pub sender_id: [u8; 32],
    /// The message payload (app defines format)
    pub payload: Vec<u8>,
    /// Timestamp when the message was created (Unix seconds)
    pub timestamp: i64,
}

/// Event: A file was shared to a topic
///
/// Emitted when a FileAnnouncement is received from another member
#[derive(Debug, Clone)]
pub struct FileAnnouncedEvent {
    /// The topic this file was shared to
    pub topic_id: [u8; 32],
    /// Who shared the file
    pub source_id: [u8; 32],
    /// BLAKE3 hash of the file (unique identifier)
    pub hash: [u8; 32],
    /// Human-readable filename
    pub display_name: String,
    /// Total file size in bytes
    pub total_size: u64,
    /// Total number of chunks
    pub total_chunks: u32,
    /// Timestamp when announced
    pub timestamp: i64,
}

/// Event: File download progress update
///
/// Emitted periodically as chunks are received
#[derive(Debug, Clone)]
pub struct FileProgressEvent {
    /// File hash
    pub hash: [u8; 32],
    /// Chunks downloaded so far
    pub chunks_complete: u32,
    /// Total chunks in the file
    pub total_chunks: u32,
}

/// Event: File download completed
///
/// Emitted when all chunks have been received
#[derive(Debug, Clone)]
pub struct FileCompleteEvent {
    /// File hash
    pub hash: [u8; 32],
    /// Filename
    pub display_name: String,
    /// Total size in bytes
    pub total_size: u64,
}

/// Event: Sync update received from a peer
///
/// Application should apply this to their local CRDT.
/// Harbor is a transport layer - the `data` bytes are opaque.
#[derive(Debug, Clone)]
pub struct SyncUpdateEvent {
    /// The topic this sync update belongs to
    pub topic_id: [u8; 32],
    /// The sender who made the changes
    pub sender_id: [u8; 32],
    /// Raw CRDT bytes (Loro delta, Yjs update, etc.)
    pub data: Vec<u8>,
}

/// Event: A peer is requesting sync state
///
/// Application should respond with their current CRDT state via
/// `protocol.respond_sync()`.
#[derive(Debug, Clone)]
pub struct SyncRequestEvent {
    /// The topic being synced
    pub topic_id: [u8; 32],
    /// Who is requesting (respond to this peer)
    pub sender_id: [u8; 32],
}

/// Event: Response to our sync request
///
/// Application should import this into their CRDT.
#[derive(Debug, Clone)]
pub struct SyncResponseEvent {
    /// The topic this response is for
    pub topic_id: [u8; 32],
    /// Full CRDT state bytes
    pub data: Vec<u8>,
}

/// Event: A peer wants to stream to us
///
/// Application should accept or reject via `protocol.accept_stream()` / `reject_stream()`.
#[derive(Debug, Clone)]
pub struct StreamRequestEvent {
    /// The topic this stream is scoped to
    pub topic_id: [u8; 32],
    /// The peer requesting to stream
    pub peer_id: [u8; 32],
    /// Unique identifier for this stream request
    pub request_id: [u8; 32],
    /// Human-readable stream name
    pub name: String,
    /// Catalog metadata (codecs, renditions) serialized bytes
    pub catalog: Vec<u8>,
}

/// Event: Our stream request was accepted by the destination
#[derive(Debug, Clone)]
pub struct StreamAcceptedEvent {
    /// The request that was accepted
    pub request_id: [u8; 32],
}

/// Event: Our stream request was rejected by the destination
#[derive(Debug, Clone)]
pub struct StreamRejectedEvent {
    /// The request that was rejected
    pub request_id: [u8; 32],
    /// Optional reason for rejection
    pub reason: Option<String>,
}

/// Event: An active stream has ended
#[derive(Debug, Clone)]
pub struct StreamEndedEvent {
    /// The stream that ended
    pub request_id: [u8; 32],
    /// The peer whose stream ended
    pub peer_id: [u8; 32],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_incoming_message_structure() {
        let msg = IncomingMessage {
            topic_id: test_id(1),
            sender_id: test_id(2),
            payload: b"Hello world".to_vec(),
            timestamp: 1234567890,
        };

        assert_eq!(msg.topic_id, test_id(1));
        assert_eq!(msg.sender_id, test_id(2));
        assert_eq!(msg.payload, b"Hello world");
        assert_eq!(msg.timestamp, 1234567890);
    }

    #[test]
    fn test_file_announced_event() {
        let event = FileAnnouncedEvent {
            topic_id: test_id(1),
            source_id: test_id(2),
            hash: test_id(3),
            display_name: "test.pdf".to_string(),
            total_size: 1024 * 1024,
            total_chunks: 2,
            timestamp: 1234567890,
        };

        assert_eq!(event.display_name, "test.pdf");
        assert_eq!(event.total_size, 1024 * 1024);
    }

    #[test]
    fn test_file_progress_event() {
        let event = FileProgressEvent {
            hash: test_id(1),
            chunks_complete: 5,
            total_chunks: 10,
        };

        assert_eq!(event.chunks_complete, 5);
        assert_eq!(event.total_chunks, 10);
    }

    #[test]
    fn test_file_complete_event() {
        let event = FileCompleteEvent {
            hash: test_id(1),
            display_name: "done.zip".to_string(),
            total_size: 2048,
        };

        assert_eq!(event.display_name, "done.zip");
        assert_eq!(event.total_size, 2048);
    }

    #[test]
    fn test_protocol_event_variants() {
        let msg_event = ProtocolEvent::Message(IncomingMessage {
            topic_id: test_id(1),
            sender_id: test_id(2),
            payload: vec![],
            timestamp: 0,
        });

        assert!(matches!(msg_event, ProtocolEvent::Message(_)));

        let file_event = ProtocolEvent::FileAnnounced(FileAnnouncedEvent {
            topic_id: test_id(1),
            source_id: test_id(2),
            hash: test_id(3),
            display_name: String::new(),
            total_size: 0,
            total_chunks: 0,
            timestamp: 0,
        });

        assert!(matches!(file_event, ProtocolEvent::FileAnnounced(_)));
    }
}

