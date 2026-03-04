//! Deduplication processor using signature hashing.

use crate::error_monitor::pipeline::traits::Processor;
use crate::error_monitor::pipeline::types::LogRecord;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Mutex;

/// Processor that deduplicates records by error signature hash.
///
/// Uses a bounded set to track recently seen signatures and filter
/// out duplicates within a processing window.
pub struct DedupProcessor {
    seen: Mutex<HashSet<String>>,
    capacity: usize,
}

impl DedupProcessor {
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: Mutex::new(HashSet::new()),
            capacity,
        }
    }
}

#[async_trait]
impl Processor for DedupProcessor {
    fn name(&self) -> &str {
        "dedup"
    }

    async fn process(&self, records: Vec<LogRecord>) -> Vec<LogRecord> {
        let mut seen = match self.seen.lock() {
            Ok(s) => s,
            Err(_) => return records, // Poisoned lock, pass through
        };

        // Reset if capacity exceeded
        if seen.len() > self.capacity {
            seen.clear();
        }

        records
            .into_iter()
            .filter(|record| {
                if let Some(ref event) = record.parsed {
                    let hash = event.compute_signature_hash();
                    seen.insert(hash) // Returns true if newly inserted
                } else {
                    true // No parsed event, pass through
                }
            })
            .collect()
    }
}
