//! Observed Claude CLI `--output-format stream-json` lines (Nov 20, 2025):
//! ```
//! {"type":"assistant","message":{"content":[{"type":"text","text":"..."}], "...": "..."},"session_id":"...","uuid":"..."}
//! {"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"..."},"index":0},...}
//! {"type":"result","subtype":"success","is_error":false,"usage":{...},"uuid":"..."}
//! ```
//! Root objects are newline-delimited JSON. Text arrives either via `stream_event`
//! deltas (`event.delta.text`) or bundled in an `assistant.message.content[*].text`.
//! A terminating `"type":"result"` record signals completion.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{sse::Event, sse::KeepAlive, IntoResponse, Response, Sse},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    convert::Infallible,
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use std::collections::HashMap;
use std::sync::OnceLock;
use sha2::{Digest, Sha256};
use hex::encode as hex_encode;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {}

static CONTEXT_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, String>>> = OnceLock::new();

fn cache() -> &'static tokio::sync::Mutex<HashMap<String, String>> {
    CONTEXT_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/v1/chat/completions", post(handler))
        .with_state(AppState {});

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("bind 0.0.0.0:8080");
    println!("Server listening on :8080");
    axum::serve(listener, app).await.unwrap();
}

/// Incoming OpenAI-compatible request (minimal fields we care about).
#[derive(Deserialize, Debug)]
struct ChatRequest {
    model: String,
    messages: Vec<OAChatMessage>,
    #[serde(default)]
    stream: bool,
}

#[derive(Deserialize, Debug)]
struct OAChatMessage {
    role: String,
    #[serde(default)]
    content: Value, // string or array/object; we down-convert to text
}

/// Axum handler.
async fn handler(State(_state): State<AppState>, Json(req): Json<ChatRequest>) -> Response {
    match process_request(req).await {
        Ok(resp) => resp,
        Err(err) => {
            eprintln!("handler error: {err:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn process_request(req: ChatRequest) -> Result<Response, GatewayError> {
    println!(
        "incoming request: model={} stream={} messages={}",
        req.model,
        req.stream,
        req.messages.len()
    );
    let messages_len = req.messages.len();
    let (system_prompt, _) = flatten_messages(&req.messages);

    // Find the longest cached history prefix (exclude newest messages).
    let mut resume_session: Option<String> = None;
    let mut history_prefix_len: usize = 0;
    for cut in (1..messages_len).rev() {
        let candidate = &req.messages[..cut];
        let h = history_hash(candidate);
        if let Some(sid) = cache().lock().await.get(&h) {
            resume_session = Some(sid.clone());
            history_prefix_len = cut;
            break;
        }
    }

    let new_slice = if history_prefix_len > 0 {
        &req.messages[history_prefix_len..]
    } else {
        &req.messages[..]
    };
    let (_, prompt) = flatten_messages(new_slice);
    let conversation_hash = history_hash(&req.messages);

    // Spawn Claude CLI.
    let mut cmd = Command::new("claude");
    if let Some(sid) = resume_session.as_ref() {
        cmd.arg("--resume").arg(sid);
    }
    println!(
        "spawning claude: resume={} model={} stream={}",
        resume_session
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("<new>"),
        req.model,
        req.stream
    );

    let mut child = cmd
        .arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--model")
        .arg(req.model.clone())
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
        .arg("--system-prompt")
        .arg(system_prompt)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(GatewayError::Spawn)?;

    let stdout = child
        .stdout
        .take()
        .ok_or(GatewayError::Spawn(std::io::Error::new(
            std::io::ErrorKind::Other,
            "missing stdout",
        )))?;
    let stderr = child.stderr.take();

    if req.stream {
        let (tx, rx) = mpsc::channel::<Result<Event, GatewayError>>(16);
        let model = req.model.clone();
        let conv_hash = conversation_hash.clone();
        let cache_ref = cache();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            let created = unix_ts();
            let id = format!("chatcmpl-{}", Uuid::new_v4());
            let mut session_id_seen: Option<String> = resume_session.clone();
            // First chunk sets role.
            let _ = tx.send(Ok(make_delta_event(&id, &model, created, Some("assistant"), "")))
                .await;
            while let Ok(Some(line)) = reader.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                println!("[claude stdout stream] {line}");
                match serde_json::from_str::<ClaudeRecord>(&line) {
                    Ok(rec) => {
                        match rec {
                            ClaudeRecord::SystemInit { session_id, .. } => {
                                session_id_seen = session_id.or(session_id_seen);
                            }
                            ClaudeRecord::StreamEvent { event, .. } => {
                                if let Some(text) = event.delta.and_then(|d| d.text) {
                                    let _ = tx
                                        .send(Ok(make_delta_event(&id, &model, created, None, &text)))
                                        .await;
                                }
                            }
                            ClaudeRecord::Assistant { message, .. } => {
                                let text = extract_from_contents(&message.content);
                                if !text.is_empty() {
                                    let _ = tx
                                        .send(Ok(make_delta_event(&id, &model, created, None, &text)))
                                        .await;
                                }
                            }
                            ClaudeRecord::Result { .. } => {
                                let done = make_done_event(&id, &model, created, None);
                                let _ = tx.send(Ok(done)).await;
                                if let Some(sid) = session_id_seen.clone() {
                                    let mut guard = cache_ref.lock().await;
                                    guard.insert(conv_hash.clone(), sid);
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(GatewayError::Parse(e))).await;
                        break;
                    }
                }
            }
            let _ = tx.send(Ok(make_done_event(&id, &model, created, None))).await;
        });

        let stream = ReceiverStream::new(rx).map(|item| -> Result<Event, Infallible> {
            match item {
                Ok(ev) => Ok(ev),
                Err(err) => Ok(Event::default().data(format!(r#"{{"error":"{}"}}"#, err))),
            }
        });

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "text/event-stream".parse().unwrap());
        Ok(Sse::new(stream).keep_alive(KeepAlive::new()).into_response())
    } else {
        // Non-stream: collect stdout fully.
        let mut reader = BufReader::new(stdout).lines();
        let mut final_text = String::new();
        let mut usage: Option<Value> = None;
        let mut session_id_seen: Option<String> = resume_session.clone();
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
                        final_text = extract_from_contents(&message.content);
                    }
                    ClaudeRecord::Result { usage: u, .. } => usage = Some(u.unwrap_or(Value::Null)),
                    _ => {}
                }
            }
        }
        if let Some(sid) = session_id_seen {
            let mut guard = cache().lock().await;
            guard.insert(conversation_hash, sid);
        }
        if let Some(child_stderr) = stderr {
            let mut err_reader = BufReader::new(child_stderr).lines();
            if let Some(line) = err_reader.next_line().await? {
                if !line.trim().is_empty() {
                    return Err(GatewayError::Cli(line));
                }
            }
        }
        let created = unix_ts();
        let id = format!("chatcmpl-{}", Uuid::new_v4());
        let response = ChatCompletionResponse {
            id,
            object: "chat.completion".into(),
            created,
            model: req.model,
            choices: vec![ChatChoice {
                index: 0,
                finish_reason: "stop".into(),
                message: OAChatMessageOut {
                    role: "assistant".into(),
                    content: vec![OAContentPart {
                        r#type: "text".into(),
                        text: final_text,
                    }],
                },
            }],
            usage,
        };
        println!(
            "sending completion id={} len={} chars usage_present={}",
            response.id,
            response.choices[0].message.content.iter().map(|c| c.text.len()).sum::<usize>(),
            response.usage.is_some()
        );
        Ok(Json(response).into_response())
    }
}

fn make_delta_event(
    id: &str,
    model: &str,
    created: u64,
    role: Option<&str>,
    text: &str,
) -> Event {
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
                    vec![]
                } else {
                    vec![OAContentPart {
                        r#type: "text".into(),
                        text: text.to_string(),
                    }]
                },
            },
            finish_reason: None,
        }],
    };
    Event::default().data(serde_json::to_string(&delta).unwrap())
}

fn make_done_event(id: &str, model: &str, created: u64, usage: Option<Value>) -> Event {
    let choice = StreamChoice {
        index: 0,
        delta: Delta {
            role: None,
            content: vec![],
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
    let mut event = Event::default().data(serde_json::to_string(&delta).unwrap());
    if let Some(u) = usage {
        event = event.comment(u.to_string());
    }
    event
}

fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn flatten_messages(messages: &[OAChatMessage]) -> (String, String) {
    let mut system = "You are a helpful AI assistant.".to_string();
    let mut blocks = Vec::new();
    for msg in messages {
        let text = extract_text(&msg.content);
        match msg.role.as_str() {
            "system" if !text.is_empty() => system = text,
            "user" => blocks.push(format!("User: {}", text)),
            "assistant" => blocks.push(format!("Assistant: {}", text)),
            _ => {}
        }
    }
    (system, blocks.join("\n"))
}

fn history_hash(messages: &[OAChatMessage]) -> String {
    let mut hasher = Sha256::new();
    for m in messages {
        hasher.update(m.role.as_bytes());
        hasher.update(b":");
        hasher.update(extract_text(&m.content).as_bytes());
        hasher.update(b"\n");
    }
    hex_encode(hasher.finalize())
}

fn extract_text(v: &Value) -> String {
    match v {
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
        _ => "".into(),
    }
}

fn extract_from_contents(contents: &[ContentBlock]) -> String {
    contents
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------- Claude stream types ----------

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ClaudeRecord {
    #[serde(rename = "system")]
    SystemInit {
        #[serde(rename = "subtype")]
        subtype: String,
        session_id: Option<String>,
    },
    #[serde(rename = "assistant")]
    Assistant {
        message: ClaudeMessage,
        session_id: Option<String>,
    },
    #[serde(rename = "stream_event")]
    StreamEvent {
        event: StreamEvent,
        session_id: Option<String>,
    },
    #[serde(rename = "result")]
    Result {
        subtype: String,
        is_error: Option<bool>,
        usage: Option<Value>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
struct ClaudeMessage {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
struct StreamEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    delta: Option<EventDelta>,
}

#[derive(Deserialize, Debug)]
struct EventDelta {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

// ---------- OpenAI response payloads ----------

#[derive(Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<ChatChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Value>,
}

#[derive(Serialize)]
struct ChatChoice {
    index: u32,
    finish_reason: String,
    message: OAChatMessageOut,
}

#[derive(Serialize)]
struct OAChatMessageOut {
    role: String,
    content: Vec<OAContentPart>,
}

#[derive(Serialize)]
struct OAContentPart {
    #[serde(rename = "type")]
    r#type: String,
    text: String,
}

#[derive(Serialize)]
struct StreamDelta {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<StreamChoice>,
}

#[derive(Serialize)]
struct StreamChoice {
    index: u32,
    delta: Delta,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
}

#[derive(Serialize)]
struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default)]
    content: Vec<OAContentPart>,
}

#[derive(Error, Debug)]
enum GatewayError {
    #[error("failed to spawn claude: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("failed to parse claude output: {0}")]
    Parse(#[source] serde_json::Error),
    #[error("claude stderr: {0}")]
    Cli(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
