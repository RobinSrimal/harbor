//! Send packet creation and verification
//!
//! Implements the Harbor packet structure from the Proposal:
//! ```text
//! packet = {
//!     PacketID,
//!     HarborID,
//!     EndpointID,
//!     epoch,       // Reserved for future tiers (always 0 for Tier 1)
//!     nonce,
//!     ciphertext,
//!     aead_tag,  // included in ciphertext for ChaCha20-Poly1305
//!     mac,
//!     signature
//! }
//! ```
//!
//! # Security Tiers
//!
//! ## Tier 1 (Current): Open Topics
//! - Keys derived solely from `topic_id`
//! - Epoch always 0
//! - No eviction, no forward secrecy
//! - Anyone with `topic_id` can participate
//!
//! ## Tier 2 (Future): Admin-Managed
//! - Keys derived from `topic_id` + admin-distributed epoch secret
//! - Admin can evict by rotating epoch secret
//! - Forward/backward secrecy via epoch rotation
//!
//! ## Tier 3 (Future): Full MLS
//! - Full MLS/TreeKEM protocol
//! - Post-compromise security
//!
//! # Verification Layers
//!
//! Different entities can verify different aspects:
//!
//! | Verifier | MAC | Signature | Decrypt |
//! |----------|-----|-----------|---------|
//! | **Topic Member** | ✅ | ✅ | ✅ |
//! | **Harbor Node** | ❌ | ✅ | ❌ |
//!
//! Harbor Nodes can verify signatures (proves sender created packet) but
//! cannot verify MACs or decrypt (requires topic_id which they don't have).

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::security::harbor::{
    authenticate::create_mac,
    encrypt::{decrypt, encrypt, generate_nonce},
    sign::{sign_packet, verify_packet},
};
use crate::security::topic_keys::harbor_id_from_topic;

/// Maximum packet payload size (512 KB)
pub const MAX_PAYLOAD_SIZE: usize = 512 * 1024;

/// Packet ID size (32 bytes = 256 bits, consistent with other IDs)
pub const PACKET_ID_SIZE: usize = 32;

/// Error during packet creation or verification
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    /// Payload exceeds maximum size
    PayloadTooLarge { size: usize, max: usize },
    /// MAC verification failed (packet not from topic member)
    InvalidMac,
    /// Signature verification failed (sender identity mismatch)
    InvalidSignature,
    /// AEAD decryption failed (tampered or wrong keys)
    DecryptionFailed,
    /// Invalid packet structure
    InvalidFormat(String),
    /// Serialization failed
    SerializationFailed(String),
    /// HarborID doesn't match expected value
    HarborIdMismatch,
    /// Sender ID in request doesn't match packet
    SenderIdMismatch,
    /// Epoch mismatch between packet and provided keys
    EpochMismatch { packet_epoch: u64, key_epoch: u64 },
    /// No epoch key available for this packet's epoch
    NoEpochKey { epoch: u64 },
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacketError::PayloadTooLarge { size, max } => {
                write!(f, "payload size {} exceeds maximum {}", size, max)
            }
            PacketError::InvalidMac => write!(f, "MAC verification failed"),
            PacketError::InvalidSignature => write!(f, "signature verification failed"),
            PacketError::DecryptionFailed => write!(f, "decryption failed"),
            PacketError::InvalidFormat(msg) => write!(f, "invalid packet format: {}", msg),
            PacketError::SerializationFailed(msg) => write!(f, "serialization failed: {}", msg),
            PacketError::HarborIdMismatch => write!(f, "HarborID mismatch"),
            PacketError::SenderIdMismatch => write!(f, "sender ID mismatch"),
            PacketError::EpochMismatch { packet_epoch, key_epoch } => {
                write!(f, "epoch mismatch: packet epoch {} != key epoch {}", packet_epoch, key_epoch)
            }
            PacketError::NoEpochKey { epoch } => {
                write!(f, "no epoch key available for epoch {}", epoch)
            }
        }
    }
}

impl std::error::Error for PacketError {}

/// A Send packet ready for transmission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendPacket {
    /// Unique packet identifier (16 bytes)
    pub packet_id: [u8; PACKET_ID_SIZE],
    /// Hash of TopicID - used to find Harbor Nodes
    pub harbor_id: [u8; 32],
    /// Sender's EndpointID (public key)
    pub endpoint_id: [u8; 32],
    /// Epoch number for key derivation
    /// - Tier 1: Always 0 (keys from topic_id only)
    /// - Tier 2+: Increments with membership changes (forward secrecy)
    #[serde(default)]
    pub epoch: u64,
    /// Nonce used for AEAD encryption (12 bytes)
    pub nonce: [u8; 12],
    /// Encrypted payload (includes 16-byte AEAD tag)
    pub ciphertext: Vec<u8>,
    /// MAC over ciphertext for topic authentication (32 bytes)
    pub mac: [u8; 32],
    /// Ed25519 signature for sender verification (64 bytes)
    #[serde(with = "BigArray")]
    pub signature: [u8; 64],
}

impl SendPacket {
    /// Get the size of the packet in bytes (approximate)
    pub fn size(&self) -> usize {
        PACKET_ID_SIZE + 32 + 32 + 8 + 12 + self.ciphertext.len() + 32 + 64
    }

    /// Serialize the packet for transmission
    ///
    /// Returns an error if serialization fails (should be rare).
    pub fn to_bytes(&self) -> Result<Vec<u8>, PacketError> {
        postcard::to_allocvec(self)
            .map_err(|e| PacketError::SerializationFailed(e.to_string()))
    }

    /// Deserialize a packet from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PacketError> {
        postcard::from_bytes(bytes).map_err(|e| PacketError::InvalidFormat(e.to_string()))
    }
}

/// Epoch encryption keys
/// 
/// For Tier 1: Derived from `topic_id` with epoch 0
/// For Tier 2+: Derived from epoch secret distributed by admin
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct EpochKeys {
    /// Epoch number these keys are for
    #[zeroize(skip)]
    pub epoch: u64,
    /// 32-byte encryption key for AEAD (ChaCha20-Poly1305)
    pub k_enc: [u8; 32],
    /// 32-byte MAC key for HMAC authentication
    pub k_mac: [u8; 32],
}

impl EpochKeys {
    /// Create new epoch keys
    pub fn new(epoch: u64, k_enc: [u8; 32], k_mac: [u8; 32]) -> Self {
        Self { epoch, k_enc, k_mac }
    }

    /// Derive epoch keys from an epoch secret
    /// 
    /// For Tier 2: The epoch secret is distributed by the topic admin.
    /// For Tier 3: The epoch secret comes from MLS export_secret().
    /// 
    /// We derive separate encryption and MAC keys from it, incorporating
    /// the epoch number into the derivation for domain separation.
    pub fn derive_from_secret(epoch: u64, epoch_secret: &[u8; 32]) -> Self {
        use blake3::Hasher;
        
        // Derive k_enc: hash(epoch_secret || epoch || "enc")
        let mut hasher = Hasher::new_derive_key("harbor epoch encryption key");
        hasher.update(epoch_secret);
        hasher.update(&epoch.to_le_bytes());
        let k_enc_hash = hasher.finalize();
        let mut k_enc = [0u8; 32];
        k_enc.copy_from_slice(&k_enc_hash.as_bytes()[..32]);
        
        // Derive k_mac: hash(epoch_secret || epoch || "mac")
        let mut hasher = Hasher::new_derive_key("harbor epoch mac key");
        hasher.update(epoch_secret);
        hasher.update(&epoch.to_le_bytes());
        let k_mac_hash = hasher.finalize();
        let mut k_mac = [0u8; 32];
        k_mac.copy_from_slice(&k_mac_hash.as_bytes()[..32]);
        
        Self { epoch, k_enc, k_mac }
    }

    /// Derive keys from TopicID and epoch
    /// 
    /// This is the primary method for Tier 1 (Open Topics).
    /// For Tier 1, epoch is always 0.
    /// For Tier 2+, epoch increments with membership changes.
    pub fn derive_from_topic(topic_id: &[u8; 32], epoch: u64) -> Self {
        use blake3::Hasher;
        
        // Derive epoch secret from topic_id and epoch
        let mut hasher = Hasher::new();
        hasher.update(b"harbor-epoch-secret");
        hasher.update(topic_id);
        hasher.update(&epoch.to_le_bytes());
        let epoch_secret_hash = hasher.finalize();
        let mut epoch_secret = [0u8; 32];
        epoch_secret.copy_from_slice(&epoch_secret_hash.as_bytes()[..32]);
        
        Self::derive_from_secret(epoch, &epoch_secret)
    }
}

impl std::fmt::Debug for EpochKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EpochKeys")
            .field("epoch", &self.epoch)
            .field("k_enc", &"[REDACTED]")
            .field("k_mac", &"[REDACTED]")
            .finish()
    }
}

/// Builder for creating Send packets
///
/// The private key is zeroized when the builder is dropped.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct PacketBuilder {
    topic_id: [u8; 32],
    sender_private_key: [u8; 32],
    #[zeroize(skip)]
    sender_public_key: [u8; 32],
}

impl PacketBuilder {
    /// Create a new packet builder
    ///
    /// # Arguments
    /// * `topic_id` - The topic this packet belongs to
    /// * `sender_private_key` - Sender's Ed25519 private key for signing
    /// * `sender_public_key` - Sender's Ed25519 public key (EndpointID)
    pub fn new(
        topic_id: [u8; 32],
        sender_private_key: [u8; 32],
        sender_public_key: [u8; 32],
    ) -> Self {
        Self {
            topic_id,
            sender_private_key,
            sender_public_key,
        }
    }

    /// Build a packet using epoch keys
    /// 
    /// For Tier 1: Use with `EpochKeys::derive_from_topic(topic_id, 0)`
    /// For Tier 2+: Use epoch keys from the current epoch secret
    pub fn build_with_epoch(&self, plaintext: &[u8], epoch_keys: &EpochKeys) -> Result<SendPacket, PacketError> {
        // Check payload size
        if plaintext.len() > MAX_PAYLOAD_SIZE {
            return Err(PacketError::PayloadTooLarge {
                size: plaintext.len(),
                max: MAX_PAYLOAD_SIZE,
            });
        }

        // Generate packet ID and nonce
        let packet_id = generate_packet_id();
        let nonce = generate_nonce();

        // Compute HarborID
        let harbor_id = harbor_id_from_topic(&self.topic_id);

        // Step 1: AEAD encrypt with AAD = HarborID || EndpointID || epoch
        let mut aad = Vec::with_capacity(72);
        aad.extend_from_slice(&harbor_id);
        aad.extend_from_slice(&self.sender_public_key);
        aad.extend_from_slice(&epoch_keys.epoch.to_le_bytes());

        let ciphertext = encrypt(&epoch_keys.k_enc, &nonce, plaintext, &aad)
            .map_err(|_| PacketError::DecryptionFailed)?;

        // Step 2: Create MAC over ciphertext (includes epoch in binding)
        let mac_hash = create_mac(&epoch_keys.k_mac, &ciphertext);
        let mut mac = [0u8; 32];
        mac.copy_from_slice(mac_hash.as_bytes());

        // Step 3: Sign the packet
        let signature = sign_packet(
            &self.sender_private_key,
            &self.sender_public_key,
            &nonce,
            &ciphertext,
        );

        Ok(SendPacket {
            packet_id,
            harbor_id,
            endpoint_id: self.sender_public_key,
            epoch: epoch_keys.epoch,
            nonce,
            ciphertext,
            mac,
            signature,
        })
    }

    /// Build a packet from plaintext payload (Tier 1 mode)
    /// 
    /// Uses TopicID-derived keys with epoch 0. This is the standard
    /// method for Tier 1 (Open Topics).
    pub fn build(&self, plaintext: &[u8]) -> Result<SendPacket, PacketError> {
        // Use epoch 0 with topic-derived keys for backward compatibility
        let epoch_keys = EpochKeys::derive_from_topic(&self.topic_id, 0);
        self.build_with_epoch(plaintext, &epoch_keys)
    }
}

/// Generate a random packet ID
fn generate_packet_id() -> [u8; PACKET_ID_SIZE] {
    let mut id = [0u8; PACKET_ID_SIZE];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut id);
    id
}

/// Create a Send packet with epoch keys (convenience function)
///
/// For Tier 2+: Use with epoch keys from the current epoch secret.
///
/// # Arguments
/// * `topic_id` - The topic this packet belongs to
/// * `sender_private_key` - Sender's Ed25519 private key
/// * `sender_public_key` - Sender's EndpointID
/// * `plaintext` - The message payload
/// * `epoch_keys` - Epoch-specific encryption keys
pub fn create_packet_with_epoch(
    topic_id: &[u8; 32],
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    plaintext: &[u8],
    epoch_keys: &EpochKeys,
) -> Result<SendPacket, PacketError> {
    PacketBuilder::new(*topic_id, *sender_private_key, *sender_public_key)
        .build_with_epoch(plaintext, epoch_keys)
}

/// Create a Send packet (Tier 1 convenience function)
///
/// Uses keys derived from TopicID with epoch 0.
/// This is the standard method for Tier 1 (Open Topics).
///
/// # Arguments
/// * `topic_id` - The topic this packet belongs to
/// * `sender_private_key` - Sender's Ed25519 private key
/// * `sender_public_key` - Sender's EndpointID
/// * `plaintext` - The message payload
pub fn create_packet(
    topic_id: &[u8; 32],
    sender_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    plaintext: &[u8],
) -> Result<SendPacket, PacketError> {
    PacketBuilder::new(*topic_id, *sender_private_key, *sender_public_key).build(plaintext)
}

/// Verification mode for packets
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    /// Full verification: MAC + Signature (for content and leave messages)
    Full,
    /// MAC-only verification (for join messages from unknown senders)
    MacOnly,
}

/// Verify and decrypt a received packet using epoch keys
///
/// For Tier 2+: Use with epoch keys matching the packet's epoch.
///
/// # Arguments
/// * `packet` - The received packet
/// * `topic_id` - The expected topic (to verify HarborID)
/// * `epoch_keys` - Epoch keys for decryption (must match packet.epoch)
/// * `mode` - Verification mode (Full or MacOnly)
///
/// # Returns
/// The decrypted plaintext if all verifications pass
pub fn verify_and_decrypt_with_epoch(
    packet: &SendPacket,
    topic_id: &[u8; 32],
    epoch_keys: &EpochKeys,
    mode: VerificationMode,
) -> Result<Vec<u8>, PacketError> {
    // Verify HarborID matches
    let expected_harbor_id = harbor_id_from_topic(topic_id);
    if packet.harbor_id != expected_harbor_id {
        return Err(PacketError::HarborIdMismatch);
    }

    // Verify epoch matches
    if packet.epoch != epoch_keys.epoch {
        return Err(PacketError::EpochMismatch {
            packet_epoch: packet.epoch,
            key_epoch: epoch_keys.epoch,
        });
    }

    // Step 1: Verify MAC (always required - proves topic access)
    // Use constant-time comparison to prevent timing attacks
    let mac_hash = create_mac(&epoch_keys.k_mac, &packet.ciphertext);
    let expected_mac = mac_hash.as_bytes();

    if packet.mac.ct_eq(expected_mac).unwrap_u8() != 1 {
        return Err(PacketError::InvalidMac);
    }

    // Step 2: Verify signature (only in Full mode)
    if mode == VerificationMode::Full {
        if !verify_packet(
            &packet.endpoint_id,
            &packet.endpoint_id,
            &packet.nonce,
            &packet.ciphertext,
            &packet.signature,
        ) {
            return Err(PacketError::InvalidSignature);
        }
    }

    // Step 3: Decrypt with AAD = HarborID || EndpointID || epoch
    let mut aad = Vec::with_capacity(72);
    aad.extend_from_slice(&packet.harbor_id);
    aad.extend_from_slice(&packet.endpoint_id);
    aad.extend_from_slice(&epoch_keys.epoch.to_le_bytes());

    let plaintext = decrypt(&epoch_keys.k_enc, &packet.nonce, &packet.ciphertext, &aad)
        .map_err(|_| PacketError::DecryptionFailed)?;

    Ok(plaintext)
}

/// Verify and decrypt a received packet (Tier 1 mode)
///
/// Uses keys derived from TopicID with epoch 0.
/// This is the standard method for Tier 1 (Open Topics).
///
/// Verification order (as specified in Proposal):
/// 1. Verify MAC (ensures packet is from topic member)
/// 2. Verify signature (ensures sender identity) - skipped in MacOnly mode
/// 3. Decrypt ciphertext
///
/// # Arguments
/// * `packet` - The received packet
/// * `topic_id` - The expected topic (to derive keys)
///
/// # Returns
/// The decrypted plaintext if all verifications pass
pub fn verify_and_decrypt_packet(
    packet: &SendPacket,
    topic_id: &[u8; 32],
) -> Result<Vec<u8>, PacketError> {
    verify_and_decrypt_packet_with_mode(packet, topic_id, VerificationMode::Full)
}

/// Verify and decrypt a packet with specified verification mode
///
/// Use `VerificationMode::MacOnly` for join packets where the sender's
/// public key is not yet known to the recipient.
///
/// Uses keys derived from TopicID with packet's epoch.
///
/// # Arguments
/// * `packet` - The received packet
/// * `topic_id` - The expected topic (to derive keys)
/// * `mode` - Verification mode (Full or MacOnly)
///
/// # Returns
/// The decrypted plaintext if verifications pass
pub fn verify_and_decrypt_packet_with_mode(
    packet: &SendPacket,
    topic_id: &[u8; 32],
    mode: VerificationMode,
) -> Result<Vec<u8>, PacketError> {
    // Derive epoch keys from topic_id and packet's epoch
    let epoch_keys = EpochKeys::derive_from_topic(topic_id, packet.epoch);
    verify_and_decrypt_with_epoch(packet, topic_id, &epoch_keys, mode)
}

/// Verify a packet's MAC only (for topic members doing quick validation).
///
/// This is useful when you have the epoch keys and want to quickly check
/// if a packet is from a topic member without full decryption.
///
/// Uses constant-time comparison to prevent timing attacks.
///
/// # Arguments
/// * `packet` - The packet to verify
/// * `epoch_keys` - The epoch keys (must match packet's epoch)
///
/// # Returns
/// `true` if the MAC is valid and epoch matches
pub fn verify_mac_only(packet: &SendPacket, epoch_keys: &EpochKeys) -> bool {
    if packet.epoch != epoch_keys.epoch {
        return false;
    }
    let mac_hash = create_mac(&epoch_keys.k_mac, &packet.ciphertext);
    let expected_mac = mac_hash.as_bytes();
    packet.mac.ct_eq(expected_mac).unwrap_u8() == 1
}

/// Verify a packet's MAC with raw MAC key (legacy)
///
/// For backward compatibility when using TopicKeys directly.
///
/// # Arguments
/// * `packet` - The packet to verify
/// * `k_mac` - The MAC key
///
/// # Returns
/// `true` if the MAC is valid
pub fn verify_mac_only_raw(packet: &SendPacket, k_mac: &[u8; 32]) -> bool {
    let mac_hash = create_mac(k_mac, &packet.ciphertext);
    let expected_mac = mac_hash.as_bytes();
    packet.mac.ct_eq(expected_mac).unwrap_u8() == 1
}

/// Verify a packet for Harbor Node storage.
///
/// Harbor Nodes can verify:
/// - Packet format is valid
/// - Signature is valid (proves the sender created this packet)
/// - HarborID in packet matches the claimed harbor_id
/// - Sender ID in packet matches the claimed sender_id
/// - Packet size is within limits
///
/// Harbor Nodes CANNOT verify:
/// - MAC (requires topic_id to derive k_mac)
/// - Decryption (requires topic_id to derive k_enc)
///
/// # Arguments
/// * `packet_bytes` - Raw packet bytes from store request
/// * `claimed_harbor_id` - HarborID from the store request
/// * `claimed_sender_id` - Sender ID from the store request
///
/// # Returns
/// The parsed `SendPacket` if all verifications pass, or an error.
pub fn verify_for_storage(
    packet_bytes: &[u8],
    claimed_harbor_id: &[u8; 32],
    claimed_sender_id: &[u8; 32],
) -> Result<SendPacket, PacketError> {
    // Check packet size before parsing
    // Minimum: packet_id(32) + harbor_id(32) + endpoint_id(32) + nonce(12) +
    //          ciphertext(16 min for tag) + mac(32) + signature(64) = 220 bytes
    if packet_bytes.len() < 220 {
        return Err(PacketError::InvalidFormat("packet too small".to_string()));
    }

    // Check maximum size (header + max payload + AEAD tag)
    let max_packet_size = 220 + MAX_PAYLOAD_SIZE;
    if packet_bytes.len() > max_packet_size {
        return Err(PacketError::PayloadTooLarge {
            size: packet_bytes.len(),
            max: max_packet_size,
        });
    }

    // Parse the packet
    let packet = SendPacket::from_bytes(packet_bytes)?;

    // Verify HarborID matches claim
    if packet.harbor_id != *claimed_harbor_id {
        tracing::warn!(
            packet_harbor = %hex::encode(packet.harbor_id),
            claimed_harbor = %hex::encode(claimed_harbor_id),
            "Harbor storage: HarborID mismatch"
        );
        return Err(PacketError::HarborIdMismatch);
    }

    // Verify sender matches claim
    if packet.endpoint_id != *claimed_sender_id {
        tracing::warn!(
            packet_sender = %hex::encode(packet.endpoint_id),
            claimed_sender = %hex::encode(claimed_sender_id),
            "Harbor storage: sender ID mismatch"
        );
        return Err(PacketError::SenderIdMismatch);
    }

    // Verify signature (this is what Harbor Nodes CAN verify!)
    if !verify_packet(
        &packet.endpoint_id,
        &packet.endpoint_id,
        &packet.nonce,
        &packet.ciphertext,
        &packet.signature,
    ) {
        tracing::warn!(
            sender = %hex::encode(packet.endpoint_id),
            "Harbor storage: invalid signature"
        );
        return Err(PacketError::InvalidSignature);
    }

    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::create_key_pair::generate_key_pair;

    fn test_topic_id() -> [u8; 32] {
        [42u8; 32]
    }

    #[test]
    fn test_create_and_verify_packet() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Hello, Harbor!";

        // Create packet
        let packet = create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Verify fields
        assert_eq!(packet.harbor_id, harbor_id_from_topic(&topic_id));
        assert_eq!(packet.endpoint_id, kp.public_key);
        assert_eq!(packet.packet_id.len(), PACKET_ID_SIZE);
        assert_eq!(packet.nonce.len(), 12);
        assert_eq!(packet.mac.len(), 32);
        assert_eq!(packet.signature.len(), 64);

        // Verify and decrypt
        let decrypted = verify_and_decrypt_packet(&packet, &topic_id).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_packet_serialization() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Test message";

        let packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Serialize and deserialize
        let bytes = packet.to_bytes().unwrap();
        let restored = SendPacket::from_bytes(&bytes).unwrap();

        assert_eq!(restored.packet_id, packet.packet_id);
        assert_eq!(restored.harbor_id, packet.harbor_id);
        assert_eq!(restored.endpoint_id, packet.endpoint_id);
        assert_eq!(restored.nonce, packet.nonce);
        assert_eq!(restored.ciphertext, packet.ciphertext);
        assert_eq!(restored.mac, packet.mac);
        assert_eq!(restored.signature, packet.signature);

        // Should still verify
        let decrypted = verify_and_decrypt_packet(&restored, &topic_id).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_payload_too_large() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let huge_payload = vec![0u8; MAX_PAYLOAD_SIZE + 1];

        let result = create_packet(&topic_id, &kp.private_key, &kp.public_key, &huge_payload);

        assert!(matches!(result, Err(PacketError::PayloadTooLarge { .. })));
    }

    #[test]
    fn test_max_payload_size_allowed() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let max_payload = vec![0u8; MAX_PAYLOAD_SIZE];

        let result = create_packet(&topic_id, &kp.private_key, &kp.public_key, &max_payload);
        assert!(result.is_ok());
    }

    #[test]
    fn test_wrong_topic_fails_mac() {
        let topic_id = test_topic_id();
        let wrong_topic = [99u8; 32];
        let kp = generate_key_pair();
        let plaintext = b"Secret message";

        let packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Verify with wrong topic should fail at HarborID mismatch
        let result = verify_and_decrypt_packet(&packet, &wrong_topic);
        assert!(matches!(result, Err(PacketError::HarborIdMismatch)));
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Secret message";

        let mut packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Tamper with ciphertext
        packet.ciphertext[0] ^= 0xFF;

        // Should fail MAC verification
        let result = verify_and_decrypt_packet(&packet, &topic_id);
        assert!(matches!(result, Err(PacketError::InvalidMac)));
    }

    #[test]
    fn test_tampered_mac_fails() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Secret message";

        let mut packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Tamper with MAC
        packet.mac[0] ^= 0xFF;

        let result = verify_and_decrypt_packet(&packet, &topic_id);
        assert!(matches!(result, Err(PacketError::InvalidMac)));
    }

    #[test]
    fn test_tampered_signature_fails() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Secret message";

        let mut packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Tamper with signature
        packet.signature[0] ^= 0xFF;

        let result = verify_and_decrypt_packet(&packet, &topic_id);
        assert!(matches!(result, Err(PacketError::InvalidSignature)));
    }

    #[test]
    fn test_wrong_sender_key_fails_signature() {
        let topic_id = test_topic_id();
        let kp1 = generate_key_pair();
        let kp2 = generate_key_pair();
        let plaintext = b"Secret message";

        let mut packet =
            create_packet(&topic_id, &kp1.private_key, &kp1.public_key, plaintext).unwrap();

        // Replace endpoint_id with different key
        packet.endpoint_id = kp2.public_key;

        let result = verify_and_decrypt_packet(&packet, &topic_id);
        // Will fail at either MAC (because AAD includes endpoint_id) or signature
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_payload() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"";

        let packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();
        let decrypted = verify_and_decrypt_packet(&packet, &topic_id).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_packet_size() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Hello, Harbor!";

        let packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Size should be reasonable
        let size = packet.size();
        // packet_id(16) + harbor_id(32) + endpoint_id(32) + nonce(12) +
        // ciphertext(plaintext + 16 tag) + mac(32) + signature(64)
        let expected_min = 16 + 32 + 32 + 12 + plaintext.len() + 16 + 32 + 64;
        assert!(size >= expected_min);
    }

    #[test]
    fn test_mac_only_verification_mode() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Join message";

        let mut packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Tamper with signature
        packet.signature[0] ^= 0xFF;

        // Full mode should fail
        let result = verify_and_decrypt_packet_with_mode(&packet, &topic_id, VerificationMode::Full);
        assert!(matches!(result, Err(PacketError::InvalidSignature)));

        // MAC-only mode should succeed (signature not checked)
        let result =
            verify_and_decrypt_packet_with_mode(&packet, &topic_id, VerificationMode::MacOnly);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), plaintext);
    }

    #[test]
    fn test_verify_mac_only_function() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Test message";

        let packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Get epoch keys for epoch 0 (default)
        let epoch_keys = EpochKeys::derive_from_topic(&topic_id, 0);

        // Valid MAC should verify
        assert!(verify_mac_only(&packet, &epoch_keys));

        // Wrong epoch should fail
        let wrong_epoch_keys = EpochKeys::derive_from_topic(&topic_id, 1);
        assert!(!verify_mac_only(&packet, &wrong_epoch_keys));

        // Test raw MAC verification
        assert!(verify_mac_only_raw(&packet, &epoch_keys.k_mac));
        let wrong_key = [0u8; 32];
        assert!(!verify_mac_only_raw(&packet, &wrong_key));
    }

    #[test]
    fn test_epoch_based_encryption() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Forward secrecy message";

        // Create packet with epoch 5
        let epoch_keys = EpochKeys::derive_from_topic(&topic_id, 5);
        let packet = create_packet_with_epoch(
            &topic_id,
            &kp.private_key,
            &kp.public_key,
            plaintext,
            &epoch_keys,
        ).unwrap();

        assert_eq!(packet.epoch, 5);

        // Should decrypt with correct epoch keys
        let decrypted = verify_and_decrypt_with_epoch(
            &packet,
            &topic_id,
            &epoch_keys,
            VerificationMode::Full,
        ).unwrap();
        assert_eq!(decrypted, plaintext);

        // Should fail with wrong epoch keys
        let wrong_epoch_keys = EpochKeys::derive_from_topic(&topic_id, 4);
        let result = verify_and_decrypt_with_epoch(
            &packet,
            &topic_id,
            &wrong_epoch_keys,
            VerificationMode::Full,
        );
        assert!(matches!(result, Err(PacketError::EpochMismatch { .. })));
    }

    #[test]
    fn test_different_epochs_different_keys() {
        let topic_id = test_topic_id();

        let keys_0 = EpochKeys::derive_from_topic(&topic_id, 0);
        let keys_1 = EpochKeys::derive_from_topic(&topic_id, 1);
        let keys_2 = EpochKeys::derive_from_topic(&topic_id, 2);

        // Each epoch should have different keys
        assert_ne!(keys_0.k_enc, keys_1.k_enc);
        assert_ne!(keys_1.k_enc, keys_2.k_enc);
        assert_ne!(keys_0.k_mac, keys_1.k_mac);
        assert_ne!(keys_1.k_mac, keys_2.k_mac);

        // Same epoch should produce same keys (deterministic)
        let keys_1_again = EpochKeys::derive_from_topic(&topic_id, 1);
        assert_eq!(keys_1.k_enc, keys_1_again.k_enc);
        assert_eq!(keys_1.k_mac, keys_1_again.k_mac);
    }

    #[test]
    fn test_late_joiner_cannot_decrypt_old_epoch() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Secret from epoch 1";

        // Member sends message at epoch 1
        let epoch_1_keys = EpochKeys::derive_from_topic(&topic_id, 1);
        let packet = create_packet_with_epoch(
            &topic_id,
            &kp.private_key,
            &kp.public_key,
            plaintext,
            &epoch_1_keys,
        ).unwrap();

        // Late joiner who only has epoch 5 keys cannot decrypt
        let epoch_5_keys = EpochKeys::derive_from_topic(&topic_id, 5);
        let result = verify_and_decrypt_with_epoch(
            &packet,
            &topic_id,
            &epoch_5_keys,
            VerificationMode::Full,
        );
        // Epoch mismatch - they don't have the right key
        assert!(matches!(result, Err(PacketError::EpochMismatch { .. })));
    }

    #[test]
    fn test_verify_for_storage_valid() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Test message";

        let packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();
        let packet_bytes = packet.to_bytes().unwrap();

        // Should verify successfully
        let result = verify_for_storage(&packet_bytes, &packet.harbor_id, &packet.endpoint_id);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_for_storage_wrong_harbor_id() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Test message";

        let packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();
        let packet_bytes = packet.to_bytes().unwrap();

        let wrong_harbor_id = [0u8; 32];
        let result = verify_for_storage(&packet_bytes, &wrong_harbor_id, &packet.endpoint_id);
        assert!(matches!(result, Err(PacketError::HarborIdMismatch)));
    }

    #[test]
    fn test_verify_for_storage_wrong_sender() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Test message";

        let packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();
        let packet_bytes = packet.to_bytes().unwrap();

        let wrong_sender = [0u8; 32];
        let result = verify_for_storage(&packet_bytes, &packet.harbor_id, &wrong_sender);
        assert!(matches!(result, Err(PacketError::SenderIdMismatch)));
    }

    #[test]
    fn test_verify_for_storage_invalid_signature() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Test message";

        let mut packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Tamper with signature
        packet.signature[0] ^= 0xFF;

        let packet_bytes = packet.to_bytes().unwrap();
        let result = verify_for_storage(&packet_bytes, &packet.harbor_id, &packet.endpoint_id);
        assert!(matches!(result, Err(PacketError::InvalidSignature)));
    }

    #[test]
    fn test_verify_for_storage_packet_too_small() {
        let tiny_packet = vec![0u8; 100];
        let harbor_id = [0u8; 32];
        let sender_id = [0u8; 32];

        let result = verify_for_storage(&tiny_packet, &harbor_id, &sender_id);
        assert!(matches!(result, Err(PacketError::InvalidFormat(_))));
    }

    #[test]
    fn test_verify_for_storage_packet_too_large() {
        // Create a packet that's way too large
        let huge_packet = vec![0u8; MAX_PAYLOAD_SIZE + 1000];
        let harbor_id = [0u8; 32];
        let sender_id = [0u8; 32];

        let result = verify_for_storage(&huge_packet, &harbor_id, &sender_id);
        assert!(matches!(result, Err(PacketError::PayloadTooLarge { .. })));
    }

    #[test]
    fn test_invalid_bytes_deserialization() {
        let garbage = vec![0xFF; 300];
        let result = SendPacket::from_bytes(&garbage);
        assert!(matches!(result, Err(PacketError::InvalidFormat(_))));
    }

    #[test]
    fn test_harbor_id_mismatch_error() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Test message";

        let mut packet =
            create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();

        // Tamper with harbor_id
        packet.harbor_id = [0u8; 32];

        let result = verify_and_decrypt_packet(&packet, &topic_id);
        assert!(matches!(result, Err(PacketError::HarborIdMismatch)));
    }

    #[test]
    fn test_error_display() {
        assert!(PacketError::InvalidMac.to_string().contains("MAC"));
        assert!(PacketError::InvalidSignature.to_string().contains("signature"));
        assert!(PacketError::DecryptionFailed.to_string().contains("decryption"));
        assert!(PacketError::HarborIdMismatch.to_string().contains("HarborID"));
        assert!(PacketError::SenderIdMismatch.to_string().contains("sender"));

        let payload_err = PacketError::PayloadTooLarge { size: 100, max: 50 };
        assert!(payload_err.to_string().contains("100"));
        assert!(payload_err.to_string().contains("50"));

        let epoch_err = PacketError::EpochMismatch { packet_epoch: 5, key_epoch: 3 };
        assert!(epoch_err.to_string().contains("epoch"));
        assert!(epoch_err.to_string().contains("5"));
        assert!(epoch_err.to_string().contains("3"));

        let no_key_err = PacketError::NoEpochKey { epoch: 7 };
        assert!(no_key_err.to_string().contains("epoch"));
        assert!(no_key_err.to_string().contains("7"));
    }

    // =========================================================================
    // Forward Secrecy Tests - Epoch-Based Encryption (Tier 2+)
    // =========================================================================

    #[test]
    fn test_forward_secrecy_late_joiner_cannot_decrypt() {
        // Scenario (Tier 2+): Alice sends message at epoch 1
        // Bob joins at epoch 5 and only has epoch 5 keys
        // Bob should NOT be able to decrypt Alice's epoch 1 message
        
        let topic_id = test_topic_id();
        let alice = generate_key_pair();
        let secret_message = b"This message was sent before Bob joined";

        // Alice encrypts at epoch 1
        let epoch_1_keys = EpochKeys::derive_from_topic(&topic_id, 1);
        let packet = create_packet_with_epoch(
            &topic_id,
            &alice.private_key,
            &alice.public_key,
            secret_message,
            &epoch_1_keys,
        ).unwrap();

        // Bob only has epoch 5 keys (joined at epoch 5)
        let epoch_5_keys = EpochKeys::derive_from_topic(&topic_id, 5);
        
        // Bob attempts to decrypt - should fail with EpochMismatch
        let result = verify_and_decrypt_with_epoch(
            &packet,
            &topic_id,
            &epoch_5_keys,
            VerificationMode::Full,
        );
        
        assert!(matches!(result, Err(PacketError::EpochMismatch { 
            packet_epoch: 1, 
            key_epoch: 5 
        })));
    }

    #[test]
    fn test_forward_secrecy_removed_member_cannot_decrypt_future() {
        // Scenario: Alice is removed at epoch 3
        // Charlie sends message at epoch 4
        // Alice (with only epoch 0-3 keys) cannot decrypt epoch 4 messages
        
        let topic_id = test_topic_id();
        let charlie = generate_key_pair();
        let message = b"Message after Alice was removed";

        // Charlie encrypts at epoch 4 (after Alice removed)
        let epoch_4_keys = EpochKeys::derive_from_topic(&topic_id, 4);
        let packet = create_packet_with_epoch(
            &topic_id,
            &charlie.private_key,
            &charlie.public_key,
            message,
            &epoch_4_keys,
        ).unwrap();

        // Alice only has up to epoch 3 keys
        let epoch_3_keys = EpochKeys::derive_from_topic(&topic_id, 3);
        
        // Alice attempts to decrypt - should fail
        let result = verify_and_decrypt_with_epoch(
            &packet,
            &topic_id,
            &epoch_3_keys,
            VerificationMode::Full,
        );
        
        assert!(matches!(result, Err(PacketError::EpochMismatch { 
            packet_epoch: 4, 
            key_epoch: 3 
        })));
    }

    #[test]
    fn test_epoch_keys_cryptographic_isolation() {
        // Verify that epoch keys are truly independent
        // Same plaintext encrypted with different epochs should be completely different
        
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Same message, different epochs";

        let epoch_1_keys = EpochKeys::derive_from_topic(&topic_id, 1);
        let epoch_2_keys = EpochKeys::derive_from_topic(&topic_id, 2);

        let packet_1 = create_packet_with_epoch(
            &topic_id, &kp.private_key, &kp.public_key, plaintext, &epoch_1_keys
        ).unwrap();

        let packet_2 = create_packet_with_epoch(
            &topic_id, &kp.private_key, &kp.public_key, plaintext, &epoch_2_keys
        ).unwrap();

        // Ciphertexts should be different (different keys + different nonces)
        assert_ne!(packet_1.ciphertext, packet_2.ciphertext);
        
        // MACs should be different (different MAC keys)
        assert_ne!(packet_1.mac, packet_2.mac);
        
        // Epochs should be recorded correctly
        assert_eq!(packet_1.epoch, 1);
        assert_eq!(packet_2.epoch, 2);

        // Each packet should only decrypt with its own epoch keys
        assert!(verify_and_decrypt_with_epoch(&packet_1, &topic_id, &epoch_1_keys, VerificationMode::Full).is_ok());
        assert!(verify_and_decrypt_with_epoch(&packet_2, &topic_id, &epoch_2_keys, VerificationMode::Full).is_ok());
        
        // Cross-epoch decryption should fail
        assert!(verify_and_decrypt_with_epoch(&packet_1, &topic_id, &epoch_2_keys, VerificationMode::Full).is_err());
        assert!(verify_and_decrypt_with_epoch(&packet_2, &topic_id, &epoch_1_keys, VerificationMode::Full).is_err());
    }

    #[test]
    fn test_epoch_keys_derive_from_secret() {
        // Test deriving epoch keys from an epoch secret (Tier 2+)
        let epoch_secret = [42u8; 32]; // Simulated admin-distributed epoch secret
        
        let keys = EpochKeys::derive_from_secret(10, &epoch_secret);
        
        assert_eq!(keys.epoch, 10);
        assert_ne!(keys.k_enc, epoch_secret); // Should be derived, not raw
        assert_ne!(keys.k_mac, epoch_secret);
        assert_ne!(keys.k_enc, keys.k_mac); // enc and mac keys should differ
        
        // Same secret + epoch should produce same keys (deterministic)
        let keys2 = EpochKeys::derive_from_secret(10, &epoch_secret);
        assert_eq!(keys.k_enc, keys2.k_enc);
        assert_eq!(keys.k_mac, keys2.k_mac);
        
        // Different epoch should produce different keys
        let keys3 = EpochKeys::derive_from_secret(11, &epoch_secret);
        assert_ne!(keys.k_enc, keys3.k_enc);
        assert_ne!(keys.k_mac, keys3.k_mac);
    }

    #[test]
    fn test_epoch_keys_topic_isolation() {
        // Keys for same epoch but different topics should be different
        let topic_a = [1u8; 32];
        let topic_b = [2u8; 32];
        
        let keys_a = EpochKeys::derive_from_topic(&topic_a, 5);
        let keys_b = EpochKeys::derive_from_topic(&topic_b, 5);
        
        assert_ne!(keys_a.k_enc, keys_b.k_enc);
        assert_ne!(keys_a.k_mac, keys_b.k_mac);
    }

    #[test]
    fn test_historical_epoch_decryption() {
        // Member should be able to decrypt old messages if they have historical keys
        // This simulates offline member catching up
        
        let topic_id = test_topic_id();
        let sender = generate_key_pair();
        
        // Store messages from different epochs
        let mut packets = Vec::new();
        for epoch in 0..5 {
            let keys = EpochKeys::derive_from_topic(&topic_id, epoch);
            let msg = format!("Message from epoch {}", epoch);
            let packet = create_packet_with_epoch(
                &topic_id,
                &sender.private_key,
                &sender.public_key,
                msg.as_bytes(),
                &keys,
            ).unwrap();
            packets.push((epoch, packet));
        }

        // Member who has ALL historical keys (was member from epoch 0)
        // should be able to decrypt all messages
        for (epoch, packet) in &packets {
            let keys = EpochKeys::derive_from_topic(&topic_id, *epoch);
            let result = verify_and_decrypt_with_epoch(
                packet, &topic_id, &keys, VerificationMode::Full
            );
            assert!(result.is_ok(), "Should decrypt epoch {} message", epoch);
            
            let decrypted = result.unwrap();
            let expected = format!("Message from epoch {}", epoch);
            assert_eq!(decrypted, expected.as_bytes());
        }

        // Member who joined at epoch 3 should only decrypt epochs 3+
        for (epoch, packet) in &packets {
            let join_epoch = 3u64;
            if *epoch >= join_epoch {
                // Has keys - should decrypt
                let keys = EpochKeys::derive_from_topic(&topic_id, *epoch);
                assert!(verify_and_decrypt_with_epoch(
                    packet, &topic_id, &keys, VerificationMode::Full
                ).is_ok());
            } else {
                // Doesn't have keys - attempting with wrong epoch should fail
                let wrong_keys = EpochKeys::derive_from_topic(&topic_id, join_epoch);
                assert!(verify_and_decrypt_with_epoch(
                    packet, &topic_id, &wrong_keys, VerificationMode::Full
                ).is_err());
            }
        }
    }

    #[test]
    fn test_epoch_embedded_in_aad() {
        // Verify that epoch is included in AAD (authenticated associated data)
        // Tampering with epoch field should cause decryption failure
        
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"AAD test message";

        let keys = EpochKeys::derive_from_topic(&topic_id, 7);
        let mut packet = create_packet_with_epoch(
            &topic_id, &kp.private_key, &kp.public_key, plaintext, &keys
        ).unwrap();

        // Verify normal decryption works
        assert!(verify_and_decrypt_with_epoch(&packet, &topic_id, &keys, VerificationMode::Full).is_ok());

        // Tamper with epoch field in packet
        packet.epoch = 8; // Change from 7 to 8

        // Decryption should fail even with "correct" epoch 8 keys
        // because the ciphertext was authenticated with epoch 7 in AAD
        let epoch_8_keys = EpochKeys::derive_from_topic(&topic_id, 8);
        let result = verify_and_decrypt_with_epoch(&packet, &topic_id, &epoch_8_keys, VerificationMode::Full);
        
        // Should fail at MAC verification (epoch in MAC key derivation)
        // or decryption (epoch in AAD)
        assert!(result.is_err());
    }

    #[test]
    fn test_backward_compatibility_epoch_zero() {
        // Legacy create_packet should use epoch 0
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Legacy message";

        let packet = create_packet(&topic_id, &kp.private_key, &kp.public_key, plaintext).unwrap();
        
        assert_eq!(packet.epoch, 0);
        
        // Should decrypt with epoch 0 keys
        let epoch_0_keys = EpochKeys::derive_from_topic(&topic_id, 0);
        let decrypted = verify_and_decrypt_with_epoch(
            &packet, &topic_id, &epoch_0_keys, VerificationMode::Full
        ).unwrap();
        assert_eq!(decrypted, plaintext);
        
        // Legacy verify_and_decrypt_packet should also work
        let decrypted2 = verify_and_decrypt_packet(&packet, &topic_id).unwrap();
        assert_eq!(decrypted2, plaintext);
    }

    #[test]
    fn test_packet_serialization_with_epoch() {
        let topic_id = test_topic_id();
        let kp = generate_key_pair();
        let plaintext = b"Serialization test";

        let keys = EpochKeys::derive_from_topic(&topic_id, 42);
        let packet = create_packet_with_epoch(
            &topic_id, &kp.private_key, &kp.public_key, plaintext, &keys
        ).unwrap();

        // Serialize and deserialize
        let bytes = packet.to_bytes().unwrap();
        let restored = SendPacket::from_bytes(&bytes).unwrap();

        // Epoch should survive serialization
        assert_eq!(restored.epoch, 42);
        assert_eq!(restored.packet_id, packet.packet_id);
        assert_eq!(restored.harbor_id, packet.harbor_id);
        assert_eq!(restored.ciphertext, packet.ciphertext);
        assert_eq!(restored.mac, packet.mac);

        // Should still decrypt
        let decrypted = verify_and_decrypt_with_epoch(
            &restored, &topic_id, &keys, VerificationMode::Full
        ).unwrap();
        assert_eq!(decrypted, plaintext);
    }
}
