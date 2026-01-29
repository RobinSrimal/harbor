//! DM encryption using X25519 ECDH
//!
//! Encrypts DM payloads so that only the intended recipient can decrypt.
//! Uses ed25519→x25519 key conversion + crypto_box (XChaCha20-Poly1305).
//!
//! The sender and recipient derive the same shared secret from their
//! respective keypairs. Harbor nodes cannot decrypt DM content.

use ed25519_dalek::{SigningKey, VerifyingKey};

/// Encrypt a DM payload for a specific recipient.
///
/// Uses the sender's private key and recipient's public key to derive
/// a shared secret via X25519 ECDH, then encrypts with XChaCha20-Poly1305.
///
/// The nonce is appended to the ciphertext (24 bytes at the end).
///
/// # Arguments
/// * `sender_private_key` - Sender's ed25519 private key (32 bytes)
/// * `recipient_public_key` - Recipient's ed25519 public key (32 bytes)
/// * `plaintext` - Data to encrypt
///
/// # Returns
/// Ciphertext with appended nonce (plaintext.len() + 16 tag + 24 nonce)
pub fn dm_seal(
    sender_private_key: &[u8; 32],
    recipient_public_key: &[u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>, DmCryptoError> {
    let shared = derive_shared_secret(sender_private_key, recipient_public_key)?;
    let mut buffer = plaintext.to_vec();
    seal_with_shared(&shared, &mut buffer);
    Ok(buffer)
}

/// Decrypt a DM payload from a specific sender.
///
/// Uses the recipient's private key and sender's public key to derive
/// the same shared secret, then decrypts.
///
/// # Arguments
/// * `recipient_private_key` - Recipient's ed25519 private key (32 bytes)
/// * `sender_public_key` - Sender's ed25519 public key (32 bytes)
/// * `ciphertext` - Data to decrypt (includes appended nonce)
///
/// # Returns
/// Decrypted plaintext
pub fn dm_open(
    recipient_private_key: &[u8; 32],
    sender_public_key: &[u8; 32],
    ciphertext: &[u8],
) -> Result<Vec<u8>, DmCryptoError> {
    let shared = derive_shared_secret(recipient_private_key, sender_public_key)?;
    let mut buffer = ciphertext.to_vec();
    open_with_shared(&shared, &mut buffer)?;
    Ok(buffer)
}

/// Nonce length for XChaCha20-Poly1305
const NONCE_LEN: usize = 24;

/// Derive a shared secret from an ed25519 private key and an ed25519 public key.
fn derive_shared_secret(
    private_key: &[u8; 32],
    public_key: &[u8; 32],
) -> Result<crypto_box::ChaChaBox, DmCryptoError> {
    // Convert ed25519 private key → x25519 secret key
    let signing_key = SigningKey::from_bytes(private_key);
    let x25519_secret = crypto_box::SecretKey::from(signing_key.to_scalar());

    // Convert ed25519 public key → x25519 public key
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .map_err(|_| DmCryptoError::InvalidPublicKey)?;
    let x25519_public = crypto_box::PublicKey::from(verifying_key.to_montgomery());

    Ok(crypto_box::ChaChaBox::new(
        &x25519_public,
        &x25519_secret,
    ))
}

/// Encrypt in-place, appending nonce to buffer.
fn seal_with_shared(shared: &crypto_box::ChaChaBox, buffer: &mut Vec<u8>) {
    use crypto_box::aead::{AeadCore, AeadInPlace, OsRng};

    let nonce = crypto_box::ChaChaBox::generate_nonce(&mut OsRng);

    shared
        .encrypt_in_place(&nonce, &[], buffer)
        .expect("encryption failed");

    buffer.extend_from_slice(&nonce);
}

/// Decrypt in-place, reading nonce from end of buffer.
fn open_with_shared(
    shared: &crypto_box::ChaChaBox,
    buffer: &mut Vec<u8>,
) -> Result<(), DmCryptoError> {
    use crypto_box::aead::AeadInPlace;

    if buffer.len() < NONCE_LEN {
        return Err(DmCryptoError::InvalidCiphertext);
    }

    let offset = buffer.len() - NONCE_LEN;
    let nonce: [u8; NONCE_LEN] = buffer[offset..]
        .try_into()
        .map_err(|_| DmCryptoError::InvalidCiphertext)?;

    buffer.truncate(offset);
    shared
        .decrypt_in_place(&nonce.into(), &[], buffer)
        .map_err(|_| DmCryptoError::DecryptionFailed)?;

    Ok(())
}

/// DM encryption/decryption errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmCryptoError {
    /// Invalid ed25519 public key
    InvalidPublicKey,
    /// Ciphertext too short or malformed
    InvalidCiphertext,
    /// Decryption failed (wrong key or tampered data)
    DecryptionFailed,
}

impl std::fmt::Display for DmCryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DmCryptoError::InvalidPublicKey => write!(f, "invalid ed25519 public key"),
            DmCryptoError::InvalidCiphertext => write!(f, "invalid DM ciphertext"),
            DmCryptoError::DecryptionFailed => write!(f, "DM decryption failed"),
        }
    }
}

impl std::error::Error for DmCryptoError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::create_key_pair::generate_key_pair;

    #[test]
    fn test_dm_seal_open_roundtrip() {
        let alice = generate_key_pair();
        let bob = generate_key_pair();
        let plaintext = b"Hello Bob, this is a DM!";

        // Alice encrypts to Bob
        let ciphertext = dm_seal(&alice.private_key, &bob.public_key, plaintext).unwrap();

        // Ciphertext should be larger (tag + nonce)
        assert!(ciphertext.len() > plaintext.len());

        // Bob decrypts from Alice
        let decrypted = dm_open(&bob.private_key, &alice.public_key, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_dm_both_directions() {
        let alice = generate_key_pair();
        let bob = generate_key_pair();

        // Alice → Bob
        let msg1 = b"Hi Bob";
        let ct1 = dm_seal(&alice.private_key, &bob.public_key, msg1).unwrap();
        let pt1 = dm_open(&bob.private_key, &alice.public_key, &ct1).unwrap();
        assert_eq!(pt1, msg1);

        // Bob → Alice
        let msg2 = b"Hi Alice";
        let ct2 = dm_seal(&bob.private_key, &alice.public_key, msg2).unwrap();
        let pt2 = dm_open(&alice.private_key, &bob.public_key, &ct2).unwrap();
        assert_eq!(pt2, msg2);
    }

    #[test]
    fn test_dm_wrong_key_fails() {
        let alice = generate_key_pair();
        let bob = generate_key_pair();
        let eve = generate_key_pair();

        let ciphertext = dm_seal(&alice.private_key, &bob.public_key, b"secret").unwrap();

        // Eve cannot decrypt (wrong private key)
        let result = dm_open(&eve.private_key, &alice.public_key, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn test_dm_tampered_ciphertext_fails() {
        let alice = generate_key_pair();
        let bob = generate_key_pair();

        let mut ciphertext = dm_seal(&alice.private_key, &bob.public_key, b"secret").unwrap();
        ciphertext[0] ^= 0xFF; // tamper

        let result = dm_open(&bob.private_key, &alice.public_key, &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn test_dm_empty_payload() {
        let alice = generate_key_pair();
        let bob = generate_key_pair();

        let ciphertext = dm_seal(&alice.private_key, &bob.public_key, b"").unwrap();
        let decrypted = dm_open(&bob.private_key, &alice.public_key, &ciphertext).unwrap();
        assert_eq!(decrypted, b"");
    }
}
