use keyring::Entry;

use super::error::KeychainError;

const DEFAULT_SERVICE: &str = "harbor-protocol-core";
const DEFAULT_USER: &str = "db-passphrase";
const SERVICE_ENV: &str = "HARBOR_KEYCHAIN_SERVICE";
const USER_ENV: &str = "HARBOR_KEYCHAIN_USER";

fn resolve_entry_component(raw: Option<String>, default_value: &str) -> String {
    raw.filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default_value.to_string())
}

fn entry_identifiers() -> (String, String) {
    let service = resolve_entry_component(std::env::var(SERVICE_ENV).ok(), DEFAULT_SERVICE);
    let user = resolve_entry_component(std::env::var(USER_ENV).ok(), DEFAULT_USER);
    (service, user)
}

fn entry() -> Result<Entry, KeychainError> {
    let (service, user) = entry_identifiers();
    Entry::new(&service, &user).map_err(|e| KeychainError::NotAvailable(e.to_string()))
}

enum RetrieveOutcome {
    Found(String),
    Missing,
    Error(String),
}

fn map_retrieve_outcome(outcome: RetrieveOutcome) -> Result<Option<String>, KeychainError> {
    match outcome {
        RetrieveOutcome::Found(password) => Ok(Some(password)),
        RetrieveOutcome::Missing => Ok(None),
        RetrieveOutcome::Error(message) => Err(KeychainError::RetrieveError(message)),
    }
}

enum DeleteOutcome {
    Deleted,
    Missing,
    Error(String),
}

fn map_delete_outcome(outcome: DeleteOutcome) -> Result<(), KeychainError> {
    match outcome {
        DeleteOutcome::Deleted | DeleteOutcome::Missing => Ok(()),
        DeleteOutcome::Error(message) => Err(KeychainError::DeleteError(message)),
    }
}

fn map_has_stored_result(
    result: Result<Option<String>, KeychainError>,
) -> Result<bool, KeychainError> {
    result.map(|value| value.is_some())
}

/// Store a database passphrase in the OS keychain.
pub fn store_passphrase(passphrase: &str) -> Result<(), KeychainError> {
    entry()?
        .set_password(passphrase)
        .map_err(|e| KeychainError::StoreError(e.to_string()))
}

/// Retrieve the database passphrase from the OS keychain.
///
/// Returns `Ok(None)` if no credential is stored.
pub fn retrieve_passphrase() -> Result<Option<String>, KeychainError> {
    let outcome = match entry()?.get_password() {
        Ok(password) => RetrieveOutcome::Found(password),
        Err(keyring::Error::NoEntry) => RetrieveOutcome::Missing,
        Err(e) => RetrieveOutcome::Error(e.to_string()),
    };
    map_retrieve_outcome(outcome)
}

/// Delete the stored passphrase from the OS keychain.
///
/// Returns `Ok(())` even if no credential was stored.
pub fn delete_passphrase() -> Result<(), KeychainError> {
    let outcome = match entry()?.delete_credential() {
        Ok(()) => DeleteOutcome::Deleted,
        Err(keyring::Error::NoEntry) => DeleteOutcome::Missing,
        Err(e) => DeleteOutcome::Error(e.to_string()),
    };
    map_delete_outcome(outcome)
}

/// Check whether a passphrase is currently stored in the keychain.
pub fn has_stored_passphrase() -> bool {
    has_stored_passphrase_result().unwrap_or(false)
}

/// Fallible version of [`has_stored_passphrase`] that preserves keychain errors.
pub fn has_stored_passphrase_result() -> Result<bool, KeychainError> {
    map_has_stored_result(retrieve_passphrase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_retrieve_outcome_found() {
        let result = map_retrieve_outcome(RetrieveOutcome::Found("secret".to_string())).unwrap();
        assert_eq!(result.as_deref(), Some("secret"));
    }

    #[test]
    fn test_map_retrieve_outcome_missing() {
        let result = map_retrieve_outcome(RetrieveOutcome::Missing).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_map_retrieve_outcome_error() {
        let err = map_retrieve_outcome(RetrieveOutcome::Error("backend failure".to_string()))
            .unwrap_err();
        match err {
            KeychainError::RetrieveError(message) => {
                assert!(message.contains("backend failure"));
            }
            other => panic!("expected RetrieveError, got {other:?}"),
        }
    }

    #[test]
    fn test_map_delete_outcome_missing_is_ok() {
        assert!(map_delete_outcome(DeleteOutcome::Missing).is_ok());
    }

    #[test]
    fn test_map_delete_outcome_error() {
        let err =
            map_delete_outcome(DeleteOutcome::Error("permission denied".to_string())).unwrap_err();
        match err {
            KeychainError::DeleteError(message) => {
                assert!(message.contains("permission denied"));
            }
            other => panic!("expected DeleteError, got {other:?}"),
        }
    }

    #[test]
    fn test_map_has_stored_result_true_when_value_exists() {
        let result = map_has_stored_result(Ok(Some("secret".to_string()))).unwrap();
        assert!(result);
    }

    #[test]
    fn test_map_has_stored_result_false_when_missing() {
        let result = map_has_stored_result(Ok(None)).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_map_has_stored_result_preserves_errors() {
        let err = map_has_stored_result(Err(KeychainError::NotAvailable("offline".to_string())))
            .unwrap_err();
        match err {
            KeychainError::NotAvailable(message) => assert!(message.contains("offline")),
            other => panic!("expected NotAvailable, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_entry_component_uses_default_for_empty() {
        let resolved = resolve_entry_component(Some("   ".to_string()), "fallback");
        assert_eq!(resolved, "fallback");
    }

    #[test]
    fn test_resolve_entry_component_preserves_non_empty() {
        let resolved = resolve_entry_component(Some("custom-service".to_string()), "fallback");
        assert_eq!(resolved, "custom-service");
    }
}
