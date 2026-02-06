//! Topic membership proofs
//!
//! Shared helpers for proving knowledge of a topic secret without sending the
//! raw `topic_id` on the wire. Used by Send (membership proof), Control topic
//! messages, and Sync direct messages.

use crate::security::harbor_id_from_topic;

/// Membership proof bytes (BLAKE3 hash).
pub type MembershipProof = [u8; 32];

/// Create a membership proof for a topic.
///
/// Proof = BLAKE3(topic_id || harbor_id || sender_id)
pub fn create_membership_proof(
    topic_id: &[u8; 32],
    harbor_id: &[u8; 32],
    sender_id: &[u8; 32],
) -> MembershipProof {
    let mut hasher = blake3::Hasher::new();
    hasher.update(topic_id);
    hasher.update(harbor_id);
    hasher.update(sender_id);
    *hasher.finalize().as_bytes()
}

/// Verify a membership proof for a topic.
pub fn verify_membership_proof(
    topic_id: &[u8; 32],
    harbor_id: &[u8; 32],
    sender_id: &[u8; 32],
    proof: &MembershipProof,
) -> bool {
    let expected = create_membership_proof(topic_id, harbor_id, sender_id);
    expected == *proof
}

/// Convenience: compute harbor_id and membership proof for a topic.
pub fn create_membership_binding(
    topic_id: &[u8; 32],
    sender_id: &[u8; 32],
) -> ([u8; 32], MembershipProof) {
    let harbor_id = harbor_id_from_topic(topic_id);
    let proof = create_membership_proof(topic_id, &harbor_id, sender_id);
    (harbor_id, proof)
}
