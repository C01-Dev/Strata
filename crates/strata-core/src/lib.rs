//! Config, shared types, errors and the state store.

pub mod config;
pub mod detect;
mod error;

pub use error::{Error, Result};

use std::path::PathBuf;

/// `$STRATA_HOME`, or `~/.strata`. Holds the socket, the state store and logs.
pub fn strata_home() -> PathBuf {
    match std::env::var_os("STRATA_HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".strata"),
    }
}
