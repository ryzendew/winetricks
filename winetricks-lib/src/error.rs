//! Error types for winetricks

use thiserror::Error;

/// Winetricks result type
pub type Result<T> = std::result::Result<T, WinetricksError>;

/// Main error type for winetricks operations
#[derive(Error, Debug)]
pub enum WinetricksError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Wine error: {0}")]
    Wine(String),

    #[error("Download error: {0}")]
    Download(String),

    #[error("Verb error: {0}")]
    Verb(String),

    #[error("Checksum mismatch: expected {expected}, got {got}")]
    ChecksumMismatch { expected: String, got: String },

    #[error("Verb not found: {0}")]
    VerbNotFound(String),

    #[error("Verb already installed: {0}")]
    VerbAlreadyInstalled(String),

    #[error("Verb conflict: {verb} conflicts with {conflicting}")]
    VerbConflict { verb: String, conflicting: String },

    #[error("Invalid wine version: {0}")]
    InvalidWineVersion(String),

    #[error("Command execution failed: {command} - {error}")]
    CommandExecution { command: String, error: String },

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}
