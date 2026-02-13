//! Parameterized, read-only SQL query builder for rusqlite.
//!
//! Prevents SQL injection by using `rusqlite::params!` for all values.
//! Only supports SELECT queries — no INSERT/UPDATE/DELETE.
//!
//! Follows the existing `SearchUnifiedWorkflowsQuery` pattern from `database/mod.rs`.

use rusqlite::{Connection, Row};
use tracing::debug;

/// A safe, parameterized SQL query builder.
///
/// All values are bound via rusqlite parameters, never interpolated into SQL.
/// Read-only by design: only supports SELECT queries.
pub struct SafeQueryBuilder {
    table: String,
    select_columns: Vec<String>,
    joins: Vec<String>,
    where_clauses: Vec<String>,
    params: Vec<Box<dyn rusqlite::types::ToSql>>,
    order_by: Vec<String>,
    group_by: Vec<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

impl SafeQueryBuilder {
    /// Create a new query builder for the given table.
    ///
    /// Only known table names are accepted to prevent injection via table names.
    pub fn new(table: &str) -> Result<Self, String> {
        if !is_valid_table(table) {
            return Err(format!("Invalid table name: {}", table));
        }
        Ok(Self {
            table: table.to_string(),
            select_columns: Vec::new(),
            joins: Vec::new(),
            where_clauses: Vec::new(),
            params: Vec::new(),
            order_by: Vec::new(),
            group_by: Vec::new(),
            limit: None,
            offset: None,
        })
    }

    /// Set the columns to select. If not called, defaults to `*`.
    pub fn select(mut self, columns: &[&str]) -> Self {
        self.select_columns = columns.iter().map(|c| validate_column(c)).collect();
        self
    }

    /// Add a WHERE clause: `column = value`
    pub fn where_eq(mut self, column: &str, value: impl rusqlite::types::ToSql + 'static) -> Self {
        let idx = self.params.len() + 1;
        self.where_clauses
            .push(format!("{} = ?{}", validate_column(column), idx));
        self.params.push(Box::new(value));
        self
    }

    /// Add a WHERE clause: `column IN (values...)`
    pub fn where_in(mut self, column: &str, values: Vec<String>) -> Self {
        if values.is_empty() {
            // WHERE 1=0 to return no results
            self.where_clauses.push("1=0".to_string());
            return self;
        }
        let col = validate_column(column);
        let placeholders: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", self.params.len() + i + 1))
            .collect();
        self.where_clauses
            .push(format!("{} IN ({})", col, placeholders.join(", ")));
        for v in values {
            self.params.push(Box::new(v));
        }
        self
    }

    /// Add a WHERE clause: `column LIKE pattern`
    pub fn where_like(
        mut self,
        column: &str,
        pattern: impl rusqlite::types::ToSql + 'static,
    ) -> Self {
        let idx = self.params.len() + 1;
        self.where_clauses
            .push(format!("{} LIKE ?{}", validate_column(column), idx));
        self.params.push(Box::new(pattern));
        self
    }

    /// Add a WHERE clause: `column > value`
    pub fn where_gt(mut self, column: &str, value: impl rusqlite::types::ToSql + 'static) -> Self {
        let idx = self.params.len() + 1;
        self.where_clauses
            .push(format!("{} > ?{}", validate_column(column), idx));
        self.params.push(Box::new(value));
        self
    }

    /// Add a WHERE clause: `column < value`
    pub fn where_lt(mut self, column: &str, value: impl rusqlite::types::ToSql + 'static) -> Self {
        let idx = self.params.len() + 1;
        self.where_clauses
            .push(format!("{} < ?{}", validate_column(column), idx));
        self.params.push(Box::new(value));
        self
    }

    /// Add a WHERE clause: `column BETWEEN low AND high`
    pub fn where_between(
        mut self,
        column: &str,
        low: impl rusqlite::types::ToSql + 'static,
        high: impl rusqlite::types::ToSql + 'static,
    ) -> Self {
        let idx_low = self.params.len() + 1;
        let idx_high = self.params.len() + 2;
        self.where_clauses.push(format!(
            "{} BETWEEN ?{} AND ?{}",
            validate_column(column),
            idx_low,
            idx_high
        ));
        self.params.push(Box::new(low));
        self.params.push(Box::new(high));
        self
    }

    /// Add a WHERE clause: `column IS NOT NULL`
    pub fn where_not_null(mut self, column: &str) -> Self {
        self.where_clauses
            .push(format!("{} IS NOT NULL", validate_column(column)));
        self
    }

    /// Add a WHERE clause: `column IS NULL`
    pub fn where_null(mut self, column: &str) -> Self {
        self.where_clauses
            .push(format!("{} IS NULL", validate_column(column)));
        self
    }

    /// Add a WHERE clause: `column != value`
    pub fn where_ne(mut self, column: &str, value: impl rusqlite::types::ToSql + 'static) -> Self {
        let idx = self.params.len() + 1;
        self.where_clauses
            .push(format!("{} != ?{}", validate_column(column), idx));
        self.params.push(Box::new(value));
        self
    }

    /// Add a JOIN clause.
    pub fn join(mut self, join_table: &str, on_clause: &str) -> Self {
        if is_valid_table(join_table) {
            self.joins
                .push(format!("JOIN {} ON {}", join_table, on_clause));
        }
        self
    }

    /// Add a LEFT JOIN clause.
    pub fn left_join(mut self, join_table: &str, on_clause: &str) -> Self {
        if is_valid_table(join_table) {
            self.joins
                .push(format!("LEFT JOIN {} ON {}", join_table, on_clause));
        }
        self
    }

    /// Add ORDER BY clause.
    pub fn order_by(mut self, column: &str, direction: &str) -> Self {
        let dir = match direction.to_uppercase().as_str() {
            "DESC" => "DESC",
            _ => "ASC",
        };
        self.order_by
            .push(format!("{} {}", validate_column(column), dir));
        self
    }

    /// Add GROUP BY clause.
    pub fn group_by(mut self, column: &str) -> Self {
        self.group_by.push(validate_column(column));
        self
    }

    /// Set LIMIT.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set OFFSET.
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Build the SQL string (for debugging).
    pub fn build_sql(&self) -> String {
        let select = if self.select_columns.is_empty() {
            "*".to_string()
        } else {
            self.select_columns.join(", ")
        };

        let mut sql = format!("SELECT {} FROM {}", select, self.table);

        for join in &self.joins {
            sql.push(' ');
            sql.push_str(join);
        }

        if !self.where_clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.where_clauses.join(" AND "));
        }

        if !self.group_by.is_empty() {
            sql.push_str(" GROUP BY ");
            sql.push_str(&self.group_by.join(", "));
        }

        if !self.order_by.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&self.order_by.join(", "));
        }

        if let Some(limit) = self.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        if let Some(offset) = self.offset {
            sql.push_str(&format!(" OFFSET {}", offset));
        }

        sql
    }

    /// Execute the query with a row mapper function.
    pub fn query<T>(
        &self,
        conn: &Connection,
        mapper: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, String> {
        let sql = self.build_sql();
        debug!("SafeQueryBuilder executing: {}", sql);

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let params_slice: Vec<&dyn rusqlite::types::ToSql> =
            self.params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(params_slice.as_slice(), mapper)
            .map_err(|e| format!("Failed to execute query: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Failed to map row: {}", e))?);
        }

        Ok(results)
    }

    /// Execute a COUNT(*) version of this query.
    pub fn count(&self, conn: &Connection) -> Result<i64, String> {
        let where_clause = if self.where_clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.where_clauses.join(" AND "))
        };

        let mut sql = format!("SELECT COUNT(*) FROM {}", self.table);
        for join in &self.joins {
            sql.push(' ');
            sql.push_str(join);
        }
        sql.push_str(&where_clause);

        let params_slice: Vec<&dyn rusqlite::types::ToSql> =
            self.params.iter().map(|p| p.as_ref()).collect();

        conn.query_row(&sql, params_slice.as_slice(), |row| row.get(0))
            .map_err(|e| format!("Failed to count: {}", e))
    }
}

/// Validate and sanitize a column name.
///
/// Only allows alphanumeric characters, underscores, and dots (for table.column).
fn validate_column(col: &str) -> String {
    col.chars()
        .filter(|c| {
            c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '(' || *c == ')' || *c == '*'
        })
        .collect()
}

/// Validate that a table name is one of our known tables.
fn is_valid_table(table: &str) -> bool {
    matches!(
        table,
        "task_runs"
            | "task_run_findings"
            | "task_run_automation"
            | "task_knowledge"
            | "task_knowledge_summaries"
            | "unified_workflows"
            | "learning_outcomes"
            | "learning_patterns"
            | "error_events"
            | "error_events_fts"
            | "workflow_generation_feedback"
            | "task_run_events"
            | "task_run_screenshots"
            | "task_run_playwright_results"
            | "configs"
            | "config_statistics"
            | "orchestrator_verification_results"
            | "orchestrator_checkpoints"
            | "sessions"
            | "session_events"
            | "verification_tests"
            | "test_results"
            | "checks"
            | "check_results"
            | "log_sources"
            | "recordings"
            | "execution_spans"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_select() {
        let builder = SafeQueryBuilder::new("task_runs")
            .unwrap()
            .where_eq("status", "complete".to_string())
            .order_by("created_at", "DESC")
            .limit(10);

        let sql = builder.build_sql();
        assert_eq!(
            sql,
            "SELECT * FROM task_runs WHERE status = ?1 ORDER BY created_at DESC LIMIT 10"
        );
    }

    #[test]
    fn test_select_with_columns() {
        let builder = SafeQueryBuilder::new("task_run_findings")
            .unwrap()
            .select(&["id", "title", "category"])
            .where_eq("status", "resolved".to_string())
            .where_eq("category", "code_bug".to_string());

        let sql = builder.build_sql();
        assert_eq!(
            sql,
            "SELECT id, title, category FROM task_run_findings WHERE status = ?1 AND category = ?2"
        );
    }

    #[test]
    fn test_between() {
        let builder = SafeQueryBuilder::new("error_events")
            .unwrap()
            .where_between(
                "captured_at",
                "2024-01-01".to_string(),
                "2024-12-31".to_string(),
            );

        let sql = builder.build_sql();
        assert_eq!(
            sql,
            "SELECT * FROM error_events WHERE captured_at BETWEEN ?1 AND ?2"
        );
    }

    #[test]
    fn test_invalid_table() {
        assert!(SafeQueryBuilder::new("DROP TABLE users").is_err());
        assert!(SafeQueryBuilder::new("nonexistent").is_err());
    }

    #[test]
    fn test_column_sanitization() {
        assert_eq!(validate_column("name"), "name");
        assert_eq!(validate_column("table.column"), "table.column");
        assert_eq!(validate_column("name; DROP TABLE"), "nameDROPTABLE");
        assert_eq!(validate_column("COUNT(*)"), "COUNT(*)");
    }

    #[test]
    fn test_group_by() {
        let builder = SafeQueryBuilder::new("task_runs")
            .unwrap()
            .select(&["category", "COUNT(*)"])
            .group_by("category")
            .order_by("COUNT(*)", "DESC");

        let sql = builder.build_sql();
        assert_eq!(
            sql,
            "SELECT category, COUNT(*) FROM task_runs GROUP BY category ORDER BY COUNT(*) DESC"
        );
    }

    #[test]
    fn test_where_in() {
        let builder = SafeQueryBuilder::new("task_runs")
            .unwrap()
            .where_in("status", vec!["complete".to_string(), "failed".to_string()]);

        let sql = builder.build_sql();
        assert_eq!(sql, "SELECT * FROM task_runs WHERE status IN (?1, ?2)");
    }

    #[test]
    fn test_sql_injection_in_column_name() {
        // SQL injection attempts in column names should be sanitized
        let builder = SafeQueryBuilder::new("task_runs")
            .unwrap()
            .where_eq("status; DROP TABLE task_runs--", "x".to_string());
        let sql = builder.build_sql();
        // Semicolons should be stripped from column name
        assert!(!sql.contains(';'));
        // The value is always parameterized, never interpolated
        assert!(sql.contains("= ?1"));
        // The column name is sanitized to a single identifier (no spaces in it)
        // "status; DROP TABLE task_runs--" -> "statusDROPTABLEtask_runs"
        assert!(sql.contains("statusDROPTABLEtask_runs"));
    }

    #[test]
    fn test_join() {
        let builder = SafeQueryBuilder::new("task_run_findings")
            .unwrap()
            .select(&["task_run_findings.title", "task_runs.task_name"])
            .join("task_runs", "task_run_findings.task_run_id = task_runs.id")
            .limit(5);

        let sql = builder.build_sql();
        assert!(sql.contains("JOIN task_runs ON"));
        assert!(sql.contains("LIMIT 5"));
    }

    #[test]
    fn test_multiple_conditions() {
        let builder = SafeQueryBuilder::new("error_events")
            .unwrap()
            .where_eq("severity", "critical".to_string())
            .where_not_null("message_embedding")
            .where_like("message", "%timeout%".to_string())
            .order_by("last_seen_at", "DESC")
            .limit(20)
            .offset(10);

        let sql = builder.build_sql();
        assert!(sql.contains("severity = ?1"));
        assert!(sql.contains("message_embedding IS NOT NULL"));
        assert!(sql.contains("message LIKE ?"));
        assert!(sql.contains("OFFSET 10"));
    }
}
