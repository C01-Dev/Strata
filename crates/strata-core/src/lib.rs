//! Config, shared types, errors and the state store.

// PLAN's edge cases: Windows is not supported in v0.1. A Windows build of `strata` stops
// here with the reason (crates that do not depend on this one may also report their Unix
// APIs).
#[cfg(windows)]
compile_error!("Strata runs on macOS and Linux; Windows is not supported in v0.1");

pub mod client;
pub mod config;
pub mod detect;
mod error;
pub mod store;

pub use error::{Error, Result};

use std::path::PathBuf;

/// `$STRATA_HOME`, or `~/.strata`. Holds the socket, the state store and logs.
pub fn strata_home() -> PathBuf {
    match std::env::var_os("STRATA_HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".strata"),
    }
}
