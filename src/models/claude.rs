//! Claude CLI output types.
//!
//! This module defines types for parsing the newline-delimited JSON output
//! from the Claude CLI when run with `--output-format stream-json`.
//!
//! The Claude CLI emits different record types:
//! - `system`: Initialization messages with session IDs
//! - `assistant`: Complete assistant messages
//! - `stream_event`: Streaming content deltas
//! - `result`: Final completion status and usage stats

use serde::Deserialize;
use serde_json::Value;

/// A single newline-delimited JSON record from Claude CLI output.
///
/// Claude CLI emits different record types depending on the event.
/// Each record is tagged with a "type" field for discrimination.
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ClaudeRecord {
    /// System initialization message, contains session ID for resumption.
    #[serde(rename = "system")]
    SystemInit {
        /// Subtype of system message (e.g., "init")
        #[serde(rename = "subtype")]
        subtype: String,

        /// Session ID for resuming this conversation later
        session_id: Option<String>,
    },

    /// Complete assistant message with full content.
    ///
    /// Usually emitted at the start or end of a turn.
    #[serde(rename = "assistant")]
    Assistant {
        /// The message content blocks
        message: ClaudeMessage,

        /// Associated session ID
        session_id: Option<String>,
    },

    /// Streaming event containing incremental content deltas.
    ///
    /// Emitted during streaming to provide real-time updates.
    #[serde(rename = "stream_event")]
    StreamEvent {
        /// The streaming event details
        event: StreamEvent,

        /// Associated session ID
        session_id: Option<String>,
    },

    /// Final result record indicating completion.
    ///
    /// Contains usage statistics and success/error status.
    #[serde(rename = "result")]
    Result {
        /// Subtype of result (e.g., "success", "error")
        subtype: String,

        /// Whether this result represents an error
        is_error: Option<bool>,

        /// Token usage and other statistics
        usage: Option<Value>,
    },

    /// Unknown or unhandled record type.
    #[serde(other)]
    Other,
}

/// A message from Claude containing content blocks.
#[derive(Deserialize, Debug)]
pub struct ClaudeMessage {
    /// Array of content blocks (text, tool use, etc.)
    pub content: Vec<ContentBlock>,
}

/// A single content block within a Claude message.
///
/// Content can be text, tool use, tool results, or other types.
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// Text content block.
    #[serde(rename = "text")]
    Text {
        /// The text content
        text: String,
    },

    /// Unknown or unhandled content block type.
    #[serde(other)]
    Other,
}

/// A streaming event from Claude CLI.
///
/// Events represent incremental updates during message generation.
#[derive(Deserialize, Debug)]
pub struct StreamEvent {
    /// Type of streaming event (e.g., "content_block_delta")
    #[serde(rename = "type")]
    pub kind: String,

    /// Delta content, if this is a content update event
    #[serde(default)]
    pub delta: Option<EventDelta>,
}

/// Delta content in a streaming event.
///
/// Contains the incremental text or JSON being generated.
#[derive(Deserialize, Debug)]
pub struct EventDelta {
    /// Type of delta (e.g., "text_delta")
    #[serde(rename = "type")]
    pub kind: String,

    /// Incremental text content
    #[serde(default)]
    pub text: Option<String>,

    /// Incremental JSON content (for tool use)
    #[serde(default)]
    pub partial_json: Option<String>,
}

/// Extracts all text content from an array of content blocks.
///
/// Filters out non-text blocks and concatenates text from all text blocks.
///
/// # Arguments
/// * `contents` - Array of content blocks to extract text from
///
/// # Returns
/// Concatenated text from all text blocks, or empty string if none found
///
/// # Examples
/// ```
/// use claude_code_openai_gateway::models::claude::{ContentBlock, extract_text_from_contents};
///
/// let blocks = vec![
///     ContentBlock::Text { text: "Hello ".to_string() },
///     ContentBlock::Text { text: "world!".to_string() },
/// ];
/// assert_eq!(extract_text_from_contents(&blocks), "Hello world!");
/// ```
pub fn extract_text_from_contents(contents: &[ContentBlock]) -> String {
    contents
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deserialize_system_init() {
        let json = json!({
            "type": "system",
            "subtype": "init",
            "session_id": "test-session-123"
        });

        let record: ClaudeRecord = serde_json::from_value(json).unwrap();
        match record {
            ClaudeRecord::SystemInit { subtype, session_id } => {
                assert_eq!(subtype, "init");
                assert_eq!(session_id, Some("test-session-123".to_string()));
            }
            _ => panic!("Expected SystemInit"),
        }
    }

    #[test]
    fn test_deserialize_assistant_message() {
        let json = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "Hello!"}
                ]
            },
            "session_id": "test-session"
        });

        let record: ClaudeRecord = serde_json::from_value(json).unwrap();
        match record {
            ClaudeRecord::Assistant { message, .. } => {
                assert_eq!(message.content.len(), 1);
                match &message.content[0] {
                    ContentBlock::Text { text } => assert_eq!(text, "Hello!"),
                    _ => panic!("Expected Text block"),
                }
            }
            _ => panic!("Expected Assistant"),
        }
    }

    #[test]
    fn test_deserialize_stream_event() {
        let json = json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "type": "text_delta",
                    "text": "Hi"
                }
            },
            "session_id": "test-session"
        });

        let record: ClaudeRecord = serde_json::from_value(json).unwrap();
        match record {
            ClaudeRecord::StreamEvent { event, .. } => {
                assert_eq!(event.kind, "content_block_delta");
                assert_eq!(event.delta.unwrap().text, Some("Hi".to_string()));
            }
            _ => panic!("Expected StreamEvent"),
        }
    }

    #[test]
    fn test_deserialize_result() {
        let json = json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "usage": {"input_tokens": 10, "output_tokens": 20}
        });

        let record: ClaudeRecord = serde_json::from_value(json).unwrap();
        match record {
            ClaudeRecord::Result { subtype, is_error, usage } => {
                assert_eq!(subtype, "success");
                assert_eq!(is_error, Some(false));
                assert!(usage.is_some());
            }
            _ => panic!("Expected Result"),
        }
    }

    #[test]
    fn test_extract_text_from_contents() {
        let blocks = vec![
            ContentBlock::Text {
                text: "Hello ".to_string(),
            },
            ContentBlock::Text {
                text: "world!".to_string(),
            },
        ];
        assert_eq!(extract_text_from_contents(&blocks), "Hello world!");
    }

    #[test]
    fn test_extract_text_from_empty_contents() {
        let blocks: Vec<ContentBlock> = vec![];
        assert_eq!(extract_text_from_contents(&blocks), "");
    }
}
