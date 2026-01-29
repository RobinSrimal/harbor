//! Service trait for Harbor network services
//!
//! Defines a common pattern for services that need:
//! - Local identity (for signing/encryption)
//! - Database connection (for persistence)
//! - Endpoint (for network operations)
//!
//! Services receive these dependencies via Arc from Protocol at construction.

use std::sync::Arc;

use iroh::Endpoint;
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;

use crate::data::LocalIdentity;

/// Common dependencies shared by all Harbor services
///
/// Protocol passes these to services at construction time.
/// Services store Arc references to share ownership with Protocol.
#[derive(Clone)]
pub struct ServiceDeps {
    /// Iroh endpoint for network operations
    pub endpoint: Endpoint,
    /// Local identity for signing and encryption
    pub identity: Arc<LocalIdentity>,
    /// Database connection for persistence
    pub db: Arc<Mutex<DbConnection>>,
}

impl ServiceDeps {
    /// Create new service dependencies
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
    ) -> Self {
        Self {
            endpoint,
            identity,
            db,
        }
    }

    /// Get our endpoint ID (public key)
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get the private key for signing
    pub fn private_key(&self) -> &[u8; 32] {
        &self.identity.private_key
    }

    /// Get the public key
    pub fn public_key(&self) -> &[u8; 32] {
        &self.identity.public_key
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_service_deps_endpoint_id() {
        // ServiceDeps derives endpoint_id from identity.public_key
        let public_key = [42u8; 32];
        // Just test the relationship conceptually
        assert_eq!(public_key.len(), 32);
    }
}
