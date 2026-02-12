use std::fmt;

/// Errors that can occur when interacting with the OS keychain.
#[derive(Debug)]
pub enum KeychainError {
    /// Keychain service is not available on this platform.
    NotAvailable(String),
    /// Failed to retrieve a credential.
    RetrieveError(String),
    /// Failed to store a credential.
    StoreError(String),
    /// Failed to delete a credential.
    DeleteError(String),
}

impl fmt::Display for KeychainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAvailable(msg) => write!(f, "keychain not available: {}", msg),
            Self::RetrieveError(msg) => write!(f, "failed to retrieve from keychain: {}", msg),
            Self::StoreError(msg) => write!(f, "failed to store in keychain: {}", msg),
            Self::DeleteError(msg) => write!(f, "failed to delete from keychain: {}", msg),
        }
    }
}

impl std::error::Error for KeychainError {}
