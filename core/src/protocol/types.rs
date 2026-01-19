//! Public types for the Protocol API
//!
//! User-facing types for topic management and invites.

use serde::{Deserialize, Serialize};

use super::error::ProtocolError;

/// Sync status for a topic
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    /// Whether sync is enabled for this topic
    pub enabled: bool,
    /// Whether there are pending changes to broadcast
    pub dirty: bool,
    /// Current document version (hex-encoded)
    pub version: String,
}

/// Member info for topic invites
///
/// Contains the EndpointID and optional relay URL for connectivity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberInfo {
    /// The member's EndpointID (32-byte public key)
    pub endpoint_id: [u8; 32],
    /// Optional relay URL for cross-network connectivity
    pub relay_url: Option<String>,
}

impl MemberInfo {
    /// Create member info with just an endpoint ID (no relay)
    pub fn new(endpoint_id: [u8; 32]) -> Self {
        Self {
            endpoint_id,
            relay_url: None,
        }
    }

    /// Create member info with endpoint ID and relay URL
    pub fn with_relay(endpoint_id: [u8; 32], relay_url: String) -> Self {
        Self {
            endpoint_id,
            relay_url: Some(relay_url),
        }
    }
}

/// An invite to join a topic
///
/// This contains all the information needed for another peer to join,
/// including relay URLs for cross-network connectivity.
/// Can be serialized and shared (e.g., via QR code, link, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicInvite {
    /// The topic identifier
    pub topic_id: [u8; 32],
    /// Current members with their connection info
    pub member_info: Vec<MemberInfo>,
    /// Legacy: EndpointIDs only (for backwards compatibility)
    #[serde(default)]
    pub members: Vec<[u8; 32]>,
}

impl TopicInvite {
    /// Create a new topic invite with member info (includes relay URLs)
    pub fn new_with_info(topic_id: [u8; 32], member_info: Vec<MemberInfo>) -> Self {
        let members = member_info.iter().map(|m| m.endpoint_id).collect();
        Self {
            topic_id,
            member_info,
            members,
        }
    }

    /// Create a new topic invite (legacy, no relay info)
    pub fn new(topic_id: [u8; 32], members: Vec<[u8; 32]>) -> Self {
        let member_info = members.iter().map(|id| MemberInfo::new(*id)).collect();
        Self {
            topic_id,
            member_info,
            members,
        }
    }

    /// Get all member endpoint IDs
    pub fn member_ids(&self) -> Vec<[u8; 32]> {
        if !self.member_info.is_empty() {
            self.member_info.iter().map(|m| m.endpoint_id).collect()
        } else {
            self.members.iter().copied().collect()
        }
    }

    /// Get member info (with relay URLs if available)
    pub fn get_member_info(&self) -> &[MemberInfo] {
        &self.member_info
    }

    /// Serialize to bytes (for sharing)
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        postcard::to_allocvec(self)
            .map_err(|e| ProtocolError::InvalidInvite(format!("serialization failed: {}", e)))
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        postcard::from_bytes(bytes).map_err(|e| ProtocolError::InvalidInvite(e.to_string()))
    }

    /// Serialize to hex (for text sharing)
    pub fn to_hex(&self) -> Result<String, ProtocolError> {
        Ok(hex::encode(self.to_bytes()?))
    }

    /// Deserialize from hex
    pub fn from_hex(s: &str) -> Result<Self, ProtocolError> {
        let bytes =
            hex::decode(s).map_err(|e| ProtocolError::InvalidInvite(e.to_string()))?;
        Self::from_bytes(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_member_info_new() {
        let info = MemberInfo::new(test_id(1));
        assert_eq!(info.endpoint_id, test_id(1));
        assert!(info.relay_url.is_none());
    }

    #[test]
    fn test_member_info_with_relay() {
        let info = MemberInfo::with_relay(test_id(1), "https://relay.example.com".to_string());
        assert_eq!(info.endpoint_id, test_id(1));
        assert_eq!(info.relay_url, Some("https://relay.example.com".to_string()));
    }

    #[test]
    fn test_topic_invite_new() {
        let topic_id = test_id(1);
        let members = vec![test_id(10), test_id(11)];
        let invite = TopicInvite::new(topic_id, members.clone());

        assert_eq!(invite.topic_id, topic_id);
        assert_eq!(invite.members, members);
    }

    #[test]
    fn test_topic_invite_new_with_info() {
        let topic_id = test_id(1);
        let member_info = vec![
            MemberInfo::new(test_id(10)),
            MemberInfo::with_relay(test_id(11), "https://relay.example.com".to_string()),
        ];
        let invite = TopicInvite::new_with_info(topic_id, member_info);

        assert_eq!(invite.topic_id, topic_id);
        assert_eq!(invite.member_info.len(), 2);
        assert_eq!(invite.members.len(), 2);
    }

    #[test]
    fn test_topic_invite_serialization() {
        let invite = TopicInvite::new(test_id(1), vec![test_id(10), test_id(11), test_id(12)]);

        // Test bytes serialization
        let bytes = invite.to_bytes().unwrap();
        let restored = TopicInvite::from_bytes(&bytes).unwrap();
        assert_eq!(restored.topic_id, invite.topic_id);
        assert_eq!(restored.members.len(), 3);
    }

    #[test]
    fn test_topic_invite_empty_members() {
        let invite = TopicInvite::new(test_id(1), vec![]);

        let bytes = invite.to_bytes().unwrap();
        let restored = TopicInvite::from_bytes(&bytes).unwrap();
        assert_eq!(restored.topic_id, invite.topic_id);
        assert!(restored.members.is_empty());
    }

    #[test]
    fn test_topic_invite_many_members() {
        let members: Vec<[u8; 32]> = (0..100).map(|i| test_id(i)).collect();
        let invite = TopicInvite::new(test_id(255), members.clone());

        let bytes = invite.to_bytes().unwrap();
        let restored = TopicInvite::from_bytes(&bytes).unwrap();
        assert_eq!(restored.members.len(), 100);
    }

    #[test]
    fn test_topic_invite_hex() {
        let invite = TopicInvite::new(test_id(1), vec![test_id(10), test_id(11)]);

        // Test hex serialization
        let hex_str = invite.to_hex().unwrap();
        let restored = TopicInvite::from_hex(&hex_str).unwrap();
        assert_eq!(restored.topic_id, invite.topic_id);
        assert_eq!(restored.members.len(), 2);
    }

    #[test]
    fn test_topic_invite_hex_roundtrip() {
        let invite = TopicInvite::new(test_id(42), vec![test_id(1), test_id(2), test_id(3)]);

        let hex1 = invite.to_hex().unwrap();
        let restored1 = TopicInvite::from_hex(&hex1).unwrap();
        let hex2 = restored1.to_hex().unwrap();

        // Should be identical after roundtrip
        assert_eq!(hex1, hex2);
    }

    #[test]
    fn test_invalid_invite_bytes() {
        let result = TopicInvite::from_bytes(b"garbage");
        assert!(result.is_err());

        let result = TopicInvite::from_bytes(&[]);
        assert!(result.is_err());

        let result = TopicInvite::from_bytes(&[0; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_invite_hex() {
        let result = TopicInvite::from_hex("not-valid-hex!!!");
        assert!(result.is_err());

        let result = TopicInvite::from_hex("zzzz");
        assert!(result.is_err());

        let result = TopicInvite::from_hex("");
        assert!(result.is_err());
    }

    #[test]
    fn test_member_ids() {
        let invite = TopicInvite::new(test_id(1), vec![test_id(10), test_id(11)]);
        let ids = invite.member_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&test_id(10)));
        assert!(ids.contains(&test_id(11)));
    }

    #[test]
    fn test_get_member_info() {
        let member_info = vec![
            MemberInfo::new(test_id(10)),
            MemberInfo::with_relay(test_id(11), "https://relay.example.com".to_string()),
        ];
        let invite = TopicInvite::new_with_info(test_id(1), member_info);
        
        let info = invite.get_member_info();
        assert_eq!(info.len(), 2);
        assert!(info[0].relay_url.is_none());
        assert!(info[1].relay_url.is_some());
    }
}
