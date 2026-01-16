use ed25519_dalek::SigningKey;
use rand::RngCore;
use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// An Ed25519 key pair for signing and verification.
///
/// The private key is automatically zeroed from memory when dropped
/// to prevent secret key material from lingering.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct KeyPair {
    /// 32-byte private key (keep this secret!)
    pub private_key: [u8; 32],
    /// 32-byte public key (share this freely)
    #[zeroize(skip)]
    pub public_key: [u8; 32],
}

// Custom Debug implementation to prevent accidental private key exposure in logs
impl fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyPair")
            .field("private_key", &"[REDACTED]")
            .field("public_key", &hex::encode(self.public_key))
            .finish()
    }
}


/// Generate a new random Ed25519 key pair.
///
/// Uses the operating system's cryptographically secure random number generator.
pub fn generate_key_pair() -> KeyPair {
    // Generate 32 random bytes for the private key
    let mut secret_bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret_bytes);

    let signing_key = SigningKey::from_bytes(&secret_bytes);

    let keypair = KeyPair {
        private_key: signing_key.to_bytes(),
        public_key: signing_key.verifying_key().to_bytes(),
    };

    // Zeroize the temporary secret bytes
    secret_bytes.zeroize();

    keypair
}

/// Derive a key pair from existing private key bytes.
///
/// Use this to restore a key pair from a stored private key.
pub fn key_pair_from_bytes(private_key: &[u8; 32]) -> KeyPair {
    let signing_key = SigningKey::from_bytes(private_key);

    KeyPair {
        private_key: signing_key.to_bytes(),
        public_key: signing_key.verifying_key().to_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, Verifier};

    #[test]
    fn test_generate_key_pair() {
        let kp = generate_key_pair();

        // Keys should be 32 bytes
        assert_eq!(kp.private_key.len(), 32);
        assert_eq!(kp.public_key.len(), 32);

        // Private and public keys should be different
        assert_ne!(kp.private_key, kp.public_key);
    }

    #[test]
    fn test_key_pair_from_bytes() {
        let original = generate_key_pair();

        // Recreate key pair from private key
        let restored = key_pair_from_bytes(&original.private_key);

        // Should produce the same public key
        assert_eq!(original.public_key, restored.public_key);
        assert_eq!(original.private_key, restored.private_key);
    }

    #[test]
    fn test_unique_key_pairs() {
        let kp1 = generate_key_pair();
        let kp2 = generate_key_pair();

        // Each generated key pair should be unique
        assert_ne!(kp1.private_key, kp2.private_key);
        assert_ne!(kp1.public_key, kp2.public_key);
    }

    #[test]
    fn test_sign_and_verify() {
        // Generate a key pair
        let kp = generate_key_pair();

        // Create a signing key from the private key
        let signing_key = SigningKey::from_bytes(&kp.private_key);
        let verifying_key = signing_key.verifying_key();

        // Sign a message
        let message = b"Hello, Harbor!";
        let signature = signing_key.sign(message);

        // Verify the signature
        assert!(
            verifying_key.verify(message, &signature).is_ok(),
            "Signature verification should succeed"
        );

        // Verify with wrong message fails
        let wrong_message = b"Wrong message";
        assert!(
            verifying_key.verify(wrong_message, &signature).is_err(),
            "Signature verification should fail for wrong message"
        );
    }

    #[test]
    fn test_sign_verify_with_different_keypair_fails() {
        let kp1 = generate_key_pair();
        let kp2 = generate_key_pair();

        let signing_key1 = SigningKey::from_bytes(&kp1.private_key);
        let verifying_key2 = SigningKey::from_bytes(&kp2.private_key).verifying_key();

        let message = b"Test message";
        let signature = signing_key1.sign(message);

        // Verification with different key pair should fail
        assert!(
            verifying_key2.verify(message, &signature).is_err(),
            "Signature verification should fail with different public key"
        );
    }

    #[test]
    fn test_deterministic_key_derivation() {
        // Same private key bytes should always produce the same key pair
        let private_key: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];

        let kp1 = key_pair_from_bytes(&private_key);
        let kp2 = key_pair_from_bytes(&private_key);

        assert_eq!(kp1.public_key, kp2.public_key);
        assert_eq!(kp1.private_key, kp2.private_key);
    }

    #[test]
    fn test_all_zero_private_key() {
        // Edge case: all-zero private key should still produce a valid key pair
        let zero_key: [u8; 32] = [0u8; 32];
        let kp = key_pair_from_bytes(&zero_key);

        // Should produce valid 32-byte keys
        assert_eq!(kp.private_key.len(), 32);
        assert_eq!(kp.public_key.len(), 32);

        // The key pair should still work for signing
        let signing_key = SigningKey::from_bytes(&kp.private_key);
        let message = b"Test with zero key";
        let signature = signing_key.sign(message);
        assert!(signing_key.verifying_key().verify(message, &signature).is_ok());
    }

    #[test]
    fn test_debug_does_not_expose_private_key() {
        let kp = generate_key_pair();
        let debug_output = format!("{:?}", kp);

        // Debug output should not contain the actual private key bytes
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output should redact private key"
        );

        // Should contain the public key in hex
        let public_hex = hex::encode(kp.public_key);
        assert!(
            debug_output.contains(&public_hex),
            "Debug output should show public key"
        );
    }
}
