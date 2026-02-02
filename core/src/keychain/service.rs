use keyring::Entry;

use super::error::KeychainError;

const SERVICE: &str = "harbor-protocol";
const USER: &str = "db-passphrase";

fn entry() -> Result<Entry, KeychainError> {
    Entry::new(SERVICE, USER).map_err(|e| KeychainError::NotAvailable(e.to_string()))
}

/// Store a database passphrase in the OS keychain.
pub fn store_passphrase(passphrase: &str) -> Result<(), KeychainError> {
    entry()?.set_password(passphrase).map_err(|e| KeychainError::StoreError(e.to_string()))
}

/// Retrieve the database passphrase from the OS keychain.
///
/// Returns `Ok(None)` if no credential is stored.
pub fn retrieve_passphrase() -> Result<Option<String>, KeychainError> {
    match entry()?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(KeychainError::NotAvailable(e.to_string())),
    }
}

/// Delete the stored passphrase from the OS keychain.
///
/// Returns `Ok(())` even if no credential was stored.
pub fn delete_passphrase() -> Result<(), KeychainError> {
    match entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(KeychainError::DeleteError(e.to_string())),
    }
}

/// Check whether a passphrase is currently stored in the keychain.
pub fn has_stored_passphrase() -> bool {
    retrieve_passphrase().ok().flatten().is_some()
}
