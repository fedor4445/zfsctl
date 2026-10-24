use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("command '{cmd}' failed with status {status}: {stderr}")]
    CommandFailed {
        cmd: String,
        status: i32,
        stderr: String,
    },
    #[error("failed to spawn command '{cmd}': {source}")]
    Spawn {
        cmd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse output: {0}")]
    Parse(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("protected: {0}")]
    Held(String),
    #[error("could not check holds: {0}")]
    HoldCheck(String),
    #[error("SMB: {0}")]
    Smb(String),
    /// A message that is already in words for the operator, shown as it is.
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
