use std::path::PathBuf;

/// Everything that can go wrong inside the engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not determine a data directory for the current user")]
    NoDataDir,

    #[error("config file {path} is invalid: {source}")]
    Config {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("clip {0} not found")]
    ClipNotFound(i64),

    #[error("pinboard {0:?} not found")]
    PinboardNotFound(String),

    #[error("refusing to store a clip larger than {limit} bytes (got {actual})")]
    ClipTooLarge { limit: usize, actual: usize },

    #[error(
        "control socket path {0} is too long for this platform. \
         Use a shorter --data-dir, or set PASTEPORT_SOCKET to a short path"
    )]
    SocketPathTooLong(PathBuf),

    #[error("{0} is not owned by the current user; refusing to use it")]
    NotOurDirectory(PathBuf),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
