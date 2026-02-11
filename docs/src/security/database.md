# Local Data & Keys

Harbor stores identity keys, topics, and connection state locally in an **encrypted database**. You provide the 32-byte database key at startup.

## Provide the Database Key

```rust
use harbor_core::{Protocol, ProtocolConfig};

let db_key: [u8; 32] = get_key_from_somewhere();

let protocol = Protocol::start(
    ProtocolConfig::default()
        .with_db_key(db_key)
        .with_db_path("myapp.db".into())
).await?;
```

If the key is lost, the database cannot be opened.

## Recommended Key Storage

- **Desktop apps**: store a passphrase in the OS keychain and derive the key
- **Server apps**: load a key from a secure secret manager or environment variable

## Defaults

- Database file: `harbor_protocol.db` (current directory)
- Blob storage: `.harbor_blobs/` next to the database

## Practical Advice

- Never hardcode or log the database key
- Keep the database in a user-private location
- Use a consistent key to persist identity across restarts
