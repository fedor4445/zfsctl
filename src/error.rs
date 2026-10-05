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
    #[error("SMB: {0}")]
    Smb(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
