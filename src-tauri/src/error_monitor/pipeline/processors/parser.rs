//! Parser processor that dispatches to language-specific parsers.

use crate::error_monitor::parsers::{create_parser, LogParser};
use crate::error_monitor::pipeline::traits::Processor;
use crate::error_monitor::pipeline::types::LogRecord;
use crate::error_monitor::types::ParserType;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

/// Processor that parses raw log content into structured ErrorEvents.
pub struct ParserProcessor {
    parsers: Mutex<HashMap<ParserType, Box<dyn LogParser>>>,
}

impl ParserProcessor {
    pub fn new() -> Self {
        Self {
            parsers: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl Processor for ParserProcessor {
    fn name(&self) -> &str {
        "parser"
    }

    async fn process(&self, records: Vec<LogRecord>) -> Vec<LogRecord> {
        records
            .into_iter()
            .filter_map(|mut record| {
                // Skip records that already have parsed events
                if record.parsed.is_some() {
                    return Some(record);
                }

                let parser_type = &record.source_meta.parser_type;

                // Get or create parser
                let mut parsers = self.parsers.lock().ok()?;
                if !parsers.contains_key(parser_type) {
                    parsers.insert(parser_type.clone(), create_parser(parser_type));
                }

                let parser = parsers.get(parser_type)?;
                let errors = parser.parse_content(&record.raw, &record.source_name);

                if let Some(error) = errors.into_iter().next() {
                    record.severity = Some(error.severity.clone());
                    record.timestamp = error.log_timestamp.clone();
                    record.parsed = Some(error);
                    Some(record)
                } else {
                    None // No error found in this record
                }
            })
            .collect()
    }
}
