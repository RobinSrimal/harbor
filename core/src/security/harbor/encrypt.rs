//! ChaCha20-Poly1305 AEAD encryption
//!
//! Provides authenticated encryption with associated data (AEAD) using
//! ChaCha20-Poly1305, a fast and secure cipher.
//!
//! # Security Warning
//!
//! **NONCE REUSE IS CATASTROPHIC**: Never reuse a nonce with the same key.
//! Reusing a nonce completely breaks confidentiality - an attacker can
//! recover the XOR of two plaintexts and potentially the authentication key.
//!
//! Always use `generate_nonce()` for each encryption, or use a counter-based
//! nonce scheme that guarantees uniqueness.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;

/// Authentication tag size for ChaCha20-Poly1305
pub const TAG_SIZE: usize = 16;

/// Nonce size for ChaCha20-Poly1305
pub const NONCE_SIZE: usize = 12;

/// AEAD encryption/decryption error
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeadError {
    /// Encryption failed (should never happen with valid inputs)
    EncryptionFailed,
    /// Decryption failed - authentication tag mismatch or corrupted data
    DecryptionFailed,
    /// Ciphertext is too short (must be at least TAG_SIZE bytes)
    CiphertextTooShort,
}

impl std::fmt::Display for AeadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AeadError::EncryptionFailed => write!(f, "AEAD encryption failed"),
            AeadError::DecryptionFailed => write!(f, "AEAD decryption failed (authentication error)"),
            AeadError::CiphertextTooShort => write!(f, "Ciphertext too short (minimum {} bytes)", TAG_SIZE),
        }
    }
}

impl std::error::Error for AeadError {}

/// Generate a random 12-byte nonce for ChaCha20-Poly1305.
///
/// # Security
///
/// **Each nonce MUST be unique per key.** This function uses the OS CSPRNG
/// which provides sufficient randomness that collisions are astronomically
/// unlikely (birthday bound: ~2^48 encryptions before 50% collision chance).
///
/// For high-volume encryption (billions of messages), consider a counter-based
/// or hybrid nonce scheme instead.
pub fn generate_nonce() -> [u8; NONCE_SIZE] {
    let mut nonce = [0u8; NONCE_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    nonce
}

/// Encrypt plaintext with associated data using ChaCha20-Poly1305.
///
/// # Arguments
/// * `key` - 32-byte encryption key
/// * `nonce` - 12-byte nonce (**must be unique per key** - see module docs)
/// * `plaintext` - Data to encrypt (can be empty)
/// * `aad` - Associated data (authenticated but not encrypted)
///
/// # Returns
/// Ciphertext with 16-byte authentication tag appended.
/// Output length = plaintext.len() + 16.
///
/// # Security
///
/// **NEVER reuse a nonce with the same key.** This completely breaks security.
pub fn encrypt(
    key: &[u8; 32],
    nonce: &[u8; NONCE_SIZE],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, AeadError> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from_slice(nonce);

    cipher
        .encrypt(nonce, Payload { msg: plaintext, aad })
        .map_err(|_| AeadError::EncryptionFailed)
}

/// Decrypt ciphertext with associated data using ChaCha20-Poly1305.
///
/// # Arguments
/// * `key` - 32-byte encryption key
/// * `nonce` - 12-byte nonce (same as used for encryption)
/// * `ciphertext` - Data to decrypt (must be at least 16 bytes for auth tag)
/// * `aad` - Associated data (must match what was used during encryption)
///
/// # Returns
/// Decrypted plaintext, or error if:
/// - Ciphertext is shorter than 16 bytes
/// - Authentication tag doesn't match (wrong key, nonce, AAD, or corrupted data)
pub fn decrypt(
    key: &[u8; 32],
    nonce: &[u8; NONCE_SIZE],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, AeadError> {
    // Ciphertext must be at least TAG_SIZE bytes (the auth tag)
    if ciphertext.len() < TAG_SIZE {
        return Err(AeadError::CiphertextTooShort);
    }

    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from_slice(nonce);

    cipher
        .decrypt(nonce, Payload { msg: ciphertext, aad })
        .map_err(|_| AeadError::DecryptionFailed)
}

/// Encrypt without associated data.
///
/// Convenience wrapper for `encrypt()` with empty AAD.
pub fn encrypt_simple(
    key: &[u8; 32],
    nonce: &[u8; NONCE_SIZE],
    plaintext: &[u8],
) -> Result<Vec<u8>, AeadError> {
    encrypt(key, nonce, plaintext, &[])
}

/// Decrypt without associated data.
///
/// Convenience wrapper for `decrypt()` with empty AAD.
pub fn decrypt_simple(
    key: &[u8; 32],
    nonce: &[u8; NONCE_SIZE],
    ciphertext: &[u8],
) -> Result<Vec<u8>, AeadError> {
    decrypt(key, nonce, ciphertext, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_with_aad() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let plaintext = b"Hello, Harbor!";
        let aad = b"metadata";

        let ciphertext = encrypt(&key, &nonce, plaintext, aad).unwrap();

        // Ciphertext should be plaintext + 16-byte tag
        assert_eq!(ciphertext.len(), plaintext.len() + TAG_SIZE);

        // Decrypt should recover original plaintext
        let decrypted = decrypt(&key, &nonce, &ciphertext, aad).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_wrong_aad_fails() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let plaintext = b"Secret data";
        let aad = b"correct metadata";

        let ciphertext = encrypt(&key, &nonce, plaintext, aad).unwrap();

        // Decryption with wrong AAD should fail
        let result = decrypt(&key, &nonce, &ciphertext, b"wrong metadata");
        assert_eq!(result, Err(AeadError::DecryptionFailed));
    }

    #[test]
    fn test_wrong_key_fails() {
        let key: [u8; 32] = [0x42; 32];
        let wrong_key: [u8; 32] = [0x00; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let plaintext = b"Secret data";

        let ciphertext = encrypt(&key, &nonce, plaintext, &[]).unwrap();

        // Decryption with wrong key should fail
        let result = decrypt(&wrong_key, &nonce, &ciphertext, &[]);
        assert_eq!(result, Err(AeadError::DecryptionFailed));
    }

    #[test]
    fn test_wrong_nonce_fails() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let wrong_nonce: [u8; 12] = [0x00; 12];
        let plaintext = b"Secret data";

        let ciphertext = encrypt(&key, &nonce, plaintext, &[]).unwrap();

        // Decryption with wrong nonce should fail
        let result = decrypt(&key, &wrong_nonce, &ciphertext, &[]);
        assert_eq!(result, Err(AeadError::DecryptionFailed));
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let plaintext = b"Secret data";

        let mut ciphertext = encrypt(&key, &nonce, plaintext, &[]).unwrap();

        // Tamper with ciphertext
        ciphertext[0] ^= 0xFF;

        // Decryption should fail due to authentication
        let result = decrypt(&key, &nonce, &ciphertext, &[]);
        assert_eq!(result, Err(AeadError::DecryptionFailed));
    }

    #[test]
    fn test_tampered_tag_fails() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let plaintext = b"Secret data";

        let mut ciphertext = encrypt(&key, &nonce, plaintext, &[]).unwrap();

        // Tamper with the authentication tag (last 16 bytes)
        let tag_start = ciphertext.len() - TAG_SIZE;
        ciphertext[tag_start] ^= 0xFF;

        // Decryption should fail
        let result = decrypt(&key, &nonce, &ciphertext, &[]);
        assert_eq!(result, Err(AeadError::DecryptionFailed));
    }

    #[test]
    fn test_simple_encrypt_decrypt() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let plaintext = b"Simple message";

        let ciphertext = encrypt_simple(&key, &nonce, plaintext).unwrap();
        let decrypted = decrypt_simple(&key, &nonce, &ciphertext).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_generate_nonce() {
        let nonce1 = generate_nonce();
        let nonce2 = generate_nonce();

        // Nonces should be correct size
        assert_eq!(nonce1.len(), NONCE_SIZE);
        assert_eq!(nonce2.len(), NONCE_SIZE);

        // Random nonces should be different
        assert_ne!(nonce1, nonce2);
    }

    #[test]
    fn test_empty_plaintext() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let plaintext: &[u8] = b"";

        let ciphertext = encrypt(&key, &nonce, plaintext, &[]).unwrap();

        // Empty plaintext should produce only the 16-byte tag
        assert_eq!(ciphertext.len(), TAG_SIZE);

        // Should decrypt back to empty
        let decrypted = decrypt(&key, &nonce, &ciphertext, &[]).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn test_ciphertext_too_short() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];

        // Empty ciphertext
        let result = decrypt(&key, &nonce, &[], &[]);
        assert_eq!(result, Err(AeadError::CiphertextTooShort));

        // 15 bytes (one byte short of tag)
        let short_ciphertext = [0u8; 15];
        let result = decrypt(&key, &nonce, &short_ciphertext, &[]);
        assert_eq!(result, Err(AeadError::CiphertextTooShort));

        // Exactly 16 bytes should attempt decryption (will fail auth, not length)
        let min_ciphertext = [0u8; 16];
        let result = decrypt(&key, &nonce, &min_ciphertext, &[]);
        assert_eq!(result, Err(AeadError::DecryptionFailed));
    }

    #[test]
    fn test_large_plaintext() {
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];

        // 1 MB plaintext
        let plaintext = vec![0xAB_u8; 1024 * 1024];

        let ciphertext = encrypt(&key, &nonce, &plaintext, &[]).unwrap();
        assert_eq!(ciphertext.len(), plaintext.len() + TAG_SIZE);

        let decrypted = decrypt(&key, &nonce, &ciphertext, &[]).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_error_display() {
        assert_eq!(
            AeadError::EncryptionFailed.to_string(),
            "AEAD encryption failed"
        );
        assert_eq!(
            AeadError::DecryptionFailed.to_string(),
            "AEAD decryption failed (authentication error)"
        );
        assert_eq!(
            AeadError::CiphertextTooShort.to_string(),
            "Ciphertext too short (minimum 16 bytes)"
        );
    }

    #[test]
    fn test_deterministic_encryption() {
        // Same inputs should produce same output (no randomness in encrypt itself)
        let key: [u8; 32] = [0x42; 32];
        let nonce: [u8; 12] = [0x24; 12];
        let plaintext = b"Test message";

        let ciphertext1 = encrypt(&key, &nonce, plaintext, &[]).unwrap();
        let ciphertext2 = encrypt(&key, &nonce, plaintext, &[]).unwrap();

        assert_eq!(ciphertext1, ciphertext2);
    }

    #[test]
    fn test_different_nonces_different_ciphertext() {
        let key: [u8; 32] = [0x42; 32];
        let nonce1: [u8; 12] = [0x01; 12];
        let nonce2: [u8; 12] = [0x02; 12];
        let plaintext = b"Same message";

        let ciphertext1 = encrypt(&key, &nonce1, plaintext, &[]).unwrap();
        let ciphertext2 = encrypt(&key, &nonce2, plaintext, &[]).unwrap();

        // Different nonces should produce different ciphertext
        assert_ne!(ciphertext1, ciphertext2);
    }
}
