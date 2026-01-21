//! Protocol errors

/// Errors that can occur in the protocol
#[derive(Debug)]
pub enum ProtocolError {
    /// Failed to start the protocol
    StartFailed(String),
    /// Database error
    Database(String),
    /// Network error
    Network(String),
    /// Not a member of the topic
    NotMember,
    /// Topic not found
    TopicNotFound,
    /// Invalid invite data
    InvalidInvite(String),
    /// Protocol is not running
    NotRunning,
    /// Message too large (max 512KB)
    MessageTooLarge,
    /// Invalid input provided
    InvalidInput(String),
    /// Resource not found
    NotFound(String),
    /// IO error
    Io(String),
    /// Sync error
    Sync(String),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::StartFailed(e) => write!(f, "failed to start protocol: {}", e),
            ProtocolError::Database(e) => write!(f, "database error: {}", e),
            ProtocolError::Network(e) => write!(f, "network error: {}", e),
            ProtocolError::NotMember => write!(f, "not a member of this topic"),
            ProtocolError::TopicNotFound => write!(f, "topic not found"),
            ProtocolError::InvalidInvite(e) => write!(f, "invalid invite: {}", e),
            ProtocolError::NotRunning => write!(f, "protocol is not running"),
            ProtocolError::MessageTooLarge => write!(f, "message too large (max 512KB)"),
            ProtocolError::InvalidInput(e) => write!(f, "invalid input: {}", e),
            ProtocolError::NotFound(e) => write!(f, "not found: {}", e),
            ProtocolError::Io(e) => write!(f, "io error: {}", e),
            ProtocolError::Sync(e) => write!(f, "sync error: {}", e),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<std::io::Error> for ProtocolError {
    fn from(e: std::io::Error) -> Self {
        ProtocolError::Io(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_error_display() {
        let err = ProtocolError::NotMember;
        assert_eq!(err.to_string(), "not a member of this topic");

        let err = ProtocolError::MessageTooLarge;
        assert_eq!(err.to_string(), "message too large (max 512KB)");

        let err = ProtocolError::TopicNotFound;
        assert_eq!(err.to_string(), "topic not found");

        let err = ProtocolError::NotRunning;
        assert_eq!(err.to_string(), "protocol is not running");

        let err = ProtocolError::Database("test error".to_string());
        assert_eq!(err.to_string(), "database error: test error");

        let err = ProtocolError::Network("connection failed".to_string());
        assert_eq!(err.to_string(), "network error: connection failed");

        let err = ProtocolError::InvalidInvite("bad format".to_string());
        assert_eq!(err.to_string(), "invalid invite: bad format");

        let err = ProtocolError::StartFailed("no endpoint".to_string());
        assert_eq!(err.to_string(), "failed to start protocol: no endpoint");
    }

    #[test]
    fn test_protocol_error_is_error_trait() {
        let err: Box<dyn std::error::Error> = Box::new(ProtocolError::NotMember);
        assert!(!err.to_string().is_empty());
    }
}

