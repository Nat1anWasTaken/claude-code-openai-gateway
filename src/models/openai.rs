//! OpenAI API compatible request and response types.
//!
//! This module defines data structures that match the OpenAI Chat Completion API,
//! allowing clients to use this gateway as a drop-in replacement for OpenAI endpoints.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Incoming OpenAI-compatible chat completion request.
///
/// This struct represents the minimal set of fields required from an OpenAI
/// chat completion request to proxy it to Claude CLI.
#[derive(Deserialize, Debug)]
pub struct ChatRequest {
    /// The model to use (e.g., "claude-3-5-sonnet-20241022")
    pub model: String,

    /// Array of messages in the conversation
    pub messages: Vec<OAChatMessage>,

    /// Whether to stream the response using Server-Sent Events
    #[serde(default)]
    pub stream: bool,
}

/// A single message in an OpenAI chat conversation.
///
/// Messages can have different roles (system, user, assistant) and their
/// content can be a simple string or a structured object/array.
#[derive(Deserialize, Debug)]
pub struct OAChatMessage {
    /// Role of the message sender ("system", "user", or "assistant")
    pub role: String,

    /// Content of the message - can be string, array, or object
    ///
    /// OpenAI supports multiple content formats. We normalize these
    /// to plain text when forwarding to Claude.
    #[serde(default)]
    pub content: Value,
}

/// Complete chat completion response (non-streaming).
///
/// Returned when `stream: false` in the request.
#[derive(Serialize)]
pub struct ChatCompletionResponse {
    /// Unique identifier for this completion
    pub id: String,

    /// Object type, always "chat.completion"
    pub object: String,

    /// Unix timestamp of when the completion was created
    pub created: u64,

    /// Model that generated the completion
    pub model: String,

    /// Array of completion choices (typically one)
    pub choices: Vec<ChatChoice>,

    /// Token usage information, if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

/// A single completion choice in the response.
#[derive(Serialize)]
pub struct ChatChoice {
    /// Index of this choice in the array
    pub index: u32,

    /// Reason the completion finished ("stop", "length", etc.)
    pub finish_reason: String,

    /// The generated message
    pub message: OAChatMessageOut,
}

/// Outgoing message in a chat completion response.
#[derive(Serialize)]
pub struct OAChatMessageOut {
    /// Role of the message sender (always "assistant" for completions)
    pub role: String,

    /// Text content of the message
    pub content: String,
}

/// Streaming chat completion delta (one chunk).
///
/// Sent as Server-Sent Events when `stream: true` in the request.
#[derive(Serialize)]
pub struct StreamDelta {
    /// Unique identifier for this completion
    pub id: String,

    /// Object type, always "chat.completion.chunk"
    pub object: String,

    /// Unix timestamp of when the completion was created
    pub created: u64,

    /// Model that generated the completion
    pub model: String,

    /// Array of delta choices (typically one)
    pub choices: Vec<StreamChoice>,
}

/// A single delta choice in a streaming response.
#[derive(Serialize)]
pub struct StreamChoice {
    /// Index of this choice in the array
    pub index: u32,

    /// The delta content for this chunk
    pub delta: Delta,

    /// Finish reason, only present in the final chunk
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// Delta content in a streaming chunk.
///
/// Contains incremental changes to the message being generated.
#[derive(Serialize)]
pub struct Delta {
    /// Role, only present in the first chunk
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// Incremental text content
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deserialize_chat_request_simple() {
        let json = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let req: ChatRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.model, "claude-3-5-sonnet-20241022");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.stream, false);
    }

    #[test]
    fn test_deserialize_chat_request_with_stream() {
        let json = json!({
            "model": "test-model",
            "messages": [],
            "stream": true
        });

        let req: ChatRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.stream, true);
    }

    #[test]
    fn test_serialize_chat_completion_response() {
        let response = ChatCompletionResponse {
            id: "test-id".to_string(),
            object: "chat.completion".to_string(),
            created: 1234567890,
            model: "test-model".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                finish_reason: "stop".to_string(),
                message: OAChatMessageOut {
                    role: "assistant".to_string(),
                    content: "Hello!".to_string(),
                },
            }],
            usage: None,
        };

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["id"], "test-id");
        assert_eq!(json["choices"][0]["message"]["content"], "Hello!");
    }

    #[test]
    fn test_serialize_stream_delta() {
        let delta = StreamDelta {
            id: "test-id".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1234567890,
            model: "test-model".to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta: Delta {
                    role: Some("assistant".to_string()),
                    content: Some("Hi".to_string()),
                },
                finish_reason: None,
            }],
        };

        let json = serde_json::to_value(&delta).unwrap();
        assert_eq!(json["object"], "chat.completion.chunk");
        assert_eq!(json["choices"][0]["delta"]["content"], "Hi");
    }
}
