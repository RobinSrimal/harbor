mod error;
mod service;

pub use error::KeychainError;
pub use service::{
    delete_passphrase, has_stored_passphrase, has_stored_passphrase_result, retrieve_passphrase,
    store_passphrase,
};
