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
    /// Sync document was updated (CRDT changes received)
    SyncUpdated(SyncUpdatedEvent),
    /// Initial sync completed for a topic
    SyncInitialized(SyncInitializedEvent),
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

/// Event: Sync document was updated
///
/// Emitted when CRDT changes are received from other members
#[derive(Debug, Clone)]
pub struct SyncUpdatedEvent {
    /// The topic this sync document belongs to
    pub topic_id: [u8; 32],
    /// The sender who made the changes
    pub sender_id: [u8; 32],
    /// Size of the update in bytes
    pub update_size: usize,
}

/// Event: Initial sync completed
///
/// Emitted when a new member receives the full document state
#[derive(Debug, Clone)]
pub struct SyncInitializedEvent {
    /// The topic that was synced
    pub topic_id: [u8; 32],
    /// Size of the snapshot in bytes
    pub snapshot_size: usize,
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

