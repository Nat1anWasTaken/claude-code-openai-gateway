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
use crate::utils::{compute_message_hash, flatten_messages, message_hash_material, unix_timestamp};
use axum::{
    extract::{Extension, State},
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
use tower_http::request_id::RequestId;
use tracing::{debug, error, info, info_span, trace, warn, Instrument};
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
    Extension(req_id_ext): Extension<RequestId>,
    Json(req): Json<ChatRequest>,
) -> Response {
    let req_id = req_id_ext
        .header_value()
        .to_str()
        .ok()
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let span = info_span!("request", req_id = %req_id, model = %req.model, stream = req.stream, messages = req.messages.len());

    match process_chat_request(req, req_id).instrument(span).await {
        Ok(resp) => resp,
        Err(err) => {
            error!(error = ?err, "handler error");
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
async fn process_chat_request(req: ChatRequest, req_id: String) -> Result<Response, GatewayError> {
    info!("incoming request");

    let conversation_hash = compute_message_hash(&req.messages);
    debug!(conversation_hash = %conversation_hash, "computed conversation hash");
    trace!(material = %message_hash_material(&req.messages).replace('\n', "\\n"), "conversation hash material");
    let (system_prompt, _) = flatten_messages(&req.messages);

    let (mut resume_session, mut history_prefix_len) = find_cached_prefix(
        |cut| {
            let hash = compute_message_hash(&req.messages[..cut]);
            trace!(prefix_len = cut, hash = %hash, "cache lookup candidate");
            hash
        },
        req.messages.len(),
    )
    .await;

    // If the cached prefix already covers all messages, don't resume—otherwise we'd
    // send an empty delta to Claude and get an empty response.
    if history_prefix_len >= req.messages.len() {
        debug!(
            history_prefix_len,
            "cache covers entire request, disabling resume"
        );
        resume_session = None;
        history_prefix_len = 0;
    }

    info!(
        cache_resume = resume_session.as_deref().unwrap_or("<none>"),
        history_prefix_len, "cache decision"
    );

    let new_messages = if history_prefix_len > 0 {
        &req.messages[history_prefix_len..]
    } else {
        &req.messages[..]
    };

    let (_, prompt) = flatten_messages(new_messages);

    let config = ClaudeCliConfig::new(prompt, system_prompt, req.model.clone())
        .with_resume_session(resume_session.clone());

    let mut child = spawn_claude_cli(&config)?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| GatewayError::Spawn(std::io::Error::other("missing stdout")))?;
    let stderr = child.stderr.take();

    if req.stream {
        process_streaming_request(stdout, stderr, req.model, conversation_hash, req_id).await
    } else {
        process_non_streaming_request(stdout, stderr, child, req.model, conversation_hash, req_id)
            .await
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
    req_id: String,
) -> Result<Response, GatewayError> {
    let (tx, rx) = mpsc::channel::<Result<Event, GatewayError>>(16);

    tokio::spawn(async move {
        stream_claude_output(stdout, tx, model, conversation_hash, req_id).await;
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
    req_id: String,
) {
    let mut reader = BufReader::new(stdout).lines();
    let created = unix_timestamp();
    let id = format!("chatcmpl-{}", Uuid::new_v4());
    let mut session_id_seen: Option<String> = None;
    let mut chunks_sent = 0usize;

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
        trace!(%req_id, raw = %line, "claude stdout");

        match serde_json::from_str::<ClaudeRecord>(&line) {
            Ok(rec) => {
                if let Some(event) =
                    process_claude_record(rec, &id, &model, created, &mut session_id_seen)
                {
                    if let Err(e) = tx.send(Ok(event)).await {
                        warn!(%req_id, error = %e, "failed to send SSE event");
                        break;
                    }
                    chunks_sent += 1;
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
                // Claude CLI can emit non-JSON lines; ignore them for streaming clients.
                warn!(%req_id, error = %e, raw = %line, "failed to parse claude stdout line");
                continue;
            }
        }
    }

    let _ = tx
        .send(Ok(make_done_event(&id, &model, created, None)))
        .await;

    info!(%req_id, chunks_sent, "streaming response finished");
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
            .and_then(|d| {
                if let Some(text) = d.text {
                    Some(text)
                } else {
                    d.partial_json
                }
            })
            .map(|text| make_delta_event(id, model, created, None, &text)),
        ClaudeRecord::Assistant { .. } => None,
        ClaudeRecord::Result { .. } => None,
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
    req_id: String,
) -> Result<Response, GatewayError> {
    let mut reader = BufReader::new(stdout).lines();
    let mut final_text = String::new();
    let mut usage: Option<Value> = None;
    let mut session_id_seen: Option<String> = None;

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        trace!(%req_id, raw = %line, "claude stdout");

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
        warn!(%req_id, stderr = %stderr_text, "claude stderr");
    }

    info!(%req_id, chars = final_text.len(), usage_present = usage.is_some(), "non-stream response ready");

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
    let delta = build_delta_payload(id, model, created, role, text);
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
    let delta = build_done_payload(id, model, created);
    Event::default().data(serde_json::to_string(&delta).unwrap())
}

/// Builds the JSON payload for a delta event (shared by tests).
fn build_delta_payload(
    id: &str,
    model: &str,
    created: u64,
    role: Option<&str>,
    text: &str,
) -> StreamDelta {
    StreamDelta {
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
    }
}

/// Builds the JSON payload for the terminal done event (shared by tests).
fn build_done_payload(id: &str, model: &str, created: u64) -> StreamDelta {
    StreamDelta {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created,
        model: model.to_string(),
        choices: vec![StreamChoice {
            index: 0,
            delta: Delta {
                role: None,
                content: None,
            },
            finish_reason: Some("stop".into()),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::clear_cache;
    use axum::response::sse::Sse;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use serial_test::serial;
    use std::convert::Infallible;
    use std::time::Duration;
    use tokio::process::Command;
    use tokio::time::timeout;

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
    fn test_build_delta_payload_json_shape() {
        let delta = build_delta_payload("id", "m", 42, Some("assistant"), "hello");
        assert_eq!(delta.object, "chat.completion.chunk");
        assert_eq!(delta.choices.len(), 1);
        let choice = &delta.choices[0];
        assert_eq!(choice.delta.role.as_deref(), Some("assistant"));
        assert_eq!(choice.delta.content.as_deref(), Some("hello"));
        assert!(choice.finish_reason.is_none());
    }

    #[test]
    fn test_build_done_payload_has_stop_reason() {
        let delta = build_done_payload("id", "m", 42);
        assert_eq!(delta.choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(delta.choices[0].delta.role.is_none());
        assert!(delta.choices[0].delta.content.is_none());
    }

    #[tokio::test]
    #[serial]
    async fn test_stream_claude_output_emits_events_and_caches_session() {
        clear_cache().await;

        // Fake Claude stdout with init -> stream delta -> result lines.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("printf '%s\n' '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sid-1\"}' '{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}}' '{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"name\\\":\\\"tool\\\"}\"}}}' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}'")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn fake claude");

        let stdout = child
            .stdout
            .take()
            .expect("missing stdout from fake claude");

        let (tx, mut rx) = mpsc::channel::<Result<Event, GatewayError>>(8);
        let handle = tokio::spawn(stream_claude_output(
            stdout,
            tx,
            "test-model".into(),
            "conv-hash".into(),
            "req-123".into(),
        ));

        let mut payloads = Vec::new();
        while let Some(evt) = rx.recv().await {
            let data = extract_sse_data(evt.unwrap()).await;
            payloads.push(data);
        }
        handle.await.unwrap();
        timeout(Duration::from_secs(1), child.wait())
            .await
            .unwrap()
            .unwrap();

        // First event is role bootstrap, second is streamed text, third is partial json, last is done.
        let first: serde_json::Value = serde_json::from_str(&payloads[0]).unwrap();
        assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
        let streamed: serde_json::Value = serde_json::from_str(&payloads[1]).unwrap();
        assert_eq!(streamed["choices"][0]["delta"]["content"], "hello");
        let partial: serde_json::Value = serde_json::from_str(&payloads[2]).unwrap();
        assert_eq!(
            partial["choices"][0]["delta"]["content"],
            "{\"name\":\"tool\"}"
        );

        let cache = crate::cache::get_cache().lock().await;
        assert_eq!(cache.get("conv-hash"), Some(&"sid-1".to_string()));
    }

    #[tokio::test]
    #[serial]
    async fn test_process_non_streaming_request_collects_usage_and_caches() {
        clear_cache().await;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("printf '%s\n' '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sid-2\"}' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi there\"}]}}' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"usage\":{\"output_tokens\":5}}'")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn fake claude");

        let stdout = child
            .stdout
            .take()
            .expect("missing stdout from fake claude");

        let response = process_non_streaming_request(
            stdout,
            None,
            child,
            "test-model".into(),
            "hash-usage".into(),
            "req-usage".into(),
        )
        .await
        .expect("non-streaming request failed");

        let collected = BodyExt::collect(response.into_body()).await.unwrap();
        let body = String::from_utf8(collected.to_bytes().to_vec()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["choices"][0]["message"]["content"], "hi there");
        assert_eq!(json["model"], "test-model");
        assert_eq!(json["usage"]["output_tokens"], 5);

        let cache = crate::cache::get_cache().lock().await;
        assert_eq!(cache.get("hash-usage"), Some(&"sid-2".to_string()));
    }

    async fn extract_sse_data(event: Event) -> String {
        let stream = futures::stream::once(async { Ok::<_, Infallible>(event) });
        let response = Sse::new(stream).into_response();
        let collected = BodyExt::collect(response.into_body()).await.unwrap();
        let bytes: Bytes = collected.to_bytes();
        // SSE format: data: <payload>\n\n
        for line in String::from_utf8(bytes.to_vec()).unwrap().lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                return data.to_string();
            }
        }
        panic!("no data line in SSE event");
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
