# Database Encryption

Harbor encrypts all local data using SQLCipher, with optional OS keyring integration for secure key storage.

## Overview

```
┌─────────────────────────────────────────┐
│           Your Application              │
└─────────────────────────────────────────┘
                    │
                    ▼ db_key (32 bytes)
┌─────────────────────────────────────────┐
│              Protocol                    │
│                                          │
│  ┌─────────────────────────────────────┐ │
│  │           SQLCipher                  │ │
│  │      (AES-256 encryption)           │ │
│  └─────────────────────────────────────┘ │
└─────────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────┐
│         Encrypted Database File         │
│  - Identity (keypair)                   │
│  - Topics and membership                │
│  - Peer connections                     │
│  - DHT routing table                    │
│  - Harbor packet cache                  │
└─────────────────────────────────────────┘
```

## Providing the Database Key

The database key is a 32-byte secret used to encrypt the local database. You must provide it when starting the protocol:

```rust
use harbor_core::{Protocol, ProtocolConfig};

let db_key: [u8; 32] = get_key_from_somewhere();

let config = ProtocolConfig::default()
    .with_db_key(db_key)
    .with_db_path("myapp.db".into());  // Optional: defaults to "harbor_protocol.db"

let protocol = Protocol::start(config).await?;
```

**Key requirements:**
- Exactly 32 bytes
- Must be the same key to reopen an existing database
- If lost, the database cannot be decrypted

**Database path:**
- Default: `harbor_protocol.db` in the current working directory
- Use `.with_db_path()` to specify a custom location
- Blob storage defaults to `.harbor_blobs/` next to the database

## Key Storage Options

### Option 1: OS Keyring (Recommended)

Harbor provides keyring integration for secure key storage using the OS credential manager (Keychain on macOS, Credential Manager on Windows, Secret Service on Linux).

```rust
use harbor_core::keychain;

// Check if a key is already stored
if keychain::has_stored_passphrase() {
    // Retrieve existing key
    let passphrase = keychain::retrieve_passphrase()?.unwrap();
    let db_key = derive_key_from_passphrase(&passphrase);
} else {
    // First run: generate and store
    let passphrase = generate_secure_passphrase();
    keychain::store_passphrase(&passphrase)?;
    let db_key = derive_key_from_passphrase(&passphrase);
}
```

**Keyring API:**

```rust
/// Store a passphrase in the OS keychain
pub fn store_passphrase(passphrase: &str) -> Result<(), KeychainError>

/// Retrieve the passphrase (returns None if not stored)
pub fn retrieve_passphrase() -> Result<Option<String>, KeychainError>

/// Delete the stored passphrase
pub fn delete_passphrase() -> Result<(), KeychainError>

/// Check if a passphrase is stored
pub fn has_stored_passphrase() -> bool
```

The keyring stores credentials under:
- **Service**: `harbor-protocol`
- **User**: `db-passphrase`

### Option 2: User-Provided Passphrase

Prompt the user for a passphrase and derive a key:

```rust
use blake3::Hasher;

fn derive_key_from_passphrase(passphrase: &str) -> [u8; 32] {
    let mut hasher = Hasher::new_derive_key("harbor-db-key");
    hasher.update(passphrase.as_bytes());
    let mut key = [0u8; 32];
    hasher.finalize_xof().fill(&mut key);
    key
}

// Prompt user (use rpassword crate for hidden input)
let passphrase = rpassword::prompt_password("Enter passphrase: ")?;
let db_key = derive_key_from_passphrase(&passphrase);
```

### Option 3: Environment Variable

For server deployments or scripts:

```rust
fn db_key_from_env() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let hex_str = std::env::var("HARBOR_DB_KEY")?;
    let bytes = hex::decode(&hex_str)?;

    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}
```

```bash
export HARBOR_DB_KEY=$(openssl rand -hex 32)
```

**Security note**: Environment variables may be visible in process listings. Prefer keyring or user prompts for desktop applications.

## SQLCipher Details

Harbor uses [SQLCipher](https://www.zetetic.net/sqlcipher/) for database encryption:

- **Cipher**: AES-256 in CBC mode
- **Key derivation**: PBKDF2-HMAC-SHA512
- **Page size**: 4096 bytes
- **HMAC**: SHA512 for page authentication

The database file is fully encrypted at rest. Without the correct key, the file appears as random bytes.

### What's Encrypted

| Data | Stored In |
|------|-----------|
| Your identity (Ed25519 keypair) | `local_node` table |
| Topics you've joined | `topics` table |
| Topic membership lists | `topic_members` table |
| Epoch keys for each topic | `topic_epochs` table |
| Connected peers | `peers` table |
| DHT routing table | `dht_routing` table |
| Harbor packet cache | `harbor_cache` table |
| Pending messages | `outgoing_packets` table |

### File Structure

```
app.db          # Encrypted SQLite database
app.db-wal      # Write-ahead log (also encrypted)
app.db-shm      # Shared memory file (temporary)
.harbor_blobs/  # File chunks (encrypted per-topic)
```

## Security Considerations

### Key Management

1. **Never hardcode keys** in your application
2. **Never log keys** - `ProtocolConfig::Debug` redacts the key automatically
3. **Use the keyring** for desktop apps - it's protected by the OS
4. **Derive keys from passphrases** using a proper KDF (BLAKE3, Argon2, etc.)

### Database Location

Choose a secure location for the database:

```rust
// Good: User's private data directory
let db_path = dirs::data_local_dir()
    .unwrap()
    .join("my-app")
    .join("harbor.db");

// Bad: World-readable location
let db_path = PathBuf::from("/tmp/harbor.db");
```

### Backup Considerations

- Database backups are encrypted (safe to store)
- **Key backups are critical** - if lost, data is unrecoverable
- Consider key escrow or recovery mechanisms for important data

## Example: Full Key Management Flow

```rust
use harbor_core::{Protocol, ProtocolConfig, keychain};
use blake3::Hasher;

fn derive_key(passphrase: &str) -> [u8; 32] {
    let mut hasher = Hasher::new_derive_key("harbor-db-key");
    hasher.update(passphrase.as_bytes());
    let mut key = [0u8; 32];
    hasher.finalize_xof().fill(&mut key);
    key
}

async fn start_harbor() -> Result<Protocol, Box<dyn std::error::Error>> {
    let db_key = if keychain::has_stored_passphrase() {
        // Returning user: retrieve from keyring
        let passphrase = keychain::retrieve_passphrase()?.unwrap();
        derive_key(&passphrase)
    } else {
        // New user: prompt and store
        let passphrase = rpassword::prompt_password("Create passphrase: ")?;
        let confirm = rpassword::prompt_password("Confirm passphrase: ")?;

        if passphrase != confirm {
            return Err("Passphrases don't match".into());
        }

        keychain::store_passphrase(&passphrase)?;
        derive_key(&passphrase)
    };

    let config = ProtocolConfig::default()
        .with_db_key(db_key)
        .with_db_path(dirs::data_local_dir().unwrap().join("myapp/harbor.db"));

    Ok(Protocol::start(config).await?)
}
```
