//! Seal functions for Harbor storage
//!
//! These functions apply the full crypto stack (encryption + MAC + signature)
//! to raw payloads before sending to Harbor nodes for offline storage.
//!
//! Direct delivery uses QUIC TLS only - these functions are only called
//! during harbor_replication for packets that couldn't be delivered directly.

use crate::security::send::packet::{
    create_packet, create_packet_with_epoch, create_dm_packet, PacketError, SendPacket, EpochKeys,
};

/// Seal a raw topic message for Harbor storage.
///
/// Applies the full crypto stack:
/// - ChaCha20-Poly1305 encryption (with topic-derived key)
/// - BLAKE3 MAC (proves topic membership)
/// - Ed25519 signature (proves sender identity)
///
/// # Arguments
/// * `topic_id` - The topic this message belongs to
/// * `sender_private_key` - Sender's Ed25519 private key
/// * `sender_public_key` - Sender's Ed25519 public key (EndpointID)
/// * `raw_payload` - The raw encoded TopicMessage payload
///
/// # Returns
/// The sealed `SendPacket` ready for Harbor storage
pub fn seal_topic_packet(
    topic_id: &[u8; 32],
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    raw_payload: &[u8],
) -> Result<SendPacket, PacketError> {
    create_packet(topic_id, sender_private_key, sender_public_key, raw_payload)
}

/// Seal a raw topic message with specific epoch keys for Harbor storage.
///
/// For Tier 2 (admin-managed topics with key rotation), use this when
/// sending messages with a non-zero epoch.
///
/// # Arguments
/// * `topic_id` - The topic this message belongs to
/// * `sender_private_key` - Sender's Ed25519 private key
/// * `sender_public_key` - Sender's Ed25519 public key (EndpointID)
/// * `raw_payload` - The raw encoded TopicMessage payload
/// * `epoch_keys` - Epoch-specific encryption keys
///
/// # Returns
/// The sealed `SendPacket` ready for Harbor storage
pub fn seal_topic_packet_with_epoch(
    topic_id: &[u8; 32],
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    raw_payload: &[u8],
    epoch_keys: &EpochKeys,
) -> Result<SendPacket, PacketError> {
    create_packet_with_epoch(topic_id, sender_private_key, sender_public_key, raw_payload, epoch_keys)
}

/// Seal a raw topic message and serialize to bytes for Harbor storage.
///
/// Convenience function that seals and serializes in one step.
///
/// # Arguments
/// * `topic_id` - The topic this message belongs to
/// * `sender_private_key` - Sender's Ed25519 private key
/// * `sender_public_key` - Sender's Ed25519 public key (EndpointID)
/// * `raw_payload` - The raw encoded TopicMessage payload
///
/// # Returns
/// Serialized sealed packet bytes ready for Harbor StoreRequest
pub fn seal_topic_packet_bytes(
    topic_id: &[u8; 32],
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    raw_payload: &[u8],
) -> Result<Vec<u8>, PacketError> {
    let packet = seal_topic_packet(topic_id, sender_private_key, sender_public_key, raw_payload)?;
    packet.to_bytes()
}

/// Seal a raw DM for Harbor storage.
///
/// Applies the DM crypto stack:
/// - ECDH key exchange (Ed25519 → X25519)
/// - XChaCha20-Poly1305 encryption
/// - Ed25519 signature (proves sender identity)
///
/// # Arguments
/// * `sender_private_key` - Sender's Ed25519 private key
/// * `sender_public_key` - Sender's Ed25519 public key (EndpointID)
/// * `recipient_public_key` - Recipient's Ed25519 public key
/// * `raw_payload` - The raw encoded DmMessage payload
///
/// # Returns
/// The sealed `SendPacket` ready for Harbor storage
pub fn seal_dm_packet(
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    recipient_public_key: &[u8; 32],
    raw_payload: &[u8],
) -> Result<SendPacket, PacketError> {
    create_dm_packet(sender_private_key, sender_public_key, recipient_public_key, raw_payload)
}

/// Seal a raw DM and serialize to bytes for Harbor storage.
///
/// Convenience function that seals and serializes in one step.
///
/// # Arguments
/// * `sender_private_key` - Sender's Ed25519 private key
/// * `sender_public_key` - Sender's Ed25519 public key (EndpointID)
/// * `recipient_public_key` - Recipient's Ed25519 public key
/// * `raw_payload` - The raw encoded DmMessage payload
///
/// # Returns
/// Serialized sealed packet bytes ready for Harbor StoreRequest
pub fn seal_dm_packet_bytes(
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    recipient_public_key: &[u8; 32],
    raw_payload: &[u8],
) -> Result<Vec<u8>, PacketError> {
    let packet = seal_dm_packet(sender_private_key, sender_public_key, recipient_public_key, raw_payload)?;
    packet.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::create_key_pair::generate_key_pair;
    use crate::security::send::packet::{verify_and_decrypt_packet, verify_and_decrypt_dm_packet};

    fn test_topic_id() -> [u8; 32] {
        [42u8; 32]
    }

    #[test]
    fn test_seal_topic_packet() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let raw_payload = b"Hello, Harbor!";

        let packet = seal_topic_packet(
            &topic_id,
            &kp.private_key,
            &kp.public_key,
            raw_payload,
        ).unwrap();

        // Should be a valid sealed packet
        assert!(!packet.is_dm());
        assert_eq!(packet.endpoint_id, kp.public_key);

        // Should decrypt correctly
        let decrypted = verify_and_decrypt_packet(&packet, &topic_id).unwrap();
        assert_eq!(decrypted, raw_payload);
    }

    #[test]
    fn test_seal_topic_packet_bytes() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let raw_payload = b"Serialized test";

        let bytes = seal_topic_packet_bytes(
            &topic_id,
            &kp.private_key,
            &kp.public_key,
            raw_payload,
        ).unwrap();

        // Should deserialize back to a valid packet
        let packet = SendPacket::from_bytes(&bytes).unwrap();
        let decrypted = verify_and_decrypt_packet(&packet, &topic_id).unwrap();
        assert_eq!(decrypted, raw_payload);
    }

    #[test]
    fn test_seal_dm_packet() {
        let sender = generate_key_pair();
        let recipient = generate_key_pair();
        let raw_payload = b"Secret DM";

        let packet = seal_dm_packet(
            &sender.private_key,
            &sender.public_key,
            &recipient.public_key,
            raw_payload,
        ).unwrap();

        // Should be a DM packet
        assert!(packet.is_dm());
        assert_eq!(packet.endpoint_id, sender.public_key);
        assert_eq!(packet.harbor_id, recipient.public_key); // DM uses recipient as harbor_id

        // Should decrypt correctly
        let decrypted = verify_and_decrypt_dm_packet(&packet, &recipient.private_key).unwrap();
        assert_eq!(decrypted, raw_payload);
    }

    #[test]
    fn test_seal_dm_packet_bytes() {
        let sender = generate_key_pair();
        let recipient = generate_key_pair();
        let raw_payload = b"Serialized DM";

        let bytes = seal_dm_packet_bytes(
            &sender.private_key,
            &sender.public_key,
            &recipient.public_key,
            raw_payload,
        ).unwrap();

        // Should deserialize back to a valid DM packet
        let packet = SendPacket::from_bytes(&bytes).unwrap();
        let decrypted = verify_and_decrypt_dm_packet(&packet, &recipient.private_key).unwrap();
        assert_eq!(decrypted, raw_payload);
    }
}
