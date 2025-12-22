//! Utility functions for the Claude OpenAI Gateway.
//!
//! This module provides pure, stateless utility functions for:
//! - Computing conversation hashes for caching
//! - Extracting text from various JSON value formats
//! - Flattening OpenAI message arrays into prompts
//! - Generating timestamps

use crate::models::openai::OAChatMessage;
use hex::encode as hex_encode;
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the current Unix timestamp in seconds.
///
/// # Returns
/// Number of seconds since the Unix epoch (January 1, 1970)
///
/// # Panics
/// Panics if the system time is before the Unix epoch
///
/// # Examples
/// ```
/// use claude_code_openai_gateway::utils::unix_timestamp;
///
/// let now = unix_timestamp();
/// assert!(now > 0);
/// ```
pub fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before Unix epoch")
        .as_secs()
}

/// Builds the exact string that gets hashed for conversation caching.
///
/// Format: `{role}:{text}\n` per message, where `text` is the plain text
/// extracted from the message content.
pub fn message_hash_material(messages: &[OAChatMessage]) -> String {
    let mut material = String::new();
    for msg in messages {
        material.push_str(&msg.role);
        material.push(':');
        material.push_str(&extract_text_from_value(&msg.content));
        material.push('\n');
    }
    material
}

/// Computes a SHA-256 hash of a message array for caching purposes.
///
/// Deterministic because it hashes the output of `message_hash_material`.
pub fn compute_message_hash(messages: &[OAChatMessage]) -> String {
    let material = message_hash_material(messages);
    let mut hasher = Sha256::new();
    hasher.update(material.as_bytes());
    hex_encode(hasher.finalize())
}

/// Extracts plain text from a JSON value.
///
/// Handles three JSON formats:
/// - String: Returns the string directly
/// - Array: Extracts "text" fields from objects and concatenates
/// - Object: Returns the "text" field if present
///
/// # Arguments
/// * `value` - JSON value to extract text from
///
/// # Returns
/// Extracted text, or empty string if no text found
///
/// # Examples
/// ```
/// use claude_code_openai_gateway::utils::extract_text_from_value;
/// use serde_json::json;
///
/// assert_eq!(extract_text_from_value(&json!("hello")), "hello");
/// assert_eq!(
///     extract_text_from_value(&json!([{"text": "hi"}, {"text": " there"}])),
///     "hi there"
/// );
/// assert_eq!(extract_text_from_value(&json!({"text": "world"})), "world");
/// assert_eq!(extract_text_from_value(&json!(123)), "");
/// ```
pub fn extract_text_from_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Flattens OpenAI message array into system prompt and conversation text.
///
/// Separates system messages from user/assistant messages and formats
/// the conversation as "Role: content" lines.
///
/// # Arguments
/// * `messages` - Slice of OpenAI chat messages to flatten
///
/// # Returns
/// Tuple of (system_prompt, conversation_text)
/// - system_prompt: Last system message content, or default
/// - conversation_text: Formatted conversation with "User:" and "Assistant:" prefixes
///
/// # Examples
/// ```
/// use claude_code_openai_gateway::utils::flatten_messages;
/// use claude_code_openai_gateway::models::openai::OAChatMessage;
/// use serde_json::json;
///
/// let messages = vec![
///     OAChatMessage {
///         role: "system".to_string(),
///         content: json!("Be helpful"),
///     },
///     OAChatMessage {
///         role: "user".to_string(),
///         content: json!("Hello"),
///     },
///     OAChatMessage {
///         role: "assistant".to_string(),
///         content: json!("Hi there!"),
///     },
/// ];
/// let (system, conversation) = flatten_messages(&messages);
/// assert_eq!(system, "Be helpful");
/// assert_eq!(conversation, "User: Hello\nAssistant: Hi there!");
/// ```
pub fn flatten_messages(messages: &[OAChatMessage]) -> (String, String) {
    let mut system = "You are a helpful AI assistant.".to_string();
    let mut blocks = Vec::new();

    for msg in messages {
        let text = extract_text_from_value(&msg.content);
        match msg.role.as_str() {
            "system" if !text.is_empty() => system = text,
            "user" => blocks.push(format!("User: {}", text)),
            "assistant" => blocks.push(format!("Assistant: {}", text)),
            _ => {}
        }
    }

    (system, blocks.join("\n"))
}

/// Returns the OS argument size limit in bytes, if available.
#[allow(dead_code)]
pub fn arg_max_bytes() -> Option<usize> {
    #[cfg(unix)]
    unsafe {
        let value = libc::sysconf(libc::_SC_ARG_MAX);
        if value > 0 {
            Some(value as usize)
        } else {
            None
        }
    }

    #[cfg(not(unix))]
    {
        None
    }
}

/// Estimates environment size in bytes for exec argument accounting.
#[allow(dead_code)]
pub fn environment_size_bytes() -> usize {
    #[cfg(unix)]
    {
        std::env::vars_os()
            .map(|(k, v)| k.as_bytes().len() + 1 + v.as_bytes().len() + 1)
            .sum()
    }

    #[cfg(not(unix))]
    {
        0
    }
}

/// Estimates total argv size in bytes (including NUL terminators).
#[allow(dead_code)]
pub fn argv_size_bytes<I, S>(args: I) -> usize
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().map(|arg| arg.as_ref().len() + 1).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_unix_timestamp() {
        let ts = unix_timestamp();
        assert!(ts > 1_600_000_000);
        assert!(ts < 2_000_000_000);
    }

    #[test]
    fn test_extract_text_from_string() {
        let value = json!("hello world");
        assert_eq!(extract_text_from_value(&value), "hello world");
    }

    #[test]
    fn test_extract_text_from_array() {
        let value = json!([
            {"type": "text", "text": "hello"},
            {"type": "text", "text": " world"}
        ]);
        assert_eq!(extract_text_from_value(&value), "hello world");
    }

    #[test]
    fn test_extract_text_from_object() {
        let value = json!({"text": "test"});
        assert_eq!(extract_text_from_value(&value), "test");
    }

    #[test]
    fn test_extract_text_from_non_text() {
        assert_eq!(extract_text_from_value(&json!(123)), "");
        assert_eq!(extract_text_from_value(&json!(null)), "");
        assert_eq!(extract_text_from_value(&json!(true)), "");
    }

    #[test]
    fn test_compute_message_hash() {
        let messages = vec![
            OAChatMessage {
                role: "user".to_string(),
                content: json!("Hello"),
            },
            OAChatMessage {
                role: "assistant".to_string(),
                content: json!("Hi!"),
            },
        ];
        let hash = compute_message_hash(&messages);
        assert_eq!(hash.len(), 64);

        let hash2 = compute_message_hash(&messages);
        assert_eq!(hash, hash2);

        let messages2 = vec![OAChatMessage {
            role: "user".to_string(),
            content: json!("Different"),
        }];
        let hash3 = compute_message_hash(&messages2);
        assert_ne!(hash, hash3);
    }

    #[test]
    fn test_flatten_messages_with_system() {
        let messages = vec![
            OAChatMessage {
                role: "system".to_string(),
                content: json!("Custom system prompt"),
            },
            OAChatMessage {
                role: "user".to_string(),
                content: json!("Hello"),
            },
            OAChatMessage {
                role: "assistant".to_string(),
                content: json!("Hi there!"),
            },
        ];

        let (system, conversation) = flatten_messages(&messages);
        assert_eq!(system, "Custom system prompt");
        assert_eq!(conversation, "User: Hello\nAssistant: Hi there!");
    }

    #[test]
    fn test_flatten_messages_without_system() {
        let messages = vec![OAChatMessage {
            role: "user".to_string(),
            content: json!("Test"),
        }];

        let (system, conversation) = flatten_messages(&messages);
        assert_eq!(system, "You are a helpful AI assistant.");
        assert_eq!(conversation, "User: Test");
    }

    #[test]
    fn test_flatten_messages_empty() {
        let messages: Vec<OAChatMessage> = vec![];
        let (system, conversation) = flatten_messages(&messages);
        assert_eq!(system, "You are a helpful AI assistant.");
        assert_eq!(conversation, "");
    }

    #[test]
    fn test_flatten_messages_unknown_role() {
        let messages = vec![
            OAChatMessage {
                role: "user".to_string(),
                content: json!("Hi"),
            },
            OAChatMessage {
                role: "tool".to_string(),
                content: json!("Tool result"),
            },
        ];

        let (_, conversation) = flatten_messages(&messages);
        assert_eq!(conversation, "User: Hi");
    }
}
