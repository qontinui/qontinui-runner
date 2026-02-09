//! Claude CLI stream-json protocol types and codec.
//!
//! This module implements the bidirectional NDJSON protocol used by Claude CLI
//! when invoked with `--input-format stream-json --output-format stream-json`.

pub mod codec;
pub mod request_id;
pub mod types;
