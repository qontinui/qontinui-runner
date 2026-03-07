//! StdinWriter - thread-safe writer for Claude CLI's stdin pipe.
//!
//! Wraps ChildStdin in a Mutex to allow serialized writes from multiple threads.

use serde::Serialize;
use std::io::Write;
use std::process::ChildStdin;
use std::sync::Mutex;
use tracing::{debug, info, warn};

use crate::claude_protocol::codec::encode_message;
use crate::str_utils::truncate_str;

/// Thread-safe wrapper around Claude CLI's stdin pipe.
pub struct StdinWriter {
    stdin: Mutex<Option<ChildStdin>>,
}

impl StdinWriter {
    /// Create a new StdinWriter from a ChildStdin handle.
    pub fn new(stdin: ChildStdin) -> Self {
        Self {
            stdin: Mutex::new(Some(stdin)),
        }
    }

    /// Write a serializable message as NDJSON to stdin.
    pub fn write_message<T: Serialize>(&self, msg: &T) -> Result<(), String> {
        let encoded = encode_message(msg)?;
        let mut guard = self
            .stdin
            .lock()
            .map_err(|e| format!("StdinWriter lock poisoned: {}", e))?;

        match guard.as_mut() {
            Some(stdin) => {
                stdin
                    .write_all(encoded.as_bytes())
                    .map_err(|e| format!("Failed to write to stdin: {}", e))?;
                stdin
                    .flush()
                    .map_err(|e| format!("Failed to flush stdin: {}", e))?;
                let preview = if encoded.len() > 200 {
                    truncate_str(&encoded, 200)
                } else {
                    &encoded
                };
                info!(
                    "[STDIN_WRITER] Wrote NDJSON message ({} bytes): {}",
                    encoded.len(),
                    preview.trim()
                );
                Ok(())
            }
            None => {
                warn!("Attempted to write to closed stdin");
                Err("Stdin is closed".to_string())
            }
        }
    }

    /// Close the stdin pipe (signals EOF to the child process).
    pub fn close(&self) {
        if let Ok(mut guard) = self.stdin.lock() {
            if guard.take().is_some() {
                debug!("StdinWriter: stdin pipe closed");
            }
        }
    }

    /// Check if stdin is still open.
    pub fn is_open(&self) -> bool {
        self.stdin
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }
}

impl std::fmt::Debug for StdinWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let open = self.is_open();
        f.debug_struct("StdinWriter")
            .field("is_open", &open)
            .finish()
    }
}
