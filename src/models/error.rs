//! Error types for the Claude OpenAI Gateway.
//!
//! This module defines all error types that can occur during gateway operation,
//! including process spawning failures and CLI errors.

use thiserror::Error;

/// Errors that can occur during gateway operations.
///
/// This enum represents all possible failure modes when proxying requests
/// from OpenAI format to Claude CLI and back.
#[derive(Error, Debug)]
pub enum GatewayError {
    /// Failed to spawn the Claude CLI process.
    ///
    /// # Arguments
    /// * Source error from the I/O operation that failed
    #[error("failed to spawn claude: {0}")]
    Spawn(#[source] std::io::Error),

    /// Claude CLI exited with an error or wrote to stderr.
    ///
    /// # Arguments
    /// * Error message from Claude CLI's stderr
    #[error("claude stderr: {0}")]
    Cli(String),

    /// General I/O error during operation.
    ///
    /// # Arguments
    /// * Source I/O error
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
