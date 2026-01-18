//! Issue builder utilities

use crate::check_executor::types::{CheckIssue, IssueSeverity};

/// Builder for creating CheckIssue instances
pub struct IssueBuilder {
    file: String,
    line: Option<u32>,
    column: Option<u32>,
    end_line: Option<u32>,
    end_column: Option<u32>,
    code: Option<String>,
    message: String,
    severity: IssueSeverity,
    fixable: bool,
}

impl IssueBuilder {
    pub fn new(file: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line: None,
            column: None,
            end_line: None,
            end_column: None,
            code: None,
            message: message.into(),
            severity: IssueSeverity::Error,
            fixable: false,
        }
    }

    pub fn line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    pub fn column(mut self, column: u32) -> Self {
        self.column = Some(column);
        self
    }

    pub fn end_line(mut self, end_line: u32) -> Self {
        self.end_line = Some(end_line);
        self
    }

    pub fn end_column(mut self, end_column: u32) -> Self {
        self.end_column = Some(end_column);
        self
    }

    pub fn code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn severity(mut self, severity: IssueSeverity) -> Self {
        self.severity = severity;
        self
    }

    pub fn warning(mut self) -> Self {
        self.severity = IssueSeverity::Warning;
        self
    }

    pub fn error(mut self) -> Self {
        self.severity = IssueSeverity::Error;
        self
    }

    pub fn info(mut self) -> Self {
        self.severity = IssueSeverity::Info;
        self
    }

    pub fn fixable(mut self, fixable: bool) -> Self {
        self.fixable = fixable;
        self
    }

    pub fn build(self) -> CheckIssue {
        CheckIssue {
            file: self.file,
            line: self.line,
            column: self.column,
            end_line: self.end_line,
            end_column: self.end_column,
            code: self.code,
            message: self.message,
            severity: self.severity,
            fixable: self.fixable,
            fix_applied: None,
        }
    }
}
