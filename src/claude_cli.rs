//! Claude CLI process management and interaction.
//!
//! This module provides functions for:
//! - Building Claude CLI command configurations
//! - Spawning Claude CLI processes
//! - Reading and parsing output streams

use crate::models::error::GatewayError;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::info;

/// Configuration for spawning a Claude CLI process.
#[derive(Debug, Clone)]
pub struct ClaudeCliConfig {
    /// Optional session ID to resume from
    pub resume_session: Option<String>,

    /// User prompt to send to Claude
    pub prompt: String,

    /// System prompt to set the assistant's behavior
    pub system_prompt: String,

    /// Model identifier (e.g., "claude-3-5-sonnet-20241022")
    pub model: String,
}

impl ClaudeCliConfig {
    /// Creates a new Claude CLI configuration.
    ///
    /// # Arguments
    /// * `prompt` - The user prompt to send
    /// * `system_prompt` - The system prompt to use
    /// * `model` - The model identifier
    ///
    /// # Returns
    /// A new `ClaudeCliConfig` with no resume session
    ///
    /// # Examples
    /// ```
    /// use claude_code_openai_gateway::claude_cli::ClaudeCliConfig;
    ///
    /// let config = ClaudeCliConfig::new(
    ///     "Hello!".to_string(),
    ///     "Be helpful".to_string(),
    ///     "claude-3-5-sonnet-20241022".to_string(),
    /// );
    /// assert_eq!(config.prompt, "Hello!");
    /// ```
    pub fn new(prompt: String, system_prompt: String, model: String) -> Self {
        Self {
            resume_session: None,
            prompt,
            system_prompt,
            model,
        }
    }

    /// Sets the resume session ID.
    ///
    /// # Arguments
    /// * `session_id` - Optional session ID to resume from
    ///
    /// # Returns
    /// Self with the resume session configured
    ///
    /// # Examples
    /// ```
    /// use claude_code_openai_gateway::claude_cli::ClaudeCliConfig;
    ///
    /// let config = ClaudeCliConfig::new(
    ///     "Hello!".to_string(),
    ///     "Be helpful".to_string(),
    ///     "claude-3-5-sonnet-20241022".to_string(),
    /// ).with_resume_session(Some("session-123".to_string()));
    ///
    /// assert_eq!(config.resume_session, Some("session-123".to_string()));
    /// ```
    pub fn with_resume_session(mut self, session_id: Option<String>) -> Self {
        self.resume_session = session_id;
        self
    }
}

/// Builds a Command for spawning Claude CLI with the given configuration.
///
/// # Arguments
/// * `config` - Configuration for the Claude CLI process
///
/// # Returns
/// A `Command` ready to spawn, with all arguments configured
///
/// # Examples
/// ```no_run
/// use claude_code_openai_gateway::claude_cli::{ClaudeCliConfig, build_claude_command};
///
/// let config = ClaudeCliConfig::new(
///     "Hello".to_string(),
///     "Be helpful".to_string(),
///     "claude-3-5-sonnet-20241022".to_string(),
/// );
/// let command = build_claude_command(&config);
/// ```
pub fn build_claude_command(config: &ClaudeCliConfig) -> Command {
    let mut cmd = Command::new("claude");

    if let Some(ref session_id) = config.resume_session {
        cmd.arg("--resume").arg(session_id);
    }

    cmd.arg("-p")
        .arg(&config.prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--model")
        .arg(&config.model)
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
        .arg("--system-prompt")
        .arg(&config.system_prompt)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    cmd
}

/// Spawns a Claude CLI process with the given configuration.
///
/// # Arguments
/// * `config` - Configuration for the Claude CLI process
///
/// # Returns
/// A spawned `Child` process with stdout and stderr piped
///
/// # Errors
/// Returns `GatewayError::Spawn` if the process fails to spawn
///
/// # Examples
/// ```no_run
/// use claude_code_openai_gateway::claude_cli::{ClaudeCliConfig, spawn_claude_cli};
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let config = ClaudeCliConfig::new(
///         "Hello".to_string(),
///         "Be helpful".to_string(),
///         "claude-3-5-sonnet-20241022".to_string(),
///     );
///     let child = spawn_claude_cli(&config)?;
///     Ok(())
/// }
/// ```
pub fn spawn_claude_cli(config: &ClaudeCliConfig) -> Result<Child, GatewayError> {
    info!(
        resume = config.resume_session.as_deref().unwrap_or("<new>"),
        model = %config.model,
        "spawning claude"
    );

    let mut cmd = build_claude_command(config);
    cmd.spawn().map_err(GatewayError::Spawn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_new() {
        let config = ClaudeCliConfig::new(
            "test prompt".to_string(),
            "test system".to_string(),
            "test-model".to_string(),
        );

        assert_eq!(config.prompt, "test prompt");
        assert_eq!(config.system_prompt, "test system");
        assert_eq!(config.model, "test-model");
        assert_eq!(config.resume_session, None);
    }

    #[test]
    fn test_config_with_resume_session() {
        let config = ClaudeCliConfig::new(
            "prompt".to_string(),
            "system".to_string(),
            "model".to_string(),
        )
        .with_resume_session(Some("session-123".to_string()));

        assert_eq!(config.resume_session, Some("session-123".to_string()));
    }

    #[test]
    fn test_build_command_without_resume() {
        let config = ClaudeCliConfig::new(
            "test".to_string(),
            "system".to_string(),
            "model".to_string(),
        );

        let cmd = build_claude_command(&config);
        let program = cmd.as_std().get_program();
        assert_eq!(program, "claude");
    }

    #[test]
    fn test_build_command_with_resume() {
        let config = ClaudeCliConfig::new(
            "test".to_string(),
            "system".to_string(),
            "model".to_string(),
        )
        .with_resume_session(Some("session-abc".to_string()));

        let cmd = build_claude_command(&config);
        let program = cmd.as_std().get_program();
        assert_eq!(program, "claude");
    }
}
