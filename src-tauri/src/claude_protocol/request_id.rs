//! Thread-safe request ID generator for the Claude CLI protocol.

use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique request ID in the format `req_{counter}_{hex}`.
pub fn next_request_id() -> String {
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let random = rand::random::<u32>();
    format!("req_{}_{:08x}", count, random)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unique_ids() {
        let id1 = next_request_id();
        let id2 = next_request_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("req_"));
        assert!(id2.starts_with("req_"));
    }

    #[test]
    fn test_format() {
        let id = next_request_id();
        let parts: Vec<&str> = id.split('_').collect();
        assert_eq!(parts[0], "req");
        // Counter part
        assert!(parts[1].parse::<u64>().is_ok());
        // Hex part
        assert_eq!(parts[2].len(), 8);
    }
}
