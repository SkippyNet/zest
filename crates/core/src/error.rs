//! The error type shared by all Zest crates.

use std::path::PathBuf;

/// Convenient result alias used across the workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong inside the engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O failure with a human readable description of what was attempted.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    /// The operation needs administrator rights that the process does not have.
    #[error("administrator rights are required to {0}")]
    NeedsElevation(String),
    /// The safety guard refused to modify a critical location.
    #[error("refused to modify protected location: {}", .0.display())]
    Protected(PathBuf),
    /// The user cancelled the operation.
    #[error("operation cancelled")]
    Cancelled,
    /// Something that was expected to exist does not.
    #[error("not found: {0}")]
    NotFound(String),
    /// Data could not be parsed or was otherwise malformed.
    #[error("invalid data: {0}")]
    Invalid(String),
    /// An external command exited unsuccessfully.
    #[error("command failed: {0}")]
    Command(String),
    /// A Windows API call failed.
    #[error("Windows error: {0}")]
    Windows(String),
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Builds an [`Error::Other`] from anything printable.
    pub fn other(msg: impl std::fmt::Display) -> Self {
        Error::Other(msg.to_string())
    }

    /// True when the failure was caused by missing privileges.
    pub fn is_access_denied(&self) -> bool {
        match self {
            Error::NeedsElevation(_) => true,
            Error::Io { source, .. } => source.kind() == std::io::ErrorKind::PermissionDenied,
            _ => false,
        }
    }

    /// True when the failure simply means the target was already gone.
    pub fn is_not_found(&self) -> bool {
        match self {
            Error::NotFound(_) => true,
            Error::Io { source, .. } => source.kind() == std::io::ErrorKind::NotFound,
            _ => false,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            context: "I/O error".to_owned(),
            source,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Invalid(e.to_string())
    }
}

impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Error::Windows(format!("{} (0x{:08X})", e.message(), e.code().0 as u32))
    }
}

/// Adds a description of the attempted action to raw I/O errors.
pub trait IoContext<T> {
    /// Wraps the error with `context`, e.g. `"reading settings"`.
    fn ctx(self, context: impl Into<String>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn ctx(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::Io {
            context: context.into(),
            source,
        })
    }
}
