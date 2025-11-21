//! Session cache for Claude CLI conversation resumption.
//!
//! This module manages a global cache that maps conversation history hashes
//! to Claude CLI session IDs. This enables resuming conversations with context
//! caching, improving performance and reducing costs.

use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex;

/// Global session cache singleton.
///
/// Maps conversation history hashes to Claude CLI session IDs.
static CONTEXT_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Returns a reference to the global session cache.
///
/// Initializes the cache on first access using `OnceLock` for thread-safe
/// lazy initialization.
///
/// # Returns
/// Reference to the mutex-protected session cache
///
/// # Examples
/// ```no_run
/// use claude_code_openai_gateway::cache::get_cache;
///
/// #[tokio::main]
/// async fn main() {
///     let cache = get_cache();
///     let mut guard = cache.lock().await;
///     guard.insert("hash123".to_string(), "session456".to_string());
/// }
/// ```
pub fn get_cache() -> &'static Mutex<HashMap<String, String>> {
    CONTEXT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Finds the longest cached history prefix for a given set of messages.
///
/// Searches backwards through message history to find the longest prefix
/// that has a cached session ID. This allows resuming from the most recent
/// cached state.
///
/// # Arguments
/// * `message_hashes` - Function that computes hash for a message slice
/// * `total_messages` - Total number of messages in the conversation
///
/// # Returns
/// Tuple of (session_id, prefix_length) if a cached prefix is found,
/// or (None, 0) if no cached prefix exists
///
/// # Examples
/// ```no_run
/// use claude_code_openai_gateway::cache::{get_cache, find_cached_prefix};
///
/// #[tokio::main]
/// async fn main() {
///     let hash_fn = |slice_len: usize| format!("hash_{}", slice_len);
///     let (session_id, prefix_len) = find_cached_prefix(hash_fn, 5).await;
///     if let Some(sid) = session_id {
///         println!("Found cached session {} at prefix length {}", sid, prefix_len);
///     }
/// }
/// ```
pub async fn find_cached_prefix<F>(
    message_hashes: F,
    total_messages: usize,
) -> (Option<String>, usize)
where
    F: Fn(usize) -> String,
{
    let cache = get_cache().lock().await;

    for cut in (1..total_messages).rev() {
        let hash = message_hashes(cut);
        if let Some(session_id) = cache.get(&hash) {
            return (Some(session_id.clone()), cut);
        }
    }

    (None, 0)
}

/// Stores a session ID in the cache for a given conversation hash.
///
/// # Arguments
/// * `conversation_hash` - Hash of the full conversation history
/// * `session_id` - Claude CLI session ID to cache
///
/// # Examples
/// ```no_run
/// use claude_code_openai_gateway::cache::store_session;
///
/// #[tokio::main]
/// async fn main() {
///     store_session("hash123".to_string(), "session456".to_string()).await;
/// }
/// ```
pub async fn store_session(conversation_hash: String, session_id: String) {
    let mut cache = get_cache().lock().await;
    cache.insert(conversation_hash, session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache_store_and_retrieve() {
        let cache = get_cache();
        {
            let mut guard = cache.lock().await;
            guard.insert("test_hash".to_string(), "test_session".to_string());
        }

        {
            let guard = cache.lock().await;
            assert_eq!(guard.get("test_hash"), Some(&"test_session".to_string()));
        }
    }

    #[tokio::test]
    async fn test_store_session() {
        store_session("hash1".to_string(), "session1".to_string()).await;

        let cache = get_cache().lock().await;
        assert_eq!(cache.get("hash1"), Some(&"session1".to_string()));
    }

    #[tokio::test]
    async fn test_find_cached_prefix_found() {
        // Setup: cache a hash for prefix length 3
        store_session("prefix_3".to_string(), "session_3".to_string()).await;

        let hash_fn = |len: usize| format!("prefix_{}", len);
        let (session_id, prefix_len) = find_cached_prefix(hash_fn, 5).await;

        assert_eq!(session_id, Some("session_3".to_string()));
        assert_eq!(prefix_len, 3);
    }

    #[tokio::test]
    async fn test_find_cached_prefix_not_found() {
        let hash_fn = |len: usize| format!("nonexistent_{}", len);
        let (session_id, prefix_len) = find_cached_prefix(hash_fn, 5).await;

        assert_eq!(session_id, None);
        assert_eq!(prefix_len, 0);
    }
}
