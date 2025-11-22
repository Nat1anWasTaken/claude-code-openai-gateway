//! Claude OpenAI Gateway - OpenAI-compatible API for Claude CLI
//!
//! This gateway translates OpenAI Chat Completion API requests into Claude CLI
//! calls, enabling use of Claude through OpenAI-compatible clients.
//!
//! ## Features
//!
//! - OpenAI Chat Completion API compatibility
//! - Streaming and non-streaming responses
//! - Conversation caching for improved performance
//! - Session resumption across requests
//!
//! ## Architecture
//!
//! The gateway operates by:
//! 1. Receiving OpenAI-formatted chat completion requests
//! 2. Converting them to Claude CLI commands with `--output-format stream-json`
//! 3. Parsing the newline-delimited JSON output from Claude
//! 4. Converting responses back to OpenAI format
//!
//! ## Claude CLI Output Format
//!
//! When run with `--output-format stream-json`, Claude CLI emits newline-delimited
//! JSON records:
//!
//! ```json
//! {"type":"system","subtype":"init","session_id":"..."}
//! {"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}
//! {"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"..."}}}
//! {"type":"result","subtype":"success","is_error":false,"usage":{...}}
//! ```

use axum::{routing::post, Router};
use handlers::{handle_chat_completion, AppState};
use http::header::HeaderName;
use tokio::signal;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

mod cache;
mod claude_cli;
mod handlers;
mod models;
mod utils;

/// Main entry point for the gateway server.
///
/// Starts an HTTP server on port 8080 that handles OpenAI-compatible
/// chat completion requests.
#[tokio::main]
async fn main() {
    init_tracing();

    let x_request_id = HeaderName::from_static("x-request-id");

    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completion))
        .with_state(AppState {})
        .layer(
            ServiceBuilder::new()
                .layer(PropagateRequestIdLayer::new(x_request_id.clone()))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(DefaultMakeSpan::new().level(tracing::Level::INFO))
                        .on_response(
                            DefaultOnResponse::new()
                                .level(tracing::Level::INFO)
                                .latency_unit(tower_http::LatencyUnit::Millis),
                        ),
                )
                .layer(SetRequestIdLayer::new(x_request_id, MakeRequestUuid)),
        );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("failed to bind to 0.0.0.0:8080");

    info!("Claude OpenAI Gateway listening on :8080");
    info!("Endpoint: POST http://localhost:8080/v1/chat/completions");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

/// Waits for SIGTERM/SIGINT (and Ctrl+C on non-Unix) so the container can
/// exit cleanly when Docker sends its default SIGTERM to PID 1.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");

        tokio::select! {
            _ = term.recv() => {
                info!("Received SIGTERM, shutting down...");
            }
            _ = int.recv() => {
                info!("Received SIGINT, shutting down...");
            }
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C, shutting down...");
            }
        }
    }

    #[cfg(not(unix))]
    {
        signal::ctrl_c().await.expect("install Ctrl+C handler");
        info!("Received Ctrl+C, shutting down...");
    }
}

/// Initializes tracing subscriber with env-based level and format.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt = std::env::var("TRACING_FMT").unwrap_or_default();
    let builder = fmt::fmt().with_env_filter(filter).with_target(false);

    if fmt.eq_ignore_ascii_case("json") {
        builder.json().init();
    } else {
        builder.compact().init();
    }
}
