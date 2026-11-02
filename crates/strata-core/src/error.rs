use std::path::PathBuf;

/// The one error type. `Refused` and `NotFound` carry a message meant for the user,
/// including what to do instead; the rest are failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("state store: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("{}: {message}", path.display())]
    Config { path: PathBuf, message: String },
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    NotFound(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
