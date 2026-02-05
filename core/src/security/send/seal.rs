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
/// * `packet_id` - Unique packet identifier (same ID used for direct delivery)
///
/// # Returns
/// The sealed `SendPacket` ready for Harbor storage
pub fn seal_topic_packet(
    topic_id: &[u8; 32],
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    raw_payload: &[u8],
    packet_id: [u8; 32],
) -> Result<SendPacket, PacketError> {
    create_packet(topic_id, sender_private_key, sender_public_key, raw_payload, packet_id)
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
/// * `packet_id` - Unique packet identifier (same ID used for direct delivery)
///
/// # Returns
/// The sealed `SendPacket` ready for Harbor storage
pub fn seal_topic_packet_with_epoch(
    topic_id: &[u8; 32],
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    raw_payload: &[u8],
    epoch_keys: &EpochKeys,
    packet_id: [u8; 32],
) -> Result<SendPacket, PacketError> {
    create_packet_with_epoch(topic_id, sender_private_key, sender_public_key, raw_payload, epoch_keys, packet_id)
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
/// * `packet_id` - Unique packet identifier (same ID used for direct delivery)
///
/// # Returns
/// Serialized sealed packet bytes ready for Harbor StoreRequest
pub fn seal_topic_packet_bytes(
    topic_id: &[u8; 32],
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    raw_payload: &[u8],
    packet_id: [u8; 32],
) -> Result<Vec<u8>, PacketError> {
    let packet = seal_topic_packet(topic_id, sender_private_key, sender_public_key, raw_payload, packet_id)?;
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
/// * `packet_id` - Unique packet identifier (same ID used for direct delivery)
///
/// # Returns
/// The sealed `SendPacket` ready for Harbor storage
pub fn seal_dm_packet(
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    recipient_public_key: &[u8; 32],
    raw_payload: &[u8],
    packet_id: [u8; 32],
) -> Result<SendPacket, PacketError> {
    create_dm_packet(sender_private_key, sender_public_key, recipient_public_key, raw_payload, packet_id)
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
/// * `packet_id` - Unique packet identifier (same ID used for direct delivery)
///
/// # Returns
/// Serialized sealed packet bytes ready for Harbor StoreRequest
pub fn seal_dm_packet_bytes(
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    recipient_public_key: &[u8; 32],
    raw_payload: &[u8],
    packet_id: [u8; 32],
) -> Result<Vec<u8>, PacketError> {
    let packet = seal_dm_packet(sender_private_key, sender_public_key, recipient_public_key, raw_payload, packet_id)?;
    packet.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::create_key_pair::generate_key_pair;
    use crate::security::send::packet::{verify_and_decrypt_packet, verify_and_decrypt_dm_packet, generate_packet_id};

    fn test_topic_id() -> [u8; 32] {
        [42u8; 32]
    }

    #[test]
    fn test_seal_topic_packet() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let raw_payload = b"Hello, Harbor!";
        let packet_id = generate_packet_id();

        let packet = seal_topic_packet(
            &topic_id,
            &kp.private_key,
            &kp.public_key,
            raw_payload,
            packet_id,
        ).unwrap();

        // Should be a valid sealed packet
        assert!(!packet.is_dm());
        assert_eq!(packet.endpoint_id, kp.public_key);
        assert_eq!(packet.packet_id, packet_id);

        // Should decrypt correctly
        let decrypted = verify_and_decrypt_packet(&packet, &topic_id).unwrap();
        assert_eq!(decrypted, raw_payload);
    }

    #[test]
    fn test_seal_topic_packet_bytes() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let raw_payload = b"Serialized test";
        let packet_id = generate_packet_id();

        let bytes = seal_topic_packet_bytes(
            &topic_id,
            &kp.private_key,
            &kp.public_key,
            raw_payload,
            packet_id,
        ).unwrap();

        // Should deserialize back to a valid packet
        let packet = SendPacket::from_bytes(&bytes).unwrap();
        assert_eq!(packet.packet_id, packet_id);
        let decrypted = verify_and_decrypt_packet(&packet, &topic_id).unwrap();
        assert_eq!(decrypted, raw_payload);
    }

    #[test]
    fn test_seal_dm_packet() {
        let sender = generate_key_pair();
        let recipient = generate_key_pair();
        let raw_payload = b"Secret DM";
        let packet_id = generate_packet_id();

        let packet = seal_dm_packet(
            &sender.private_key,
            &sender.public_key,
            &recipient.public_key,
            raw_payload,
            packet_id,
        ).unwrap();

        // Should be a DM packet
        assert!(packet.is_dm());
        assert_eq!(packet.endpoint_id, sender.public_key);
        assert_eq!(packet.harbor_id, recipient.public_key); // DM uses recipient as harbor_id
        assert_eq!(packet.packet_id, packet_id);

        // Should decrypt correctly
        let decrypted = verify_and_decrypt_dm_packet(&packet, &recipient.private_key).unwrap();
        assert_eq!(decrypted, raw_payload);
    }

    #[test]
    fn test_seal_dm_packet_bytes() {
        let sender = generate_key_pair();
        let recipient = generate_key_pair();
        let raw_payload = b"Serialized DM";
        let packet_id = generate_packet_id();

        let bytes = seal_dm_packet_bytes(
            &sender.private_key,
            &sender.public_key,
            &recipient.public_key,
            raw_payload,
            packet_id,
        ).unwrap();

        // Should deserialize back to a valid DM packet
        let packet = SendPacket::from_bytes(&bytes).unwrap();
        assert_eq!(packet.packet_id, packet_id);
        let decrypted = verify_and_decrypt_dm_packet(&packet, &recipient.private_key).unwrap();
        assert_eq!(decrypted, raw_payload);
    }
}
