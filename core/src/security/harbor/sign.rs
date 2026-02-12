//! Ed25519 packet signing and verification
//!
//! Provides digital signatures for Harbor packets using Ed25519 over BLAKE3.
//! This ensures packet authenticity and integrity.
//!
//! # Signing Scheme
//!
//! ```text
//! hash = BLAKE3(endpoint_id || nonce || ciphertext)
//! signature = Ed25519_Sign(private_key, hash)
//! ```
//!
//! # Security Properties
//!
//! - **Authenticity**: Only the holder of the private key can create valid signatures.
//! - **Integrity**: Any modification to endpoint_id, nonce, or ciphertext invalidates the signature.
//! - **Non-repudiation**: Signatures prove the private key holder signed the data.
//!
//! # Why Hash-Then-Sign?
//!
//! Ed25519 can sign arbitrary-length messages, but we hash first for:
//! - Consistent input size regardless of ciphertext length
//! - BLAKE3 is extremely fast, so no performance penalty
//! - Defense in depth against potential Ed25519 implementation issues

use blake3::Hasher;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Sign a packet: signature = Sign(PrivateKey, BLAKE3(endpoint_id || nonce || ciphertext))
///
/// # Arguments
/// * `private_key` - 32-byte Ed25519 private key
/// * `endpoint_id` - Endpoint identifier (can be empty)
/// * `nonce` - 12-byte nonce used for AEAD encryption
/// * `ciphertext` - Encrypted data including the 16-byte AEAD tag (can be empty)
///
/// # Returns
/// 64-byte Ed25519 signature
pub fn sign_packet(
    private_key: &[u8; 32],
    endpoint_id: &[u8],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> [u8; 64] {
    // Hash: BLAKE3(endpoint_id || nonce || ciphertext)
    let hash = hash_packet_data(endpoint_id, nonce, ciphertext);

    // Sign the hash
    let signing_key = SigningKey::from_bytes(private_key);
    let signature: Signature = signing_key.sign(hash.as_bytes());

    signature.to_bytes()
}

/// Verify a packet signature.
///
/// # Arguments
/// * `public_key` - 32-byte Ed25519 public key
/// * `endpoint_id` - Endpoint identifier
/// * `nonce` - 12-byte nonce used for AEAD encryption
/// * `ciphertext` - Encrypted data including the 16-byte AEAD tag
/// * `signature` - 64-byte signature to verify
///
/// # Returns
/// `true` if signature is valid, `false` if:
/// - Public key is malformed (not a valid Ed25519 point)
/// - Signature doesn't match the data
/// - Any of the signed data was modified
pub fn verify_packet(
    public_key: &[u8; 32],
    endpoint_id: &[u8],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    signature: &[u8; 64],
) -> bool {
    // Reconstruct the hash
    let hash = hash_packet_data(endpoint_id, nonce, ciphertext);

    // Parse the public key - returns false for invalid keys
    let verifying_key = match VerifyingKey::from_bytes(public_key) {
        Ok(key) => key,
        Err(_) => {
            tracing::trace!("verify_packet: invalid public key bytes");
            return false;
        }
    };

    let signature = Signature::from_bytes(signature);

    verifying_key.verify(hash.as_bytes(), &signature).is_ok()
}

/// Hash packet data: BLAKE3(endpoint_id || nonce || ciphertext)
fn hash_packet_data(endpoint_id: &[u8], nonce: &[u8; 12], ciphertext: &[u8]) -> blake3::Hash {
    let mut hasher = Hasher::new();
    hasher.update(endpoint_id);
    hasher.update(nonce);
    hasher.update(ciphertext);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::create_key_pair::generate_key_pair;
    use ed25519_dalek::VerifyingKey;

    #[test]
    fn test_sign_and_verify_packet() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data_with_aead_tag_here";

        // Sign the packet
        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);

        // Signature should be 64 bytes
        assert_eq!(signature.len(), 64);

        // Verification should succeed
        assert!(verify_packet(
            &kp.public_key,
            endpoint_id,
            &nonce,
            ciphertext,
            &signature
        ));
    }

    #[test]
    fn test_wrong_public_key_fails() {
        let kp = generate_key_pair();
        let wrong_kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data";

        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);

        // Verification with wrong key should fail
        assert!(!verify_packet(
            &wrong_kp.public_key,
            endpoint_id,
            &nonce,
            ciphertext,
            &signature
        ));
    }

    #[test]
    fn test_tampered_endpoint_id_fails() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data";

        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);

        // Tampered endpoint_id should fail
        assert!(!verify_packet(
            &kp.public_key,
            b"node-99999", // tampered
            &nonce,
            ciphertext,
            &signature
        ));
    }

    #[test]
    fn test_tampered_nonce_fails() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let tampered_nonce: [u8; 12] = [0x00; 12];
        let ciphertext = b"encrypted_data";

        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);

        // Tampered nonce should fail
        assert!(!verify_packet(
            &kp.public_key,
            endpoint_id,
            &tampered_nonce,
            ciphertext,
            &signature
        ));
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data";

        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);

        // Tampered ciphertext should fail
        assert!(!verify_packet(
            &kp.public_key,
            endpoint_id,
            &nonce,
            b"tampered_data",
            &signature
        ));
    }

    #[test]
    fn test_empty_endpoint_id() {
        let kp = generate_key_pair();
        let endpoint_id: &[u8] = b"";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data";

        // Empty endpoint_id should work
        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);
        assert!(verify_packet(
            &kp.public_key,
            endpoint_id,
            &nonce,
            ciphertext,
            &signature
        ));

        // But verification with non-empty endpoint_id should fail
        assert!(!verify_packet(
            &kp.public_key,
            b"non-empty",
            &nonce,
            ciphertext,
            &signature
        ));
    }

    #[test]
    fn test_empty_ciphertext() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext: &[u8] = b"";

        // Empty ciphertext should work
        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);
        assert!(verify_packet(
            &kp.public_key,
            endpoint_id,
            &nonce,
            ciphertext,
            &signature
        ));
    }

    #[test]
    fn test_invalid_public_key_bytes() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data";

        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);

        // Find a guaranteed-invalid compressed Ed25519 point.
        let mut invalid_pubkey = None;
        let mut candidate = [0u8; 32];
        for byte in 0u8..=u8::MAX {
            candidate[0] = byte;
            if VerifyingKey::from_bytes(&candidate).is_err() {
                invalid_pubkey = Some(candidate);
                break;
            }
        }
        let invalid_pubkey = invalid_pubkey.expect("failed to find invalid public-key test vector");

        assert!(!verify_packet(
            &invalid_pubkey,
            endpoint_id,
            &nonce,
            ciphertext,
            &signature
        ));
    }

    #[test]
    fn test_random_signature_fails() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data";

        // Random signature should fail verification
        let random_signature: [u8; 64] = [0xAB; 64];
        assert!(!verify_packet(
            &kp.public_key,
            endpoint_id,
            &nonce,
            ciphertext,
            &random_signature
        ));

        // All zeros signature should also fail
        let zero_signature: [u8; 64] = [0u8; 64];
        assert!(!verify_packet(
            &kp.public_key,
            endpoint_id,
            &nonce,
            ciphertext,
            &zero_signature
        ));
    }

    #[test]
    fn test_signature_is_deterministic() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data";

        // Ed25519 is deterministic - same inputs produce same signature
        let sig1 = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);
        let sig2 = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext);

        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_signature_not_reusable() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext1 = b"first_message";
        let ciphertext2 = b"second_message";

        // Sign first message
        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, ciphertext1);

        // Signature should not work for second message
        assert!(!verify_packet(
            &kp.public_key,
            endpoint_id,
            &nonce,
            ciphertext2,
            &signature
        ));
    }

    #[test]
    fn test_large_ciphertext() {
        let kp = generate_key_pair();
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = vec![0xAB_u8; 1024 * 1024]; // 1 MB

        let signature = sign_packet(&kp.private_key, endpoint_id, &nonce, &ciphertext);
        assert!(verify_packet(
            &kp.public_key,
            endpoint_id,
            &nonce,
            &ciphertext,
            &signature
        ));
    }

    #[test]
    fn test_sign_packet_known_vector() {
        let private_key: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        let endpoint_id = b"node-12345";
        let nonce: [u8; 12] = [0x42; 12];
        let ciphertext = b"encrypted_data";

        let signature = sign_packet(&private_key, endpoint_id, &nonce, ciphertext);
        let expected_signature: [u8; 64] = [
            99, 62, 145, 255, 126, 215, 173, 66, 252, 34, 176, 128, 173, 145, 62, 254, 209, 235,
            186, 62, 44, 75, 209, 225, 16, 249, 147, 152, 71, 79, 175, 94, 121, 89, 28, 146, 52,
            127, 128, 59, 189, 6, 0, 181, 13, 176, 144, 207, 39, 46, 125, 129, 87, 184, 4, 241,
            165, 74, 48, 97, 235, 249, 177, 10,
        ];

        assert_eq!(signature, expected_signature);
    }
}
