//! Target type for addressing messages
//!
//! Messages can be sent to a topic (group) or directly to a peer (DM).

/// Destination for a send operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Send to all members of a topic
    Topic([u8; 32]),
    /// Send directly to a single peer (DM)
    Dm([u8; 32]),
}

impl Target {
    /// Returns true if this is a DM target
    pub fn is_dm(&self) -> bool {
        matches!(self, Target::Dm(_))
    }

    /// Returns the inner ID (topic_id or peer_id)
    pub fn id(&self) -> &[u8; 32] {
        match self {
            Target::Topic(id) | Target::Dm(id) => id,
        }
    }
}
