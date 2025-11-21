//! HTTP request handlers for the OpenAI-compatible API.
//!
//! This module contains the main request processing logic, including:
//! - Routing incoming chat completion requests
//! - Processing streaming and non-streaming responses
//! - Converting between OpenAI and Claude formats

use crate::cache::{find_cached_prefix, store_session};
use crate::claude_cli::{spawn_claude_cli, ClaudeCliConfig};
use crate::models::claude::{extract_text_from_contents, ClaudeRecord};
use crate::models::error::GatewayError;
use crate::models::openai::{
    ChatChoice, ChatCompletionResponse, ChatRequest, Delta, OAChatMessageOut, StreamChoice,
    StreamDelta,
};
use crate::utils::{compute_message_hash, flatten_messages, unix_timestamp};
use axum::{
    extract::State,
    http::StatusCode,
    response::{sse::Event, sse::KeepAlive, IntoResponse, Response, Sse},
    Json,
};
use futures::stream;
use serde_json::Value;
use std::convert::Infallible;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::ChildStderr,
    sync::mpsc,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use uuid::Uuid;

/// Application state (currently empty, reserved for future use).
#[derive(Clone)]
pub struct AppState {}

/// Main HTTP handler for chat completion requests.
///
/// Routes incoming OpenAI-compatible requests to the appropriate
/// processing function and handles any errors.
///
/// # Arguments
/// * `_state` - Application state (currently unused)
/// * `req` - Parsed chat completion request
///
/// # Returns
/// HTTP response with either streaming SSE or complete JSON
pub async fn handle_chat_completion(
    State(_state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Response {
    match process_chat_request(req).await {
        Ok(resp) => resp,
        Err(err) => {
            eprintln!("handler error: {err:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

/// Processes a chat completion request.
///
/// Determines whether to use streaming or non-streaming mode and
/// manages session caching for conversation resumption.
///
/// # Arguments
/// * `req` - The incoming chat request
///
/// # Returns
/// Response appropriate for the request mode (streaming or complete)
///
/// # Errors
/// Returns `GatewayError` if Claude CLI fails or produces invalid output
async fn process_chat_request(req: ChatRequest) -> Result<Response, GatewayError> {
    println!(
        "incoming request: model={} stream={} messages={}",
        req.model,
        req.stream,
        req.messages.len()
    );

    let conversation_hash = compute_message_hash(&req.messages);
    let (system_prompt, _) = flatten_messages(&req.messages);

    let (resume_session, history_prefix_len) = find_cached_prefix(
        |cut| compute_message_hash(&req.messages[..cut]),
        req.messages.len(),
    )
    .await;

    let new_messages = if history_prefix_len > 0 {
        &req.messages[history_prefix_len..]
    } else {
        &req.messages[..]
    };

    let (_, prompt) = flatten_messages(new_messages);

    let config = ClaudeCliConfig::new(prompt, system_prompt, req.model.clone())
        .with_resume_session(resume_session);

    let mut child = spawn_claude_cli(&config)?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| GatewayError::Spawn(std::io::Error::other("missing stdout")))?;
    let stderr = child.stderr.take();

    if req.stream {
        process_streaming_request(stdout, stderr, req.model, conversation_hash).await
    } else {
        process_non_streaming_request(stdout, stderr, child, req.model, conversation_hash).await
    }
}

/// Processes a streaming chat completion request.
///
/// Creates a Server-Sent Events stream that yields incremental deltas
/// as Claude generates the response.
///
/// # Arguments
/// * `stdout` - Claude CLI stdout stream
/// * `stderr` - Claude CLI stderr stream (for logging)
/// * `model` - Model identifier for response metadata
/// * `conversation_hash` - Hash of conversation for caching
///
/// # Returns
/// SSE streaming response
async fn process_streaming_request(
    stdout: tokio::process::ChildStdout,
    _stderr: Option<ChildStderr>,
    model: String,
    conversation_hash: String,
) -> Result<Response, GatewayError> {
    let (tx, rx) = mpsc::channel::<Result<Event, GatewayError>>(16);

    tokio::spawn(async move {
        stream_claude_output(stdout, tx, model, conversation_hash).await;
    });

    let stream = ReceiverStream::new(rx)
        .chain(stream::once(async { Ok(Event::default().data("[DONE]")) }))
        .map(|item| -> Result<Event, Infallible> {
            match item {
                Ok(ev) => Ok(ev),
                Err(err) => Ok(Event::default().data(format!(r#"{{"error":"{}"}}"#, err))),
            }
        });

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new())
        .into_response())
}

/// Streams Claude CLI output as Server-Sent Events.
///
/// Reads stdout line-by-line, parses JSON records, and sends SSE events
/// for each content delta.
///
/// # Arguments
/// * `stdout` - Claude CLI stdout stream
/// * `tx` - Channel sender for SSE events
/// * `model` - Model identifier
/// * `conversation_hash` - Hash for caching
async fn stream_claude_output(
    stdout: tokio::process::ChildStdout,
    tx: mpsc::Sender<Result<Event, GatewayError>>,
    model: String,
    conversation_hash: String,
) {
    let mut reader = BufReader::new(stdout).lines();
    let created = unix_timestamp();
    let id = format!("chatcmpl-{}", Uuid::new_v4());
    let mut session_id_seen: Option<String> = None;

    let _ = tx
        .send(Ok(make_delta_event(
            &id,
            &model,
            created,
            Some("assistant"),
            "",
        )))
        .await;

    while let Ok(Some(line)) = reader.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        println!("[claude stdout stream] {line}");

        match serde_json::from_str::<ClaudeRecord>(&line) {
            Ok(rec) => {
                if let Some(event) =
                    process_claude_record(rec, &id, &model, created, &mut session_id_seen)
                {
                    if let Err(e) = tx.send(Ok(event)).await {
                        eprintln!("failed to send event: {e}");
                        break;
                    }
                }

                if matches!(
                    serde_json::from_str::<ClaudeRecord>(&line),
                    Ok(ClaudeRecord::Result { .. })
                ) {
                    if let Some(sid) = session_id_seen.clone() {
                        store_session(conversation_hash.clone(), sid).await;
                    }
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(Err(GatewayError::Parse(e))).await;
                break;
            }
        }
    }

    let _ = tx
        .send(Ok(make_done_event(&id, &model, created, None)))
        .await;
}

/// Processes a single Claude CLI record and converts it to an SSE event.
///
/// # Arguments
/// * `record` - Parsed Claude record
/// * `id` - Completion ID
/// * `model` - Model identifier
/// * `created` - Timestamp
/// * `session_id_seen` - Mutable reference to track session ID
///
/// # Returns
/// Optional SSE event if this record should produce one
fn process_claude_record(
    record: ClaudeRecord,
    id: &str,
    model: &str,
    created: u64,
    session_id_seen: &mut Option<String>,
) -> Option<Event> {
    match record {
        ClaudeRecord::SystemInit { session_id, .. } => {
            *session_id_seen = session_id.or_else(|| session_id_seen.clone());
            None
        }
        ClaudeRecord::StreamEvent { event, .. } => event
            .delta
            .and_then(|d| d.text)
            .map(|text| make_delta_event(id, model, created, None, &text)),
        ClaudeRecord::Assistant { message, .. } => {
            let text = extract_text_from_contents(&message.content);
            if !text.is_empty() {
                Some(make_delta_event(id, model, created, None, &text))
            } else {
                None
            }
        }
        ClaudeRecord::Result { .. } => Some(make_done_event(id, model, created, None)),
        ClaudeRecord::Other => None,
    }
}

/// Processes a non-streaming chat completion request.
///
/// Collects the full Claude response before returning a complete
/// chat completion response.
///
/// # Arguments
/// * `stdout` - Claude CLI stdout stream
/// * `stderr` - Claude CLI stderr stream
/// * `child` - Child process handle for waiting
/// * `model` - Model identifier
/// * `conversation_hash` - Hash for caching
///
/// # Returns
/// Complete JSON response
///
/// # Errors
/// Returns error if Claude CLI fails or exits with non-zero status
async fn process_non_streaming_request(
    stdout: tokio::process::ChildStdout,
    stderr: Option<ChildStderr>,
    mut child: tokio::process::Child,
    model: String,
    conversation_hash: String,
) -> Result<Response, GatewayError> {
    let mut reader = BufReader::new(stdout).lines();
    let mut final_text = String::new();
    let mut usage: Option<Value> = None;
    let mut session_id_seen: Option<String> = None;

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        println!("[claude stdout collect] {line}");

        if let Ok(rec) = serde_json::from_str::<ClaudeRecord>(&line) {
            match rec {
                ClaudeRecord::SystemInit { session_id, .. } => {
                    session_id_seen = session_id.or(session_id_seen);
                }
                ClaudeRecord::Assistant { message, .. } => {
                    final_text = extract_text_from_contents(&message.content);
                }
                ClaudeRecord::Result { usage: u, .. } => {
                    usage = Some(u.unwrap_or(Value::Null));
                }
                _ => {}
            }
        }
    }

    if let Some(sid) = session_id_seen {
        store_session(conversation_hash, sid).await;
    }

    let stderr_text = collect_stderr_output(stderr).await?;

    let status = child.wait().await?;
    if !status.success() {
        let msg = if stderr_text.is_empty() {
            format!("claude exited with {}", status)
        } else {
            stderr_text
        };
        return Err(GatewayError::Cli(msg));
    } else if !stderr_text.is_empty() {
        eprintln!("claude stderr: {stderr_text}");
    }

    let created = unix_timestamp();
    let id = format!("chatcmpl-{}", Uuid::new_v4());
    let response = ChatCompletionResponse {
        id,
        object: "chat.completion".into(),
        created,
        model,
        choices: vec![ChatChoice {
            index: 0,
            finish_reason: "stop".into(),
            message: OAChatMessageOut {
                role: "assistant".into(),
                content: final_text,
            },
        }],
        usage,
    };

    Ok(Json(response).into_response())
}

/// Collects all output from stderr stream.
///
/// # Arguments
/// * `stderr` - Optional stderr stream
///
/// # Returns
/// Concatenated stderr output, or empty string if none
///
/// # Errors
/// Returns I/O error if reading fails
async fn collect_stderr_output(stderr: Option<ChildStderr>) -> Result<String, std::io::Error> {
    let mut stderr_text = String::new();

    if let Some(child_stderr) = stderr {
        let mut err_reader = BufReader::new(child_stderr).lines();
        while let Some(line) = err_reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if !stderr_text.is_empty() {
                stderr_text.push('\n');
            }
            stderr_text.push_str(&line);
        }
    }

    Ok(stderr_text)
}

/// Creates a Server-Sent Event for a content delta.
///
/// # Arguments
/// * `id` - Completion ID
/// * `model` - Model identifier
/// * `created` - Unix timestamp
/// * `role` - Optional role (only in first chunk)
/// * `text` - Text content for this delta
///
/// # Returns
/// SSE event with serialized StreamDelta
fn make_delta_event(id: &str, model: &str, created: u64, role: Option<&str>, text: &str) -> Event {
    let delta = StreamDelta {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created,
        model: model.to_string(),
        choices: vec![StreamChoice {
            index: 0,
            delta: Delta {
                role: role.map(|s| s.to_string()),
                content: if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                },
            },
            finish_reason: None,
        }],
    };
    Event::default().data(serde_json::to_string(&delta).unwrap())
}

/// Creates a Server-Sent Event indicating completion.
///
/// # Arguments
/// * `id` - Completion ID
/// * `model` - Model identifier
/// * `created` - Unix timestamp
/// * `_usage` - Optional usage statistics (currently unused)
///
/// # Returns
/// SSE event with finish_reason set to "stop"
fn make_done_event(id: &str, model: &str, created: u64, _usage: Option<Value>) -> Event {
    let choice = StreamChoice {
        index: 0,
        delta: Delta {
            role: None,
            content: None,
        },
        finish_reason: Some("stop".into()),
    };
    let delta = StreamDelta {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created,
        model: model.to_string(),
        choices: vec![choice],
    };
    Event::default().data(serde_json::to_string(&delta).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_delta_event_with_role() {
        let _event = make_delta_event("test-id", "test-model", 1234567890, Some("assistant"), "");
    }

    #[test]
    fn test_make_delta_event_with_text() {
        let _event = make_delta_event("test-id", "test-model", 1234567890, None, "Hello");
    }

    #[test]
    fn test_make_done_event() {
        let _event = make_done_event("test-id", "test-model", 1234567890, None);
    }

    #[test]
    fn test_process_claude_record_system_init() {
        let record = ClaudeRecord::SystemInit {
            subtype: "init".to_string(),
            session_id: Some("test-session".to_string()),
        };
        let mut session_id = None;
        let result = process_claude_record(record, "test-id", "test-model", 12345, &mut session_id);
        assert_eq!(session_id, Some("test-session".to_string()));
        assert!(result.is_none());
    }

    #[test]
    fn test_collect_stderr_output_empty() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = collect_stderr_output(None).await.unwrap();
            assert_eq!(result, "");
        });
    }
}
