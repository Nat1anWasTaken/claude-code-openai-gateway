//! Claude CLI process management and interaction.
//!
//! This module provides functions for:
//! - Building Claude CLI command configurations
//! - Spawning Claude CLI processes
//! - Reading and parsing output streams

use crate::models::error::GatewayError;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::{fs::File, io::Write};
use tokio::process::{Child, Command};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug)]
pub struct PromptFile {
    path: PathBuf,
}

impl PromptFile {
    fn new(contents: &str) -> Result<Self, std::io::Error> {
        let filename = format!("claude-system-prompt-{}.txt", Uuid::new_v4());
        let path = std::env::temp_dir().join(filename);
        let mut file = File::create(&path)?;
        file.write_all(contents.as_bytes())?;
        info!(path = %path.display(), bytes = contents.len(), "created system prompt file");
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PromptFile {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.path) {
            warn!(
                path = %self.path.display(),
                error = %err,
                "failed to remove system prompt temp file"
            );
        } else {
            info!(
                path = %self.path.display(),
                "removed system prompt temp file"
            );
        }
    }
}

#[derive(Debug)]
pub struct ClaudeCliProcess {
    pub child: Child,
    pub system_prompt_file: PromptFile,
}

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
/// * `system_prompt_file` - Optional path to a system prompt file
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
/// let command = build_claude_command(&config, None);
/// ```
pub fn build_claude_command(
    config: &ClaudeCliConfig,
    system_prompt_file: Option<&Path>,
) -> Command {
    let mut cmd = Command::new("claude");

    if let Some(ref session_id) = config.resume_session {
        cmd.arg("--resume").arg(session_id);
    }

    cmd.arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--include-partial-messages")
        .arg("--model")
        .arg(&config.model)
        .arg("--verbose")
        .arg("--dangerously-skip-permissions");

    if let Some(path) = system_prompt_file {
        cmd.arg("--system-prompt-file").arg(path);
    } else {
        cmd.arg("--system-prompt").arg(&config.system_prompt);
    }

    cmd.stdin(Stdio::piped())
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
/// A spawned Claude CLI process with stdout and stderr piped
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
///     let process = spawn_claude_cli(&config)?;
///     Ok(())
/// }
/// ```
pub fn spawn_claude_cli(config: &ClaudeCliConfig) -> Result<ClaudeCliProcess, GatewayError> {
    info!(
        resume = config.resume_session.as_deref().unwrap_or("<new>"),
        model = %config.model,
        "spawning claude"
    );

    let system_prompt_file = PromptFile::new(&config.system_prompt)?;
    let mut cmd = build_claude_command(config, Some(system_prompt_file.path()));
    let mut child = cmd.spawn().map_err(GatewayError::Spawn)?;
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = config.prompt.clone();
        tokio::spawn(async move {
            if stdin.write_all(prompt.as_bytes()).await.is_ok() {
                let _ = stdin.shutdown().await;
            }
        });
    }
    Ok(ClaudeCliProcess {
        child,
        system_prompt_file,
    })
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

        let cmd = build_claude_command(&config, None);
        let std_cmd = cmd.as_std();
        let program = std_cmd.get_program();
        let args: Vec<String> = std_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert_eq!(program, "claude");
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "--output-format");
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--include-partial-messages".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"model".to_string()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args.contains(&"--system-prompt".to_string()));
        assert!(!args.contains(&"test".to_string()));
    }

    #[test]
    fn test_build_command_with_system_prompt_file() {
        let config = ClaudeCliConfig::new(
            "test".to_string(),
            "system".to_string(),
            "model".to_string(),
        );

        let path = Path::new("/tmp/system.txt");
        let cmd = build_claude_command(&config, Some(path));
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let pos = args
            .iter()
            .position(|a| a == "--system-prompt-file")
            .expect("missing --system-prompt-file flag");
        assert_eq!(args.get(pos + 1), Some(&"/tmp/system.txt".to_string()));
    }

    #[test]
    fn test_build_command_with_resume() {
        let config = ClaudeCliConfig::new(
            "test".to_string(),
            "system".to_string(),
            "model".to_string(),
        )
        .with_resume_session(Some("session-abc".to_string()));

        let cmd = build_claude_command(&config, None);
        let program = cmd.as_std().get_program();
        assert_eq!(program, "claude");

        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let resume_pos = args
            .iter()
            .position(|a| a == "--resume")
            .expect("missing --resume flag");
        assert_eq!(args.get(resume_pos + 1), Some(&"session-abc".to_string()));
    }
}
