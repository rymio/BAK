use std::path::PathBuf;

/// Errors surfaced by `bak`. Every message is safe to print: engine errors are
/// passed through [`crate::redact::scrub`] before they are wrapped.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no bak project found in {0} or any parent directory (run `bak init`)")]
    NoProject(PathBuf),

    #[error("a bak project already exists at {0}")]
    ProjectExists(PathBuf),

    #[error("invalid config: {0}")]
    Config(String),

    #[error("unknown remote `{0}`")]
    UnknownRemote(String),

    #[error("remote `{0}` already exists")]
    RemoteExists(String),

    #[error("invalid remote url `{url}`: {reason}")]
    RemoteUrl { url: String, reason: String },

    #[error(
        "mount guard: {path} is not on a mounted drive (it is on the system disk); \
         plug in and mount the drive, then try again"
    )]
    NotMounted { path: PathBuf },

    #[error("path {0} is outside the project root")]
    OutsideProject(PathBuf),

    #[error("path {0} does not exist")]
    MissingPath(PathBuf),

    #[error("path {0} is not valid UTF-8 and cannot be stored in config.toml")]
    NonUtf8Path(PathBuf),

    #[error("{0}")]
    Engine(String),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Wrap an engine error message, scrubbing any registered secret.
    pub fn engine(message: impl std::fmt::Display) -> Self {
        Self::Engine(crate::redact::scrub(&message.to_string()))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
