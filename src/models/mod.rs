//! Data models for the Claude OpenAI Gateway.
//!
//! This module contains all data structures used throughout the gateway:
//! - OpenAI API compatible types for requests and responses
//! - Claude CLI output types for parsing stream-json format
//! - Error types for gateway operations

pub mod claude;
pub mod error;
pub mod openai;

pub use claude::{ClaudeRecord, ContentBlock, extract_text_from_contents};
pub use error::GatewayError;
pub use openai::{
    ChatChoice, ChatCompletionResponse, ChatRequest, Delta, OAChatMessage, OAChatMessageOut,
    StreamChoice, StreamDelta,
};
