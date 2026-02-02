mod error;
mod service;

pub use error::KeychainError;
pub use service::{store_passphrase, retrieve_passphrase, delete_passphrase, has_stored_passphrase};
