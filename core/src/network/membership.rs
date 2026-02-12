//! Topic membership proofs
//!
//! Shared helpers for proving knowledge of a topic secret without sending the
//! raw `topic_id` on the wire. Used by Send (membership proof), Control topic
//! messages, and Sync direct messages.

use crate::security::harbor_id_from_topic;
use subtle::ConstantTimeEq;

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
    expected.ct_eq(proof).into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_membership_proof_roundtrip() {
        let topic_id = [1u8; 32];
        let harbor_id = harbor_id_from_topic(&topic_id);
        let sender_id = [2u8; 32];

        let proof = create_membership_proof(&topic_id, &harbor_id, &sender_id);
        assert!(verify_membership_proof(
            &topic_id, &harbor_id, &sender_id, &proof
        ));
    }

    #[test]
    fn test_membership_proof_rejects_mismatch_inputs() {
        let topic_id = [3u8; 32];
        let harbor_id = harbor_id_from_topic(&topic_id);
        let sender_id = [4u8; 32];
        let proof = create_membership_proof(&topic_id, &harbor_id, &sender_id);

        let wrong_topic = [5u8; 32];
        assert!(!verify_membership_proof(
            &wrong_topic,
            &harbor_id,
            &sender_id,
            &proof
        ));

        let wrong_harbor = [6u8; 32];
        assert!(!verify_membership_proof(
            &topic_id,
            &wrong_harbor,
            &sender_id,
            &proof
        ));

        let wrong_sender = [7u8; 32];
        assert!(!verify_membership_proof(
            &topic_id,
            &harbor_id,
            &wrong_sender,
            &proof
        ));
    }

    #[test]
    fn test_membership_proof_rejects_modified_proof() {
        let topic_id = [8u8; 32];
        let harbor_id = harbor_id_from_topic(&topic_id);
        let sender_id = [9u8; 32];
        let mut proof = create_membership_proof(&topic_id, &harbor_id, &sender_id);

        proof[0] ^= 0xFF;
        assert!(!verify_membership_proof(
            &topic_id, &harbor_id, &sender_id, &proof
        ));
    }

    #[test]
    fn test_create_membership_binding() {
        let topic_id = [10u8; 32];
        let sender_id = [11u8; 32];

        let (harbor_id, proof) = create_membership_binding(&topic_id, &sender_id);
        assert_eq!(harbor_id, harbor_id_from_topic(&topic_id));
        assert!(verify_membership_proof(
            &topic_id, &harbor_id, &sender_id, &proof
        ));
    }
}
