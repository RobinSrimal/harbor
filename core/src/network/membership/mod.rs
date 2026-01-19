//! Membership Module (Tier 1 - Open Topics)
//!
//! Handles topic membership for Tier 1 (open topics):
//! - Join/Leave announcements via topic messages
//! - Member discovery via invites and Harbor pull
//!
//! # Tier 1 Security Model
//!
//! - Key derivation: `key = KDF(topic_id)` - same for all members
//! - Anyone with `topic_id` can join and participate
//! - No eviction capability (no key rotation)
//! - MAC + Signature verification proves topic membership and sender identity
//!
//! # Protocol Flow
//!
//! ## Joining
//! 1. Receive invite (topic_id + complete member list) out of band
//! 2. Derive keys from topic_id
//! 3. Add all members from invite to local list
//! 4. Send JoinMessage to all members (direct + Harbor replication)
//! 5. Start sending/receiving messages
//!
//! ## Leaving
//! 1. Send LeaveMessage to all members
//! 2. Unsubscribe from topic
//!
//! ## Member Sync
//! - Join/Leave messages stored on Harbor (wildcard recipient, 90-day retention)
//! - Harbor pull processes these to update local member lists
//! - Invites always contain complete current member list

// Message types for membership (Join/Leave/Content)
pub mod messages;

// Re-exports: Messages
pub use messages::{
    get_verification_mode_from_payload, DecodeError as MessageDecodeError, JoinMessage,
    LeaveMessage, MessageType, TopicMessage,
    // Share-related control messages
    FileAnnouncementMessage, CanSeedMessage,
};

// Wildcard recipient for Join/Leave messages stored on Harbor
// (ensures offline members can receive membership updates)
/// Wildcard recipient - packets with this recipient are delivered to ALL topic members
pub const WILDCARD_RECIPIENT: [u8; 32] = [0xFF; 32];

/// Check if a recipient ID is the wildcard
pub fn is_wildcard_recipient(recipient: &[u8; 32]) -> bool {
    *recipient == WILDCARD_RECIPIENT
}
