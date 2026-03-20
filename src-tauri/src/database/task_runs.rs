//! Task run CRUD operations, AI session management, and automation tracking.
//!
//! Contains all CheckpointDb methods related to task runs.

use chrono::Utc;
use rusqlite::{params, OptionalExtension, Result as SqliteResult};
use tracing::{info, warn};

use super::types::*;
use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Task Run Operations (Simplified Task Model)
    // ========================================================================

    /// Create a new task run using the builder pattern.
    ///
    /// This is the canonical method for creating task runs. Use `CreateTaskRunInput::new()`
    /// with builder methods to construct the input.
    ///
    /// # Example
    /// ```rust
    /// use crate::database::{CheckpointDb, CreateTaskRunInput};
    ///
    /// let input = CreateTaskRunInput::new("task-123", "My Task")
    ///     .with_prompt("Do something useful")
    ///     .with_config_id("config-456")
    ///     .with_workflow_type("unified");
    ///
    /// let task_run = db.create_task_run(&input)?;
    /// ```
    ///
    /// # Workflow Types
    /// - `"unified"` - Uses LoopController for verification-agentic loop. External code
    ///   (TaskMonitor, legacy session code) should NOT modify status.
    /// - `"legacy_session"` - Legacy session-based execution
    /// - `"automation_only"` - Pure automation without AI
    /// - `None` - Legacy/unspecified (for backward compatibility)
    pub fn create_task_run(&self, input: &CreateTaskRunInput) -> Result<TaskRun, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let auto_continue_val = input.auto_continue.unwrap_or(true);
        let task_type = input.task_type.as_deref().unwrap_or("task");

        // Default root_task_run_id to self if not provided (root-level task)
        let effective_root = input.root_task_run_id.as_deref().unwrap_or(&input.id);

        // Auto-fill runner_port from CheckpointDb if not explicitly set on the input
        let effective_runner_port = input.runner_port.or_else(|| self.get_runner_port());

        conn.execute(
            r#"
            INSERT INTO task_runs (id, task_name, prompt, task_type, status, sessions_count, max_sessions,
                                   output_log, auto_continue, execution_steps_json, log_sources_json,
                                   config_id, workflow_name, workflow_id, workflow_type,
                                   parent_task_run_id, root_task_run_id, depth,
                                   workspace_id, triggered_by, bridge_id,
                                   is_reflection, reflection_source_task_run_id,
                                   is_follow_up, follow_up_source_task_run_id,
                                   is_fixer, fixer_source_task_run_id,
                                   is_meta_optimizer,
                                   runner_port,
                                   created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, 'running', 0, ?5, '', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?27)
            "#,
            params![
                input.id,
                input.task_name,
                input.prompt,
                task_type,
                input.max_sessions.map(|v| v as i64),
                auto_continue_val as i32,
                input.execution_steps_json,
                input.log_sources_json,
                input.config_id,
                input.workflow_name,
                input.workflow_id,
                input.workflow_type,
                input.parent_task_run_id,
                effective_root,
                input.depth as i64,
                input.workspace_id,
                input.triggered_by,
                input.bridge_id,
                input.is_reflection as i32,
                input.reflection_source_task_run_id,
                input.is_follow_up as i32,
                input.follow_up_source_task_run_id,
                input.is_fixer as i32,
                input.fixer_source_task_run_id,
                input.is_meta_optimizer as i32,
                effective_runner_port.map(|p| p as i64),
                now
            ],
        )
        .map_err(|e| format!("Failed to create task run: {}", e))?;

        Ok(TaskRun {
            id: input.id.clone(),
            task_name: input.task_name.clone(),
            prompt: input.prompt.clone(),
            task_type: task_type.to_string(),
            status: "running".to_string(),
            sessions_count: 0,
            max_sessions: input.max_sessions,
            output_log: String::new(),
            error_message: None,
            auto_continue: auto_continue_val,
            execution_steps_json: input.execution_steps_json.clone(),
            log_sources_json: input.log_sources_json.clone(),
            config_id: input.config_id.clone(),
            workflow_name: input.workflow_name.clone(),
            workflow_id: input.workflow_id.clone(),
            summary: None,
            ai_summary: None,
            goal_achieved: None,
            remaining_work: None,
            summary_generated_at: None,
            transition_history_json: None,
            workflow_type: input.workflow_type.clone(),
            workspace_id: input.workspace_id.clone(),
            triggered_by: input.triggered_by.clone(),
            parent_task_run_id: input.parent_task_run_id.clone(),
            root_task_run_id: Some(effective_root.to_string()),
            depth: input.depth,
            bridge_id: input.bridge_id.clone(),
            result_data: None,
            is_reflection: input.is_reflection,
            reflection_source_task_run_id: input.reflection_source_task_run_id.clone(),
            is_follow_up: input.is_follow_up,
            follow_up_source_task_run_id: input.follow_up_source_task_run_id.clone(),
            is_fixer: input.is_fixer,
            fixer_source_task_run_id: input.fixer_source_task_run_id.clone(),
            is_meta_optimizer: input.is_meta_optimizer,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        })
    }

    /// Create a new task run with basic parameters.
    ///
    /// **Deprecated:** Use `create_task_run` with `CreateTaskRunInput::new()` builder instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_task_run with CreateTaskRunInput::new().with_prompt() builder instead"
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_run_legacy(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
    ) -> Result<TaskRun, String> {
        let mut input = CreateTaskRunInput::new(id, task_name);
        if let Some(p) = prompt {
            input = input.with_prompt(p);
        }
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        if let Some(ac) = auto_continue {
            input = input.with_auto_continue(ac);
        }
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Some(lsj) = log_sources_json {
            input = input.with_log_sources_json(lsj);
        }
        self.create_task_run(&input)
    }

    /// Create a new task run with full configuration options.
    ///
    /// **Deprecated:** Use `create_task_run` with `CreateTaskRunInput::new()` builder instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_task_run with CreateTaskRunInput::new().with_*() builder instead"
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_run_with_config(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        task_type: &str,
        config_id: Option<&str>,
        workflow_name: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
    ) -> Result<TaskRun, String> {
        let mut input = CreateTaskRunInput::new(id, task_name).with_task_type(task_type);
        if let Some(p) = prompt {
            input = input.with_prompt(p);
        }
        if let Some(cid) = config_id {
            input = input.with_config_id(cid);
        }
        if let Some(wn) = workflow_name {
            input = input.with_workflow_name(wn);
        }
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        if let Some(ac) = auto_continue {
            input = input.with_auto_continue(ac);
        }
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Some(lsj) = log_sources_json {
            input = input.with_log_sources_json(lsj);
        }
        self.create_task_run(&input)
    }

    /// Create a new task run with full configuration options including workflow_type.
    ///
    /// **Deprecated:** Use `create_task_run` with `CreateTaskRunInput::new()` builder instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_task_run with CreateTaskRunInput::new().with_*() builder instead"
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_run_with_workflow_type(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        task_type: &str,
        config_id: Option<&str>,
        workflow_name: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
        workflow_type: Option<&str>,
    ) -> Result<TaskRun, String> {
        let mut input = CreateTaskRunInput::new(id, task_name).with_task_type(task_type);
        if let Some(p) = prompt {
            input = input.with_prompt(p);
        }
        if let Some(cid) = config_id {
            input = input.with_config_id(cid);
        }
        if let Some(wn) = workflow_name {
            input = input.with_workflow_name(wn);
        }
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        if let Some(ac) = auto_continue {
            input = input.with_auto_continue(ac);
        }
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Some(lsj) = log_sources_json {
            input = input.with_log_sources_json(lsj);
        }
        if let Some(wt) = workflow_type {
            input = input.with_workflow_type(wt);
        }
        self.create_task_run(&input)
    }

    /// Create a new task run with full configuration options including hierarchy fields.
    ///
    /// **Deprecated:** Use `create_task_run` with `CreateTaskRunInput::new()` builder instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_task_run with CreateTaskRunInput::new().with_*() builder instead"
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_run_with_hierarchy(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        task_type: &str,
        config_id: Option<&str>,
        workflow_name: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
        workflow_type: Option<&str>,
        parent_task_run_id: Option<&str>,
        root_task_run_id: Option<&str>,
        depth: u32,
        workspace_id: Option<&str>,
        triggered_by: Option<&str>,
        bridge_id: Option<&str>,
    ) -> Result<TaskRun, String> {
        let mut input = CreateTaskRunInput::new(id, task_name)
            .with_task_type(task_type)
            .with_depth(depth);
        if let Some(p) = prompt {
            input = input.with_prompt(p);
        }
        if let Some(cid) = config_id {
            input = input.with_config_id(cid);
        }
        if let Some(wn) = workflow_name {
            input = input.with_workflow_name(wn);
        }
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        if let Some(ac) = auto_continue {
            input = input.with_auto_continue(ac);
        }
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Some(lsj) = log_sources_json {
            input = input.with_log_sources_json(lsj);
        }
        if let Some(wt) = workflow_type {
            input = input.with_workflow_type(wt);
        }
        if let Some(ptri) = parent_task_run_id {
            input = input.with_parent_task_run_id(ptri);
        }
        if let Some(rtri) = root_task_run_id {
            input = input.with_root_task_run_id(rtri);
        }
        if let Some(wid) = workspace_id {
            input = input.with_workspace_id(wid);
        }
        if let Some(tb) = triggered_by {
            input = input.with_triggered_by(tb);
        }
        if let Some(bid) = bridge_id {
            input = input.with_bridge_id(bid);
        }
        self.create_task_run(&input)
    }

    /// Get a task run by ID.
    /// Note: output_log is reconstructed from chunks table for backward compatibility.
    pub fn get_task_run(&self, id: &str) -> Result<Option<TaskRun>, String> {
        let conn = self.get_conn()?;

        // Get the task_run metadata including all fields
        let result: SqliteResult<TaskRun> = conn.query_row(
            r#"
            SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                   execution_steps_json, log_sources_json, config_id, workflow_name, workflow_id,
                   COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved, remaining_work,
                   summary_generated_at, transition_history_json, workflow_type,
                   workspace_id, triggered_by,
                   parent_task_run_id, root_task_run_id, depth, bridge_id, result_data,
                   COALESCE(is_reflection, 0) as is_reflection, reflection_source_task_run_id,
                   COALESCE(is_follow_up, 0) as is_follow_up, follow_up_source_task_run_id,
                   COALESCE(is_fixer, 0) as is_fixer, fixer_source_task_run_id,
                   COALESCE(is_meta_optimizer, 0) as is_meta_optimizer,
                   created_at, updated_at, completed_at
            FROM task_runs
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(), // Will be filled from chunks
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: row.get(9)?,
                    log_sources_json: row.get(10)?,
                    config_id: row.get(11)?,
                    workflow_name: row.get(12)?,
                    workflow_id: row.get(13)?,
                    summary: row.get(14)?,
                    ai_summary: row.get(15)?,
                    goal_achieved: row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                    remaining_work: row.get(17)?,
                    summary_generated_at: row.get(18)?,
                    transition_history_json: row.get(19)?,
                    workflow_type: row.get(20)?,
                    workspace_id: row.get(21)?,
                    triggered_by: row.get(22)?,
                    parent_task_run_id: row.get(23)?,
                    root_task_run_id: row.get(24)?,
                    depth: row.get::<_, Option<i64>>(25)?.unwrap_or(0) as u32,
                    bridge_id: row.get(26)?,
                    result_data: row.get(27)?,
                    is_reflection: row.get::<_, i32>(28)? != 0,
                    reflection_source_task_run_id: row.get(29)?,
                    is_follow_up: row.get::<_, i32>(30)? != 0,
                    follow_up_source_task_run_id: row.get(31)?,
                    is_fixer: row.get::<_, i32>(32)? != 0,
                    fixer_source_task_run_id: row.get(33)?,
                    is_meta_optimizer: row.get::<_, i32>(34).unwrap_or(0) != 0,
                    created_at: row.get(35)?,
                    updated_at: row.get(36)?,
                    completed_at: row.get(37)?,
                })
            },
        );

        match result {
            Ok(mut task_run) => {
                // Get output from chunks
                drop(conn); // Release connection before calling another method
                task_run.output_log = self.get_full_task_output(id).unwrap_or_default();
                Ok(Some(task_run))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get task run: {}", e)),
        }
    }

    /// Get all child task runs (direct children) for a parent task.
    ///
    /// Returns task runs where `parent_task_run_id` matches the given parent ID.
    /// This only returns direct children (depth = parent.depth + 1), not all descendants.
    ///
    /// # Example
    /// ```ignore
    /// let children = db.get_child_task_runs("parent-task-123")?;
    /// for child in children {
    ///     println!("Child task: {} (depth: {})", child.task_name, child.depth);
    /// }
    /// ```
    pub fn get_child_task_runs(&self, parent_task_run_id: &str) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json, config_id, workflow_name, workflow_id,
                       COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved, remaining_work,
                       summary_generated_at, transition_history_json, workflow_type,
                       workspace_id, triggered_by,
                       parent_task_run_id, root_task_run_id, depth, bridge_id, result_data,
                       COALESCE(is_reflection, 0) as is_reflection, reflection_source_task_run_id,
                       COALESCE(is_follow_up, 0) as is_follow_up, follow_up_source_task_run_id,
                       COALESCE(is_fixer, 0) as is_fixer, fixer_source_task_run_id,
                       COALESCE(is_meta_optimizer, 0) as is_meta_optimizer,
                       created_at, updated_at, completed_at
                FROM task_runs
                WHERE parent_task_run_id = ?1
                ORDER BY created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare child task query: {}", e))?;

        let task_runs = stmt
            .query_map(params![parent_task_run_id], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(), // Not fetching chunks for performance
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: row.get(9)?,
                    log_sources_json: row.get(10)?,
                    config_id: row.get(11)?,
                    workflow_name: row.get(12)?,
                    workflow_id: row.get(13)?,
                    summary: row.get(14)?,
                    ai_summary: row.get(15)?,
                    goal_achieved: row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                    remaining_work: row.get(17)?,
                    summary_generated_at: row.get(18)?,
                    transition_history_json: row.get(19)?,
                    workflow_type: row.get(20)?,
                    workspace_id: row.get(21)?,
                    triggered_by: row.get(22)?,
                    parent_task_run_id: row.get(23)?,
                    root_task_run_id: row.get(24)?,
                    depth: row.get::<_, Option<i64>>(25)?.unwrap_or(0) as u32,
                    bridge_id: row.get(26)?,
                    result_data: row.get(27)?,
                    is_reflection: row.get::<_, i32>(28)? != 0,
                    reflection_source_task_run_id: row.get(29)?,
                    is_follow_up: row.get::<_, i32>(30)? != 0,
                    follow_up_source_task_run_id: row.get(31)?,
                    is_fixer: row.get::<_, i32>(32)? != 0,
                    fixer_source_task_run_id: row.get(33)?,
                    is_meta_optimizer: row.get::<_, i32>(34).unwrap_or(0) != 0,
                    created_at: row.get(35)?,
                    updated_at: row.get(36)?,
                    completed_at: row.get(37)?,
                })
            })
            .map_err(|e| format!("Failed to query child tasks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get all task runs in a hierarchy (all descendants of a root task).
    ///
    /// Returns task runs where `root_task_run_id` matches the given root ID.
    /// This includes all descendants at any depth level.
    ///
    /// # Example
    /// ```ignore
    /// let all_tasks = db.get_task_run_hierarchy("root-task-123")?;
    /// for task in all_tasks {
    ///     let indent = "  ".repeat(task.depth as usize);
    ///     println!("{}Task: {} (depth: {})", indent, task.task_name, task.depth);
    /// }
    /// ```
    pub fn get_task_run_hierarchy(&self, root_task_run_id: &str) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json, config_id, workflow_name, workflow_id,
                       COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved, remaining_work,
                       summary_generated_at, transition_history_json, workflow_type,
                       workspace_id, triggered_by,
                       parent_task_run_id, root_task_run_id, depth, bridge_id, result_data,
                       COALESCE(is_reflection, 0) as is_reflection, reflection_source_task_run_id,
                       COALESCE(is_follow_up, 0) as is_follow_up, follow_up_source_task_run_id,
                       COALESCE(is_fixer, 0) as is_fixer, fixer_source_task_run_id,
                       COALESCE(is_meta_optimizer, 0) as is_meta_optimizer,
                       created_at, updated_at, completed_at
                FROM task_runs
                WHERE root_task_run_id = ?1 AND id != ?1
                ORDER BY depth ASC, created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare hierarchy query: {}", e))?;

        let task_runs = stmt
            .query_map(params![root_task_run_id], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(), // Not fetching chunks for performance
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: row.get(9)?,
                    log_sources_json: row.get(10)?,
                    config_id: row.get(11)?,
                    workflow_name: row.get(12)?,
                    workflow_id: row.get(13)?,
                    summary: row.get(14)?,
                    ai_summary: row.get(15)?,
                    goal_achieved: row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                    remaining_work: row.get(17)?,
                    summary_generated_at: row.get(18)?,
                    transition_history_json: row.get(19)?,
                    workflow_type: row.get(20)?,
                    workspace_id: row.get(21)?,
                    triggered_by: row.get(22)?,
                    parent_task_run_id: row.get(23)?,
                    root_task_run_id: row.get(24)?,
                    depth: row.get::<_, Option<i64>>(25)?.unwrap_or(0) as u32,
                    bridge_id: row.get(26)?,
                    result_data: row.get(27)?,
                    is_reflection: row.get::<_, i32>(28)? != 0,
                    reflection_source_task_run_id: row.get(29)?,
                    is_follow_up: row.get::<_, i32>(30)? != 0,
                    follow_up_source_task_run_id: row.get(31)?,
                    is_fixer: row.get::<_, i32>(32)? != 0,
                    fixer_source_task_run_id: row.get(33)?,
                    is_meta_optimizer: row.get::<_, i32>(34).unwrap_or(0) != 0,
                    created_at: row.get(35)?,
                    updated_at: row.get(36)?,
                    completed_at: row.get(37)?,
                })
            })
            .map_err(|e| format!("Failed to query task hierarchy: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Append output to a task run and increment session count.
    /// Returns true if [TASK_COMPLETE] marker was found in the appended text.
    ///
    /// Uses O(1) chunk insertion instead of O(n) string concatenation.
    /// Output is stored in the task_run_output_chunks table for efficient appending.
    ///
    /// # Arguments
    /// * `id` - Task run ID
    /// * `output` - Output text to append
    /// * `increment_session` - Whether to increment the session count
    /// * `check_completion_marker` - Whether to check for [TASK_COMPLETE] marker and mark task complete.
    ///   Set to `false` for unified workflows where verification is the authority on completion.
    ///
    /// NOTE: This method handles task completion inline to avoid multiple connection acquisitions.
    pub fn append_task_output_ex(
        &self,
        id: &str,
        output: &str,
        increment_session: bool,
        check_completion_marker: bool,
    ) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get next chunk sequence number
        let next_seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(chunk_sequence), 0) + 1 FROM task_run_output_chunks WHERE task_run_id = ?",
                params![id],
                |row| row.get(0),
            )
            .unwrap_or(1);

        // Insert new chunk (O(1) operation)
        conn.execute(
            "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content, created_at) VALUES (?, ?, ?, ?)",
            params![id, next_seq, output, now],
        )
        .map_err(|e| format!("Failed to insert output chunk: {}", e))?;

        // Update task_run metadata only (no string concatenation)
        let session_increment = if increment_session { 1 } else { 0 };
        conn.execute(
            r#"
            UPDATE task_runs SET
                sessions_count = sessions_count + ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![session_increment, now, id],
        )
        .map_err(|e| format!("Failed to update task run metadata: {}", e))?;

        // Only check for completion marker if requested
        // Unified workflows set check_completion_marker=false because verification is the authority
        let is_complete = check_completion_marker
            && output
                .lines()
                .any(|line| line.trim() == TASK_COMPLETE_MARKER);
        if is_complete {
            // IMPORTANT: Inline the completion logic here instead of calling complete_task_run()
            // to avoid nested lock acquisition (we already hold the conn lock above).
            conn.execute(
                r#"
                UPDATE task_runs SET
                    status = 'completed',
                    updated_at = ?1,
                    completed_at = ?1
                WHERE id = ?2 AND status = 'running'
                "#,
                params![now, id],
            )
            .map_err(|e| format!("Failed to complete task run: {}", e))?;

            info!("Task run {} marked completed via append_task_output", id);
        }

        Ok(is_complete)
    }

    /// Append output to a task run (legacy wrapper - checks for completion marker).
    ///
    /// This is the backward-compatible version that always checks for [TASK_COMPLETE].
    /// For unified workflows, use `append_task_output_ex` with `check_completion_marker=false`.
    pub fn append_task_output(
        &self,
        id: &str,
        output: &str,
        increment_session: bool,
    ) -> Result<bool, String> {
        self.append_task_output_ex(id, output, increment_session, true)
    }

    // ========================================================================
    // Workflow AI Sessions (restart survival)
    // ========================================================================

    /// Create a new workflow AI session record.
    /// Called when a Claude CLI subprocess is spawned for a workflow phase.
    pub fn create_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
        claude_cli_session_id: &str,
    ) -> Result<i64, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO workflow_ai_sessions
                (task_run_id, iteration, phase, stage_index, claude_cli_session_id, session_started_at, status)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')
            ON CONFLICT (task_run_id, iteration, phase, COALESCE(stage_index, -1))
            DO UPDATE SET
                claude_cli_session_id = ?5,
                session_started_at = ?6,
                session_completed_at = NULL,
                output_length = 0,
                status = 'running'
            "#,
            params![task_run_id, iteration, phase, stage_index, claude_cli_session_id, now],
        )
        .map_err(|e| format!("Failed to create workflow AI session: {}", e))?;

        let row_id = conn.last_insert_rowid();
        info!(
            "Created workflow AI session: task={}, iter={}, phase={}, cli_session={}",
            task_run_id, iteration, phase, claude_cli_session_id
        );
        Ok(row_id)
    }

    /// Mark a workflow AI session as completed, failed, or interrupted.
    pub fn complete_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
        status: &str,
        output_length: i64,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE workflow_ai_sessions
            SET status = ?1, session_completed_at = ?2, output_length = ?3
            WHERE task_run_id = ?4 AND iteration = ?5 AND phase = ?6
              AND COALESCE(stage_index, -1) = COALESCE(?7, -1)
            "#,
            params![
                status,
                now,
                output_length,
                task_run_id,
                iteration,
                phase,
                stage_index
            ],
        )
        .map_err(|e| format!("Failed to complete workflow AI session: {}", e))?;

        Ok(())
    }

    /// Get the most recent AI session for a task run, filtered by phase and iteration.
    /// Returns (claude_cli_session_id, status) if found.
    pub fn get_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
    ) -> Result<Option<(String, String)>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT claude_cli_session_id, status
            FROM workflow_ai_sessions
            WHERE task_run_id = ?1 AND iteration = ?2 AND phase = ?3
            ORDER BY id DESC
            LIMIT 1
            "#,
            params![task_run_id, iteration, phase],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );

        match result {
            Ok(session) => Ok(Some(session)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get workflow AI session: {}", e)),
        }
    }

    /// Mark all running workflow AI sessions as interrupted.
    /// Called on startup to clean up sessions from a previous runner instance.
    pub fn mark_running_ai_sessions_interrupted(&self) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let count = conn
            .execute(
                r#"
                UPDATE workflow_ai_sessions
                SET status = 'interrupted', session_completed_at = ?1
                WHERE status = 'running'
                "#,
                params![now],
            )
            .map_err(|e| format!("Failed to mark AI sessions interrupted: {}", e))?;

        if count > 0 {
            info!(
                "Marked {} running AI sessions as interrupted on startup",
                count
            );
        }
        Ok(count)
    }

    /// Flush partial AI output to task_run_output_chunks during a running session.
    /// Uses a dedicated chunk_type marker so the final output can replace it.
    pub fn flush_partial_ai_output(
        &self,
        task_run_id: &str,
        output: &str,
        iteration: i32,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let like_pattern = format!(
            "\n--- AI Output (Iteration {} — in progress) ---%",
            iteration
        );
        let formatted = format!(
            "\n--- AI Output (Iteration {} — in progress) ---\n{}\n",
            iteration, output
        );

        // Wrap DELETE + INSERT in a transaction so partial output is never lost
        // if the runner crashes between the two operations.
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Failed to begin flush transaction: {}", e))?;

        // Delete any previous partial flush for this iteration
        // (we always write the full accumulated output, not deltas)
        tx.execute(
            r#"
            DELETE FROM task_run_output_chunks
            WHERE task_run_id = ?1
              AND content LIKE ?2
            "#,
            params![task_run_id, like_pattern],
        )
        .map_err(|e| format!("Failed to delete previous partial flush: {}", e))?;

        // Insert the current partial output
        let next_seq: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(chunk_sequence), 0) + 1 FROM task_run_output_chunks WHERE task_run_id = ?",
                params![task_run_id],
                |row| row.get(0),
            )
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to get max chunk_sequence for {}: {} — defaulting to 1", task_run_id, e);
                1
            });

        tx.execute(
            "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content, created_at) VALUES (?, ?, ?, ?)",
            params![task_run_id, next_seq, formatted, now],
        )
        .map_err(|e| format!("Failed to flush partial AI output: {}", e))?;

        tx.commit()
            .map_err(|e| format!("Failed to commit flush transaction: {}", e))?;

        Ok(())
    }

    /// Delete partial (in-progress) output chunks for a given iteration.
    /// Called when the final output is written, so the partial flush is replaced.
    pub fn delete_partial_ai_output(
        &self,
        task_run_id: &str,
        iteration: i32,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            DELETE FROM task_run_output_chunks
            WHERE task_run_id = ?1
              AND content LIKE ?2
            "#,
            params![
                task_run_id,
                format!(
                    "\n--- AI Output (Iteration {} — in progress) ---%",
                    iteration
                )
            ],
        )
        .map_err(|e| format!("Failed to delete partial AI output: {}", e))?;

        Ok(())
    }

    /// Mark a task run as complete.
    ///
    /// For unified workflows, this should ONLY be called by the LoopController.
    /// Other code paths (TaskMonitor, legacy session code) should check workflow_type
    /// before calling this method. Consider using `complete_task_run_if_allowed` instead.
    pub fn complete_task_run(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'completed',
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to complete task run: {}", e))?;

        info!("Task run {} marked completed", id);
        Ok(())
    }

    /// Mark a task run as complete, but only if it's not a unified workflow.
    ///
    /// Unified workflows should only have status modified by the LoopController.
    /// This method checks workflow_type and skips the update if it's "unified".
    ///
    /// # Returns
    /// - `Ok(true)` if the task was marked complete
    /// - `Ok(false)` if the task is a unified workflow and was NOT modified
    /// - `Err(...)` if there was a database error
    pub fn complete_task_run_if_allowed(&self, id: &str, caller: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        // Check workflow_type first
        let workflow_type: Option<String> = conn
            .query_row(
                "SELECT workflow_type FROM task_runs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to get workflow_type: {}", e))?
            .flatten();

        if workflow_type.as_deref() == Some("unified") {
            warn!(
                "BLOCKED: {} attempted to complete unified workflow task {} - only LoopController should modify status",
                caller, id
            );
            return Ok(false);
        }

        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'completed',
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to complete task run: {}", e))?;

        info!(
            "Task run {} marked completed by {} (workflow_type={:?})",
            id, caller, workflow_type
        );
        Ok(true)
    }

    /// Update a task run's status without changing other fields.
    ///
    /// Used by the unified workflow loop controller to reset task status to "running"
    /// at the start of each iteration, preventing external modifications from
    /// prematurely marking the task as complete or failed.
    pub fn update_task_run_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![status, now, id],
        )
        .map_err(|e| format!("Failed to update task run status: {}", e))?;

        info!("Task run {} status updated to '{}'", id, status);
        Ok(())
    }

    /// Get the output_log for a task run (from chunks table).
    ///
    /// Returns `Ok(Some(output))` if the task run exists and has output,
    /// `Ok(None)` if the task run has no output, or an error string.
    pub fn get_task_run_output(&self, id: &str) -> Result<Option<String>, String> {
        let output = self.get_full_task_output(id)?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(Some(output))
        }
    }

    /// Update the task_name for a task run.
    pub fn update_task_run_name(&self, id: &str, name: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                task_name = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![name, now, id],
        )
        .map_err(|e| format!("Failed to update task run name: {}", e))?;

        info!("Task run {} renamed to '{}'", id, name);
        Ok(())
    }

    /// Update the result_data JSON field on a task run.
    ///
    /// Used by meta-workflow steps (e.g. save_workflow_artifact) to store
    /// structured results like generated workflow IDs.
    pub fn update_task_run_result_data(&self, id: &str, result_data: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                result_data = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![result_data, now, id],
        )
        .map_err(|e| format!("Failed to update task run result_data: {}", e))?;

        info!("Task run {} result_data updated", id);
        Ok(())
    }

    /// Get the result_data JSON from a task run.
    pub fn get_task_run_result_data(&self, id: &str) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT result_data FROM task_runs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Failed to get result_data: {}", e))?
        .ok_or_else(|| format!("Task run {} not found", id))
        .map(|v: Option<String>| v)
    }

    /// Mark a task run as failed.
    ///
    /// For unified workflows, this should ONLY be called by the LoopController.
    /// Other code paths (TaskMonitor, legacy session code) should check workflow_type
    /// before calling this method. Consider using `fail_task_run_if_allowed` instead.
    pub fn fail_task_run(&self, id: &str, error_message: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'failed',
                error_message = ?1,
                updated_at = ?2,
                completed_at = ?2
            WHERE id = ?3
            "#,
            params![error_message, now, id],
        )
        .map_err(|e| format!("Failed to fail task run: {}", e))?;

        info!("Task run {} marked failed: {}", id, error_message);
        Ok(())
    }

    /// Mark a task run as failed, but only if it's not a unified workflow.
    ///
    /// Unified workflows should only have status modified by the LoopController.
    /// This method checks workflow_type and skips the update if it's "unified".
    ///
    /// # Returns
    /// - `Ok(true)` if the task was marked failed
    /// - `Ok(false)` if the task is a unified workflow and was NOT modified
    /// - `Err(...)` if there was a database error
    pub fn fail_task_run_if_allowed(
        &self,
        id: &str,
        error_message: &str,
        caller: &str,
    ) -> Result<bool, String> {
        let conn = self.get_conn()?;

        // Check workflow_type first
        let workflow_type: Option<String> = conn
            .query_row(
                "SELECT workflow_type FROM task_runs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to get workflow_type: {}", e))?
            .flatten();

        if workflow_type.as_deref() == Some("unified") {
            warn!(
                "BLOCKED: {} attempted to fail unified workflow task {} with error '{}' - only LoopController should modify status",
                caller, id, error_message
            );
            return Ok(false);
        }

        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'failed',
                error_message = ?1,
                updated_at = ?2,
                completed_at = ?2
            WHERE id = ?3
            "#,
            params![error_message, now, id],
        )
        .map_err(|e| format!("Failed to fail task run: {}", e))?;

        info!(
            "Task run {} marked failed by {} (workflow_type={:?}): {}",
            id, caller, workflow_type, error_message
        );
        Ok(true)
    }

    /// Stop a task run.
    /// Also disables auto_continue to prevent multi-step tasks from restarting.
    pub fn stop_task_run(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'stopped',
                auto_continue = 0,
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to stop task run: {}", e))?;

        Ok(())
    }

    /// Pause a running task run.
    /// Sets the status to 'paused' so the loop controller will wait instead of proceeding.
    /// Only pauses tasks that are currently 'running'.
    pub fn pause_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let rows = conn
            .execute(
                r#"
            UPDATE task_runs SET
                status = 'paused',
                updated_at = ?1
            WHERE id = ?2 AND status = 'running'
            "#,
                params![now, id],
            )
            .map_err(|e| format!("Failed to pause task run: {}", e))?;

        Ok(rows > 0)
    }

    /// Unpause a paused task run, setting it back to 'running'.
    /// Only unpauses tasks that are currently 'paused'.
    pub fn unpause_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let rows = conn
            .execute(
                r#"
            UPDATE task_runs SET
                status = 'running',
                updated_at = ?1
            WHERE id = ?2 AND status = 'paused'
            "#,
                params![now, id],
            )
            .map_err(|e| format!("Failed to unpause task run: {}", e))?;

        Ok(rows > 0)
    }

    /// Update execution steps for a task run.
    /// Used to add/update deterministic execution steps that should be re-run on session resume.
    pub fn update_task_run_execution_steps(
        &self,
        id: &str,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                execution_steps_json = ?1,
                log_sources_json = ?2,
                updated_at = ?3
            WHERE id = ?4
            "#,
            params![execution_steps_json, log_sources_json, now, id],
        )
        .map_err(|e| format!("Failed to update task run execution steps: {}", e))?;

        Ok(())
    }

    /// Update runtime context for a task run.
    /// Used for storing execution context, replay lineage, and other runtime metadata.
    pub fn update_task_run_runtime_context(
        &self,
        id: &str,
        runtime_context_json: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                runtime_context_json = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![runtime_context_json, now, id],
        )
        .map_err(|e| format!("Failed to update task run runtime context: {}", e))?;

        Ok(())
    }

    /// Update the transition history for a task run.
    /// This stores the orchestrator's state transition history for stage-based recap.
    pub fn update_task_run_transition_history(
        &self,
        id: &str,
        transition_history_json: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                transition_history_json = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![transition_history_json, now, id],
        )
        .map_err(|e| format!("Failed to update task run transition history: {}", e))?;

        Ok(())
    }

    /// Get runtime context for a task run.
    pub fn get_task_run_runtime_context(&self, id: &str) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: rusqlite::Result<Option<String>> = conn.query_row(
            "SELECT runtime_context_json FROM task_runs WHERE id = ?",
            params![id],
            |row| row.get(0),
        );

        match result {
            Ok(context) => Ok(context),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get runtime context: {}", e)),
        }
    }

    /// Get all running (incomplete) task runs.
    /// Note: output_log is empty for performance. Use get_full_task_output() to get output.
    /// Includes execution_steps_json and log_sources_json for re-execution on resume.
    pub fn get_running_task_runs(&self, runner_port: Option<u16>) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json,
                       COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at,
                       workspace_id, triggered_by,
                       created_at, updated_at, completed_at,
                       task_type, config_id, workflow_name
                FROM task_runs
                WHERE status IN ('running', 'paused') AND (workflow_type IS NULL OR workflow_type != 'chat')
                      AND (?1 IS NULL OR runner_port IS NULL OR runner_port = ?1)
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map(params![runner_port.map(|p| p as i64)], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance - use get_full_task_output()
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    execution_steps_json: row.get(8)?,
                    log_sources_json: row.get(9)?,
                    summary: row.get(10)?,
                    ai_summary: row.get(11)?,
                    goal_achieved: row.get::<_, Option<i32>>(12)?.map(|v| v != 0),
                    remaining_work: row.get(13)?,
                    summary_generated_at: row.get(14)?,
                    transition_history_json: None,
                    workflow_type: None, // Not queried for performance
                    workspace_id: row.get(15)?,
                    triggered_by: row.get(16)?,
                    parent_task_run_id: None, // Not queried for performance
                    root_task_run_id: None,   // Not queried for performance
                    depth: 0,                 // Not queried for performance
                    bridge_id: None,          // Not queried for performance
                    result_data: None,        // Not queried for performance
                    is_reflection: false,     // Not queried for performance
                    reflection_source_task_run_id: None, // Not queried for performance
                    is_follow_up: false,      // Not queried for performance
                    follow_up_source_task_run_id: None, // Not queried for performance
                    is_fixer: false,          // Not queried for performance
                    fixer_source_task_run_id: None, // Not queried for performance
                    is_meta_optimizer: false,      // Not queried for performance
                    created_at: row.get(17)?,
                    updated_at: row.get(18)?,
                    completed_at: row.get(19)?,
                    task_type: row.get(20)?,
                    config_id: row.get(21)?,
                    workflow_name: row.get(22)?,
                    workflow_id: None,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get all running unified workflow task runs for resume on startup.
    /// Returns task runs where status = 'running' AND workflow_type = 'unified'.
    pub fn get_running_unified_workflows(&self, runner_port: Option<u16>) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json,
                       COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at,
                       workspace_id, triggered_by,
                       created_at, updated_at, completed_at,
                       task_type, config_id, workflow_name,
                       COALESCE(is_reflection, 0) as is_reflection,
                       COALESCE(is_follow_up, 0) as is_follow_up,
                       COALESCE(is_fixer, 0) as is_fixer
                FROM task_runs
                WHERE status = 'running' AND workflow_type = 'unified'
                      AND (?1 IS NULL OR runner_port IS NULL OR runner_port = ?1)
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map(params![runner_port.map(|p| p as i64)], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    execution_steps_json: row.get(8)?,
                    log_sources_json: row.get(9)?,
                    summary: row.get(10)?,
                    ai_summary: row.get(11)?,
                    goal_achieved: row.get::<_, Option<i32>>(12)?.map(|v| v != 0),
                    remaining_work: row.get(13)?,
                    summary_generated_at: row.get(14)?,
                    transition_history_json: None,
                    workflow_type: Some("unified".to_string()), // We know it's unified
                    workspace_id: row.get(15)?,
                    triggered_by: row.get(16)?,
                    parent_task_run_id: None, // Not queried for performance
                    root_task_run_id: None,   // Not queried for performance
                    depth: 0,                 // Not queried for performance
                    bridge_id: None,          // Not queried for performance
                    result_data: None,        // Not queried for performance
                    is_reflection: row.get::<_, i32>(23).unwrap_or(0) != 0,
                    reflection_source_task_run_id: None, // Not queried for performance
                    is_follow_up: row.get::<_, i32>(24).unwrap_or(0) != 0,
                    follow_up_source_task_run_id: None, // Not queried for performance
                    is_fixer: row.get::<_, i32>(25).unwrap_or(0) != 0,
                    fixer_source_task_run_id: None, // Not queried for performance
                    is_meta_optimizer: false,      // Not queried for performance
                    created_at: row.get(17)?,
                    updated_at: row.get(18)?,
                    completed_at: row.get(19)?,
                    task_type: row.get(20)?,
                    config_id: row.get(21)?,
                    workflow_name: row.get(22)?,
                    workflow_id: None,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get all running AI session task runs for resume on startup.
    /// Returns task runs where status = 'running' AND workflow_type = 'chat'.
    pub fn get_running_ai_sessions(&self, runner_port: Option<u16>) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json,
                       COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at,
                       workspace_id, triggered_by,
                       created_at, updated_at, completed_at,
                       task_type, config_id, workflow_name
                FROM task_runs
                WHERE status = 'running' AND workflow_type = 'chat'
                      AND (?1 IS NULL OR runner_port IS NULL OR runner_port = ?1)
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map(params![runner_port.map(|p| p as i64)], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    execution_steps_json: row.get(8)?,
                    log_sources_json: row.get(9)?,
                    summary: row.get(10)?,
                    ai_summary: row.get(11)?,
                    goal_achieved: row.get::<_, Option<i32>>(12)?.map(|v| v != 0),
                    remaining_work: row.get(13)?,
                    summary_generated_at: row.get(14)?,
                    transition_history_json: None,
                    workflow_type: Some("chat".to_string()),
                    workspace_id: row.get(15)?,
                    triggered_by: row.get(16)?,
                    parent_task_run_id: None,
                    root_task_run_id: None,
                    depth: 0,
                    bridge_id: None,
                    result_data: None,
                    is_reflection: false,
                    reflection_source_task_run_id: None,
                    is_follow_up: false,
                    follow_up_source_task_run_id: None,
                    is_fixer: false,
                    fixer_source_task_run_id: None,
                    is_meta_optimizer: false,
                    created_at: row.get(17)?,
                    updated_at: row.get(18)?,
                    completed_at: row.get(19)?,
                    task_type: row.get(20)?,
                    config_id: row.get(21)?,
                    workflow_name: row.get(22)?,
                    workflow_id: None,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get recent AI sessions (all statuses) for sidebar listing.
    /// Returns lightweight summaries ordered by most recently updated.
    pub fn get_ai_sessions(&self, limit: u32, runner_port: Option<u16>) -> Result<Vec<AiSessionSummary>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, status, updated_at, created_at
                FROM task_runs
                WHERE workflow_type = 'chat'
                      AND (?1 IS NULL OR runner_port IS NULL OR runner_port = ?1)
                ORDER BY updated_at DESC
                LIMIT ?2
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let sessions = stmt
            .query_map(params![runner_port.map(|p| p as i64), limit], |row| {
                Ok(AiSessionSummary {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    status: row.get(2)?,
                    updated_at: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(sessions)
    }

    /// Find an incomplete (running) task_run for a specific workflow by config_id.
    /// Returns the most recent running task_run for the given workflow, if any.
    /// Used to enable automatic resume when a workflow is re-run after a crash/restart.
    pub fn get_incomplete_task_run_for_workflow(
        &self,
        workflow_id: &str,
        runner_port: Option<u16>,
    ) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, rusqlite::Error> = conn.query_row(
            r#"
            SELECT id
            FROM task_runs
            WHERE config_id = ?1
              AND status = 'running'
              AND workflow_type = 'unified'
              AND (?2 IS NULL OR runner_port IS NULL OR runner_port = ?2)
            ORDER BY updated_at DESC
            LIMIT 1
            "#,
            params![workflow_id, runner_port.map(|p| p as i64)],
            |row| row.get(0),
        );

        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get incomplete task run: {}", e)),
        }
    }

    /// Mark an interrupted workflow as failed.
    /// Used when resume is disabled on startup.
    pub fn mark_interrupted_workflow_failed(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE task_runs SET status = 'failed', error_message = 'Workflow interrupted by runner restart (resume disabled)', completed_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )
        .map_err(|e| format!("Failed to mark interrupted workflow as failed: {}", e))?;

        Ok(())
    }

    /// Check if there's a running reflection workflow.
    /// Returns the task_run ID if one exists, None otherwise.
    /// Used to prevent duplicate reflection workflows from being created.
    pub fn has_running_reflection_workflow(&self) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, rusqlite::Error> = conn.query_row(
            r#"
            SELECT id
            FROM task_runs
            WHERE status = 'running'
              AND is_reflection = 1
              AND workflow_type = 'unified'
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            [],
            |row| row.get(0),
        );

        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!(
                "Failed to check for running reflection workflow: {}",
                e
            )),
        }
    }

    /// Check if there's a running error-fix workflow.
    /// Returns the task_run ID if one exists, None otherwise.
    /// Used to prevent duplicate error-fix workflows from being created.
    pub fn has_running_error_fix_workflow(&self) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, rusqlite::Error> = conn.query_row(
            r#"
            SELECT id
            FROM task_runs
            WHERE status = 'running'
              AND (task_name LIKE 'Fix%Error%' OR task_name LIKE '%error-fix%')
              AND workflow_type = 'unified'
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            [],
            |row| row.get(0),
        );

        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!(
                "Failed to check for running error-fix workflow: {}",
                e
            )),
        }
    }

    /// Get recent task runs (for display in UI).
    /// Note: output_log is empty for performance. Use get_full_task_output() to get output.
    pub fn get_recent_task_runs(&self, limit: u32, runner_port: Option<u16>) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                       config_id, workflow_name, COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at,
                       workspace_id, triggered_by,
                       created_at, updated_at, completed_at
                FROM task_runs
                WHERE (workflow_type IS NULL OR workflow_type != 'chat')
                      AND (?1 IS NULL OR runner_port IS NULL OR runner_port = ?1)
                ORDER BY updated_at DESC
                LIMIT ?2
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map(params![runner_port.map(|p| p as i64), limit], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance - use get_full_task_output()
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: None,
                    log_sources_json: None,
                    config_id: row.get(9)?,
                    workflow_name: row.get(10)?,
                    workflow_id: None,
                    summary: row.get(11)?,
                    ai_summary: row.get(12)?,
                    goal_achieved: row.get::<_, Option<i32>>(13)?.map(|v| v != 0),
                    remaining_work: row.get(14)?,
                    summary_generated_at: row.get(15)?,
                    transition_history_json: None,
                    workflow_type: None, // Not queried for performance
                    workspace_id: row.get(16)?,
                    triggered_by: row.get(17)?,
                    parent_task_run_id: None, // Not queried for performance
                    root_task_run_id: None,   // Not queried for performance
                    depth: 0,                 // Not queried for performance
                    bridge_id: None,          // Not queried for performance
                    result_data: None,        // Not queried for performance
                    is_reflection: false,     // Not queried for performance
                    reflection_source_task_run_id: None, // Not queried for performance
                    is_follow_up: false,      // Not queried for performance
                    follow_up_source_task_run_id: None, // Not queried for performance
                    is_fixer: false,          // Not queried for performance
                    fixer_source_task_run_id: None, // Not queried for performance
                    is_meta_optimizer: false,      // Not queried for performance
                    created_at: row.get(18)?,
                    updated_at: row.get(19)?,
                    completed_at: row.get(20)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get recent task runs with optional workflow_type filter.
    /// When workflow_type is provided, only returns task runs matching that type.
    pub fn get_recent_task_runs_filtered(
        &self,
        limit: u32,
        workflow_type: Option<&str>,
        runner_port: Option<u16>,
    ) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let port_param: Option<i64> = runner_port.map(|p| p as i64);

        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(wt) =
            workflow_type
        {
            (
                    r#"
                    SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                           config_id, workflow_name, COALESCE(summary, ai_summary) as summary, ai_summary,
                           goal_achieved, remaining_work, summary_generated_at,
                           workspace_id, triggered_by, workflow_type,
                           created_at, updated_at, completed_at
                    FROM task_runs
                    WHERE workflow_type = ?1
                          AND (?2 IS NULL OR runner_port IS NULL OR runner_port = ?2)
                    ORDER BY updated_at DESC
                    LIMIT ?3
                    "#.to_string(),
                    vec![
                        Box::new(wt.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(port_param),
                        Box::new(limit),
                    ],
                )
        } else {
            (
                    r#"
                    SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                           config_id, workflow_name, COALESCE(summary, ai_summary) as summary, ai_summary,
                           goal_achieved, remaining_work, summary_generated_at,
                           workspace_id, triggered_by, workflow_type,
                           created_at, updated_at, completed_at
                    FROM task_runs
                    WHERE (?1 IS NULL OR runner_port IS NULL OR runner_port = ?1)
                    ORDER BY updated_at DESC
                    LIMIT ?2
                    "#.to_string(),
                    vec![
                        Box::new(port_param) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(limit),
                    ],
                )
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let task_runs = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(),
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: None,
                    log_sources_json: None,
                    config_id: row.get(9)?,
                    workflow_name: row.get(10)?,
                    workflow_id: None,
                    summary: row.get(11)?,
                    ai_summary: row.get(12)?,
                    goal_achieved: row.get::<_, Option<i32>>(13)?.map(|v| v != 0),
                    remaining_work: row.get(14)?,
                    summary_generated_at: row.get(15)?,
                    transition_history_json: None,
                    workflow_type: row.get(18)?,
                    workspace_id: row.get(16)?,
                    triggered_by: row.get(17)?,
                    parent_task_run_id: None,
                    root_task_run_id: None,
                    depth: 0,
                    bridge_id: None,
                    result_data: None,
                    is_reflection: false, // Not queried for performance
                    reflection_source_task_run_id: None, // Not queried for performance
                    is_follow_up: false,  // Not queried for performance
                    follow_up_source_task_run_id: None, // Not queried for performance
                    is_fixer: false,      // Not queried for performance
                    fixer_source_task_run_id: None, // Not queried for performance
                    is_meta_optimizer: false,      // Not queried for performance
                    created_at: row.get(19)?,
                    updated_at: row.get(20)?,
                    completed_at: row.get(21)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get the last N characters of output for continuation context.
    pub fn get_task_output_tail(&self, id: &str, chars: usize) -> Result<String, String> {
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        let output = &task_run.output_log;
        if output.len() <= chars {
            Ok(output.clone())
        } else {
            let mut start = output.len() - chars;
            // Find the nearest char boundary to avoid panic on multi-byte UTF-8
            while start < output.len() && !output.is_char_boundary(start) {
                start += 1;
            }
            Ok(output[start..].to_string())
        }
    }

    /// Check if a task run should continue (not complete, not stopped, not at max sessions).
    ///
    /// Note: This does NOT check `auto_continue` because that setting only controls
    /// whether to resume on startup, not whether to continue after a step finishes.
    /// Workflows should always continue after steps are finished.
    pub fn should_continue_task(&self, id: &str) -> Result<bool, String> {
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        // Already complete or stopped
        if task_run.status != "running" {
            return Ok(false);
        }

        // Check max sessions limit
        if let Some(max) = task_run.max_sessions {
            if task_run.sessions_count >= max {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Delete a task run by ID.
    /// Note: CASCADE DELETE will automatically remove associated chunks.
    pub fn delete_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM task_runs WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete task run: {}", e))?;

        Ok(rows > 0)
    }

    /// Get the full output log by joining all chunks.
    /// Use this when you need the complete output (e.g., for display or export).
    pub fn get_full_task_output(&self, id: &str) -> Result<String, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                "SELECT content FROM task_run_output_chunks WHERE task_run_id = ? ORDER BY chunk_sequence",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let chunks: Vec<String> = stmt
            .query_map(params![id], |row| row.get(0))
            .map_err(|e| format!("Failed to query chunks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(chunks.join(""))
    }

    /// Get the auto-continue setting for a specific task run.
    pub fn get_task_auto_continue(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<i32> = conn.query_row(
            "SELECT auto_continue FROM task_runs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        );

        match result {
            Ok(value) => Ok(value != 0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(format!("Task run not found: {}", id)),
            Err(e) => Err(format!("Failed to get task auto_continue: {}", e)),
        }
    }

    /// Set the auto-continue setting for a specific task run.
    pub fn set_task_auto_continue(&self, id: &str, auto_continue: bool) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let rows = conn
            .execute(
                r#"
                UPDATE task_runs SET
                    auto_continue = ?1,
                    updated_at = ?2
                WHERE id = ?3
                "#,
                params![auto_continue as i32, now, id],
            )
            .map_err(|e| format!("Failed to set task auto_continue: {}", e))?;

        if rows == 0 {
            return Err(format!("Task run not found: {}", id));
        }

        Ok(())
    }

    /// Update the summary for a task run.
    /// Called after task completion to store the summary, goal achievement status, and remaining work.
    /// Note: Updates both 'summary' (new) and 'ai_summary' (legacy) columns for backward compatibility.
    pub fn update_task_summary(
        &self,
        id: &str,
        summary_text: &str,
        goal_achieved: bool,
        remaining_work: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let rows = conn
            .execute(
                r#"
                UPDATE task_runs SET
                    summary = ?1,
                    ai_summary = ?1,
                    goal_achieved = ?2,
                    remaining_work = ?3,
                    summary_generated_at = ?4,
                    updated_at = ?4
                WHERE id = ?5
                "#,
                params![summary_text, goal_achieved as i32, remaining_work, now, id],
            )
            .map_err(|e| format!("Failed to update task summary: {}", e))?;

        if rows == 0 {
            return Err(format!("Task run not found: {}", id));
        }

        info!("Updated summary for task run {}", id);
        Ok(())
    }

    /// Clear summary fields for a task run (used when reopening/continuing a run).
    pub fn clear_task_summary(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                summary = NULL,
                ai_summary = NULL,
                goal_achieved = NULL,
                remaining_work = NULL,
                summary_generated_at = NULL,
                updated_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to clear task summary: {}", e))?;

        Ok(())
    }

    /// Reopen a finished task run to add more iterations.
    /// Changes status back to "running", increments max_sessions, clears summary.
    pub fn reopen_task_run(&self, id: &str, additional_sessions: u32) -> Result<TaskRun, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // First, get the current task run to verify it exists and is finished
        drop(conn); // Release connection before calling get_task_run
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        // Verify the task is in a finished state
        if task_run.status == "running" {
            return Err("Task run is already running".to_string());
        }

        // Calculate new max_sessions
        let current_max = task_run.max_sessions.unwrap_or(task_run.sessions_count);
        let new_max = current_max + additional_sessions;

        // Reopen the task run
        let conn = self.get_conn()?;
        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'running',
                max_sessions = ?1,
                auto_continue = 1,
                ai_summary = NULL,
                goal_achieved = NULL,
                remaining_work = NULL,
                summary_generated_at = NULL,
                completed_at = NULL,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![new_max as i64, now, id],
        )
        .map_err(|e| format!("Failed to reopen task run: {}", e))?;

        info!(
            "Reopened task run {} with {} additional sessions (new max: {})",
            id, additional_sessions, new_max
        );

        // Return the updated task run
        drop(conn);
        self.get_task_run(id)?
            .ok_or_else(|| "Failed to retrieve updated task run".to_string())
    }

    // ========================================================================
    // Task Run Automation Operations (child records for automation metrics)
    // ========================================================================

    /// Create a new task run automation record.
    ///
    /// This creates a child record linked to a parent task_run.
    /// Use this when starting automation as part of a task.
    pub fn create_task_run_automation(
        &self,
        task_run_id: &str,
        workflow_name: Option<&str>,
        iteration_number: u32,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_automation (id, task_run_id, workflow_name, started_at, automation_status, iteration_number)
            VALUES (?1, ?2, ?3, ?4, 'running', ?5)
            "#,
            params![id, task_run_id, workflow_name, now, iteration_number as i64],
        )
        .map_err(|e| format!("Failed to create task run automation: {}", e))?;

        Ok(id)
    }

    /// Complete a task run automation record with success.
    pub fn complete_task_run_automation(
        &self,
        id: &str,
        actions_summary: Option<&str>,
        states_visited: Option<&str>,
        transitions_executed: Option<&str>,
        template_matches: Option<&str>,
        anomalies: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get start time to calculate duration
        let started_at: String = conn
            .query_row(
                "SELECT started_at FROM task_run_automation WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get automation start time: {}", e))?;

        // Calculate duration
        let duration_ms = if let Ok(start) = chrono::DateTime::parse_from_rfc3339(&started_at) {
            let end = Utc::now();
            (end.signed_duration_since(start.with_timezone(&Utc))).num_milliseconds()
        } else {
            0
        };

        conn.execute(
            r#"
            UPDATE task_run_automation SET
                automation_status = 'success',
                success = 1,
                ended_at = ?1,
                duration_ms = ?2,
                actions_summary = ?3,
                states_visited = ?4,
                transitions_executed = ?5,
                template_matches = ?6,
                anomalies = ?7
            WHERE id = ?8
            "#,
            params![
                now,
                duration_ms,
                actions_summary,
                states_visited,
                transitions_executed,
                template_matches,
                anomalies,
                id
            ],
        )
        .map_err(|e| format!("Failed to complete task run automation: {}", e))?;

        Ok(())
    }

    /// Fail a task run automation record.
    pub fn fail_task_run_automation(
        &self,
        id: &str,
        error_type: Option<&str>,
        error_message: &str,
        actions_summary: Option<&str>,
        states_visited: Option<&str>,
        transitions_executed: Option<&str>,
        template_matches: Option<&str>,
        anomalies: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get start time to calculate duration
        let started_at: String = conn
            .query_row(
                "SELECT started_at FROM task_run_automation WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get automation start time: {}", e))?;

        // Calculate duration
        let duration_ms = if let Ok(start) = chrono::DateTime::parse_from_rfc3339(&started_at) {
            let end = Utc::now();
            (end.signed_duration_since(start.with_timezone(&Utc))).num_milliseconds()
        } else {
            0
        };

        conn.execute(
            r#"
            UPDATE task_run_automation SET
                automation_status = 'failed',
                success = 0,
                ended_at = ?1,
                duration_ms = ?2,
                error_type = ?3,
                error_message = ?4,
                actions_summary = ?5,
                states_visited = ?6,
                transitions_executed = ?7,
                template_matches = ?8,
                anomalies = ?9
            WHERE id = ?10
            "#,
            params![
                now,
                duration_ms,
                error_type,
                error_message,
                actions_summary,
                states_visited,
                transitions_executed,
                template_matches,
                anomalies,
                id
            ],
        )
        .map_err(|e| format!("Failed to fail task run automation: {}", e))?;

        Ok(())
    }

    /// Get automation records for a task run.
    pub fn get_task_run_automations(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<TaskRunAutomation>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, workflow_name, started_at, ended_at, duration_ms,
                       automation_status, success, error_type, error_message,
                       actions_summary, states_visited, transitions_executed,
                       template_matches, anomalies, iteration_number
                FROM task_run_automation
                WHERE task_run_id = ?
                ORDER BY iteration_number ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let automations = stmt
            .query_map(params![task_run_id], |row| {
                Ok(TaskRunAutomation {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    workflow_name: row.get(2)?,
                    started_at: row.get(3)?,
                    ended_at: row.get(4)?,
                    duration_ms: row.get(5)?,
                    automation_status: row.get(6)?,
                    success: row.get::<_, Option<i32>>(7)?.map(|v| v != 0),
                    error_type: row.get(8)?,
                    error_message: row.get(9)?,
                    actions_summary: row.get(10)?,
                    states_visited: row.get(11)?,
                    transitions_executed: row.get(12)?,
                    template_matches: row.get(13)?,
                    anomalies: row.get(14)?,
                    iteration_number: row.get::<_, i64>(15)? as u32,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(automations)
    }

    /// Get a single automation record by its own ID.
    pub fn get_task_run_automation_by_id(
        &self,
        automation_id: &str,
    ) -> Result<Option<TaskRunAutomation>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, task_run_id, workflow_name, started_at, ended_at, duration_ms,
                   automation_status, success, error_type, error_message,
                   actions_summary, states_visited, transitions_executed,
                   template_matches, anomalies, iteration_number
            FROM task_run_automation
            WHERE id = ?
            "#,
            params![automation_id],
            |row| {
                Ok(TaskRunAutomation {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    workflow_name: row.get(2)?,
                    started_at: row.get(3)?,
                    ended_at: row.get(4)?,
                    duration_ms: row.get(5)?,
                    automation_status: row.get(6)?,
                    success: row.get::<_, Option<i32>>(7)?.map(|v| v != 0),
                    error_type: row.get(8)?,
                    error_message: row.get(9)?,
                    actions_summary: row.get(10)?,
                    states_visited: row.get(11)?,
                    transitions_executed: row.get(12)?,
                    template_matches: row.get(13)?,
                    anomalies: row.get(14)?,
                    iteration_number: row.get::<_, i64>(15)? as u32,
                })
            },
        );

        match result {
            Ok(automation) => Ok(Some(automation)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get automation record: {}", e)),
        }
    }

}
