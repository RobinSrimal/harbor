//! Live Session - Per-peer MOQ session wrapper
//!
//! Wraps a moq-lite session with topic scoping and harbor identity.

use moq_lite::{BroadcastConsumer, BroadcastProducer, Origin, Session};

/// A live streaming session with a specific peer
///
/// Wraps the moq-lite Session and Origin for a single peer connection,
/// scoped to a specific topic and stream request.
pub struct LiveSession {
    /// The underlying MOQ session
    session: Session,
    /// Origin for this session (publish/subscribe coordination)
    origin_producer: moq_lite::OriginProducer,
    origin_consumer: moq_lite::OriginConsumer,
    /// Topic this session is scoped to
    topic_id: [u8; 32],
    /// The stream request ID
    request_id: [u8; 32],
}

impl LiveSession {
    /// Create a new LiveSession wrapping a MOQ session (creates its own Origin)
    pub fn new(
        session: Session,
        topic_id: [u8; 32],
        request_id: [u8; 32],
    ) -> Self {
        let origin = Origin::produce();
        Self {
            session,
            origin_producer: origin.producer,
            origin_consumer: origin.consumer,
            topic_id,
            request_id,
        }
    }

    /// Create a LiveSession from pre-existing parts (used by the handler
    /// when the Origin was already created for the MOQ Server handshake)
    pub fn from_parts(
        session: Session,
        origin_producer: moq_lite::OriginProducer,
        origin_consumer: moq_lite::OriginConsumer,
        topic_id: [u8; 32],
        request_id: [u8; 32],
    ) -> Self {
        Self {
            session,
            origin_producer,
            origin_consumer,
            topic_id,
            request_id,
        }
    }

    /// Get the topic this session is scoped to
    pub fn topic_id(&self) -> &[u8; 32] {
        &self.topic_id
    }

    /// Get the request ID for this session
    pub fn request_id(&self) -> &[u8; 32] {
        &self.request_id
    }

    /// Create a broadcast producer for publishing media
    pub fn create_broadcast(&self, path: &str) -> Option<BroadcastProducer> {
        self.origin_producer.create_broadcast(path)
    }

    /// Consume a broadcast from the remote peer
    pub fn consume_broadcast(&self, path: &str) -> Option<BroadcastConsumer> {
        self.origin_consumer.consume_broadcast(path)
    }

    /// Get the origin consumer (for passing to moq-lite Client/Server)
    pub fn origin_consumer(&self) -> &moq_lite::OriginConsumer {
        &self.origin_consumer
    }

    /// Get the origin producer (for passing to moq-lite Client/Server)
    pub fn origin_producer(&self) -> &moq_lite::OriginProducer {
        &self.origin_producer
    }

    /// Get the underlying MOQ session
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Close the session
    pub fn close(self) {
        self.session.close(moq_lite::Error::Cancel);
    }
}
