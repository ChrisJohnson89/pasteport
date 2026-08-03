use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("license key is malformed: {0}")]
    Malformed(&'static str),

    #[error("license key format {0:?} is not recognised by this version")]
    UnknownFormat(String),

    #[error("license payload version {0} is newer than this build understands")]
    UnsupportedPayloadVersion(u32),

    #[error("license signature is not valid")]
    BadSignature,

    #[error("this build has no license verifying key compiled in")]
    NoVerifyingKey,

    #[error("license verifying key is invalid: {0}")]
    InvalidVerifyingKey(&'static str),

    #[error("could not encode license payload: {0}")]
    Encode(#[source] serde_json::Error),

    #[error("could not decode license payload: {0}")]
    Decode(#[source] serde_json::Error),

    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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
