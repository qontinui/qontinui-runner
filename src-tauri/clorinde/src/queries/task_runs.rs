// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreateTaskRunParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
    T10: crate::StringSql,
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
    T17: crate::StringSql,
    T18: crate::StringSql,
> {
    pub id: T1,
    pub task_name: T2,
    pub prompt: Option<T3>,
    pub task_type: Option<T4>,
    pub max_sessions: Option<i32>,
    pub auto_continue: bool,
    pub execution_steps_json: Option<T5>,
    pub log_sources_json: Option<T6>,
    pub config_id: Option<T7>,
    pub workflow_name: Option<T8>,
    pub workflow_id: Option<T9>,
    pub workflow_type: Option<T10>,
    pub parent_task_run_id: Option<T11>,
    pub root_task_run_id: Option<T12>,
    pub depth: i32,
    pub workspace_id: Option<T13>,
    pub triggered_by: Option<T14>,
    pub bridge_id: Option<T15>,
    pub is_reflection: Option<bool>,
    pub reflection_source_task_run_id: Option<T16>,
    pub is_follow_up: Option<bool>,
    pub follow_up_source_task_run_id: Option<T17>,
    pub is_fixer: Option<bool>,
    pub fixer_source_task_run_id: Option<T18>,
    pub is_meta_optimizer: bool,
    pub runner_port: Option<i32>,
}
#[derive(Clone, Copy, Debug)]
pub struct GetRecentTaskRunsParams {
    pub runner_port: Option<i32>,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct UpdateTaskRunStatusParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub status: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct FailTaskRunParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub error_message: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct StopTaskRunParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub reason: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct UpdateTaskSummaryParams<T1: crate::StringSql, T2: crate::StringSql, T3: crate::StringSql>
{
    pub summary: Option<T1>,
    pub goal_achieved: Option<bool>,
    pub remaining_work: Option<T2>,
    pub summary_generated_at: chrono::DateTime<chrono::FixedOffset>,
    pub id: T3,
}
#[derive(Debug)]
pub struct LeaseTaskForResumeParams<T1: crate::StringSql> {
    pub id: T1,
    pub expected_updated_at: chrono::DateTime<chrono::FixedOffset>,
}
#[derive(Debug)]
pub struct AppendTaskOutputParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub output: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct UpdateTaskNameParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub task_name: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct UpdateTaskResultDataParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub result_data: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct GetRecentTaskRunsFilteredParams<T1: crate::StringSql> {
    pub workflow_type: Option<T1>,
    pub runner_port: Option<i32>,
    pub max_results: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRun {
    pub id: String,
    pub task_name: String,
    pub prompt: String,
    pub task_type: String,
    pub status: String,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: String,
    pub auto_continue: bool,
    pub execution_steps_json: String,
    pub log_sources_json: String,
    pub config_id: String,
    pub workflow_name: String,
    pub workflow_id: String,
    pub summary: String,
    pub ai_summary: String,
    pub goal_achieved: bool,
    pub remaining_work: String,
    pub summary_generated_at: String,
    pub transition_history_json: String,
    pub workflow_type: String,
    pub workspace_id: String,
    pub triggered_by: String,
    pub parent_task_run_id: String,
    pub root_task_run_id: String,
    pub depth: i32,
    pub bridge_id: String,
    pub result_data: String,
    pub is_reflection: bool,
    pub reflection_source_task_run_id: String,
    pub is_follow_up: bool,
    pub follow_up_source_task_run_id: String,
    pub is_fixer: bool,
    pub fixer_source_task_run_id: String,
    pub is_meta_optimizer: bool,
    pub is_review: bool,
    pub blocks_parent: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTaskRunBorrowed<'a> {
    pub id: &'a str,
    pub task_name: &'a str,
    pub prompt: &'a str,
    pub task_type: &'a str,
    pub status: &'a str,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: &'a str,
    pub auto_continue: bool,
    pub execution_steps_json: &'a str,
    pub log_sources_json: &'a str,
    pub config_id: &'a str,
    pub workflow_name: &'a str,
    pub workflow_id: &'a str,
    pub summary: &'a str,
    pub ai_summary: &'a str,
    pub goal_achieved: bool,
    pub remaining_work: &'a str,
    pub summary_generated_at: &'a str,
    pub transition_history_json: &'a str,
    pub workflow_type: &'a str,
    pub workspace_id: &'a str,
    pub triggered_by: &'a str,
    pub parent_task_run_id: &'a str,
    pub root_task_run_id: &'a str,
    pub depth: i32,
    pub bridge_id: &'a str,
    pub result_data: &'a str,
    pub is_reflection: bool,
    pub reflection_source_task_run_id: &'a str,
    pub is_follow_up: bool,
    pub follow_up_source_task_run_id: &'a str,
    pub is_fixer: bool,
    pub fixer_source_task_run_id: &'a str,
    pub is_meta_optimizer: bool,
    pub is_review: bool,
    pub blocks_parent: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTaskRunBorrowed<'a>> for GetTaskRun {
    fn from(
        GetTaskRunBorrowed {
            id,
            task_name,
            prompt,
            task_type,
            status,
            sessions_count,
            max_sessions,
            error_message,
            auto_continue,
            execution_steps_json,
            log_sources_json,
            config_id,
            workflow_name,
            workflow_id,
            summary,
            ai_summary,
            goal_achieved,
            remaining_work,
            summary_generated_at,
            transition_history_json,
            workflow_type,
            workspace_id,
            triggered_by,
            parent_task_run_id,
            root_task_run_id,
            depth,
            bridge_id,
            result_data,
            is_reflection,
            reflection_source_task_run_id,
            is_follow_up,
            follow_up_source_task_run_id,
            is_fixer,
            fixer_source_task_run_id,
            is_meta_optimizer,
            is_review,
            blocks_parent,
            created_at,
            updated_at,
            completed_at,
        }: GetTaskRunBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_name: task_name.into(),
            prompt: prompt.into(),
            task_type: task_type.into(),
            status: status.into(),
            sessions_count,
            max_sessions,
            error_message: error_message.into(),
            auto_continue,
            execution_steps_json: execution_steps_json.into(),
            log_sources_json: log_sources_json.into(),
            config_id: config_id.into(),
            workflow_name: workflow_name.into(),
            workflow_id: workflow_id.into(),
            summary: summary.into(),
            ai_summary: ai_summary.into(),
            goal_achieved,
            remaining_work: remaining_work.into(),
            summary_generated_at: summary_generated_at.into(),
            transition_history_json: transition_history_json.into(),
            workflow_type: workflow_type.into(),
            workspace_id: workspace_id.into(),
            triggered_by: triggered_by.into(),
            parent_task_run_id: parent_task_run_id.into(),
            root_task_run_id: root_task_run_id.into(),
            depth,
            bridge_id: bridge_id.into(),
            result_data: result_data.into(),
            is_reflection,
            reflection_source_task_run_id: reflection_source_task_run_id.into(),
            is_follow_up,
            follow_up_source_task_run_id: follow_up_source_task_run_id.into(),
            is_fixer,
            fixer_source_task_run_id: fixer_source_task_run_id.into(),
            is_meta_optimizer,
            is_review,
            blocks_parent,
            created_at,
            updated_at,
            completed_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetRecentTaskRuns {
    pub id: String,
    pub task_name: String,
    pub prompt: String,
    pub task_type: String,
    pub status: String,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: String,
    pub auto_continue: bool,
    pub config_id: String,
    pub workflow_name: String,
    pub workflow_id: String,
    pub summary: String,
    pub ai_summary: String,
    pub goal_achieved: bool,
    pub remaining_work: String,
    pub summary_generated_at: String,
    pub workspace_id: String,
    pub triggered_by: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetRecentTaskRunsBorrowed<'a> {
    pub id: &'a str,
    pub task_name: &'a str,
    pub prompt: &'a str,
    pub task_type: &'a str,
    pub status: &'a str,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: &'a str,
    pub auto_continue: bool,
    pub config_id: &'a str,
    pub workflow_name: &'a str,
    pub workflow_id: &'a str,
    pub summary: &'a str,
    pub ai_summary: &'a str,
    pub goal_achieved: bool,
    pub remaining_work: &'a str,
    pub summary_generated_at: &'a str,
    pub workspace_id: &'a str,
    pub triggered_by: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetRecentTaskRunsBorrowed<'a>> for GetRecentTaskRuns {
    fn from(
        GetRecentTaskRunsBorrowed {
            id,
            task_name,
            prompt,
            task_type,
            status,
            sessions_count,
            max_sessions,
            error_message,
            auto_continue,
            config_id,
            workflow_name,
            workflow_id,
            summary,
            ai_summary,
            goal_achieved,
            remaining_work,
            summary_generated_at,
            workspace_id,
            triggered_by,
            created_at,
            updated_at,
            completed_at,
        }: GetRecentTaskRunsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_name: task_name.into(),
            prompt: prompt.into(),
            task_type: task_type.into(),
            status: status.into(),
            sessions_count,
            max_sessions,
            error_message: error_message.into(),
            auto_continue,
            config_id: config_id.into(),
            workflow_name: workflow_name.into(),
            workflow_id: workflow_id.into(),
            summary: summary.into(),
            ai_summary: ai_summary.into(),
            goal_achieved,
            remaining_work: remaining_work.into(),
            summary_generated_at: summary_generated_at.into(),
            workspace_id: workspace_id.into(),
            triggered_by: triggered_by.into(),
            created_at,
            updated_at,
            completed_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetRunningTaskRuns {
    pub id: String,
    pub task_name: String,
    pub prompt: String,
    pub task_type: String,
    pub status: String,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: String,
    pub auto_continue: bool,
    pub config_id: String,
    pub workflow_name: String,
    pub workflow_id: String,
    pub workflow_type: String,
    pub workspace_id: String,
    pub triggered_by: String,
    pub parent_task_run_id: String,
    pub root_task_run_id: String,
    pub depth: i32,
    pub bridge_id: String,
    pub is_reflection: bool,
    pub reflection_source_task_run_id: String,
    pub is_follow_up: bool,
    pub follow_up_source_task_run_id: String,
    pub is_fixer: bool,
    pub fixer_source_task_run_id: String,
    pub is_meta_optimizer: bool,
    pub is_review: bool,
    pub blocks_parent: bool,
    pub runner_port: i32,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetRunningTaskRunsBorrowed<'a> {
    pub id: &'a str,
    pub task_name: &'a str,
    pub prompt: &'a str,
    pub task_type: &'a str,
    pub status: &'a str,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: &'a str,
    pub auto_continue: bool,
    pub config_id: &'a str,
    pub workflow_name: &'a str,
    pub workflow_id: &'a str,
    pub workflow_type: &'a str,
    pub workspace_id: &'a str,
    pub triggered_by: &'a str,
    pub parent_task_run_id: &'a str,
    pub root_task_run_id: &'a str,
    pub depth: i32,
    pub bridge_id: &'a str,
    pub is_reflection: bool,
    pub reflection_source_task_run_id: &'a str,
    pub is_follow_up: bool,
    pub follow_up_source_task_run_id: &'a str,
    pub is_fixer: bool,
    pub fixer_source_task_run_id: &'a str,
    pub is_meta_optimizer: bool,
    pub is_review: bool,
    pub blocks_parent: bool,
    pub runner_port: i32,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetRunningTaskRunsBorrowed<'a>> for GetRunningTaskRuns {
    fn from(
        GetRunningTaskRunsBorrowed {
            id,
            task_name,
            prompt,
            task_type,
            status,
            sessions_count,
            max_sessions,
            error_message,
            auto_continue,
            config_id,
            workflow_name,
            workflow_id,
            workflow_type,
            workspace_id,
            triggered_by,
            parent_task_run_id,
            root_task_run_id,
            depth,
            bridge_id,
            is_reflection,
            reflection_source_task_run_id,
            is_follow_up,
            follow_up_source_task_run_id,
            is_fixer,
            fixer_source_task_run_id,
            is_meta_optimizer,
            is_review,
            blocks_parent,
            runner_port,
            created_at,
            updated_at,
            completed_at,
        }: GetRunningTaskRunsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_name: task_name.into(),
            prompt: prompt.into(),
            task_type: task_type.into(),
            status: status.into(),
            sessions_count,
            max_sessions,
            error_message: error_message.into(),
            auto_continue,
            config_id: config_id.into(),
            workflow_name: workflow_name.into(),
            workflow_id: workflow_id.into(),
            workflow_type: workflow_type.into(),
            workspace_id: workspace_id.into(),
            triggered_by: triggered_by.into(),
            parent_task_run_id: parent_task_run_id.into(),
            root_task_run_id: root_task_run_id.into(),
            depth,
            bridge_id: bridge_id.into(),
            is_reflection,
            reflection_source_task_run_id: reflection_source_task_run_id.into(),
            is_follow_up,
            follow_up_source_task_run_id: follow_up_source_task_run_id.into(),
            is_fixer,
            fixer_source_task_run_id: fixer_source_task_run_id.into(),
            is_meta_optimizer,
            is_review,
            blocks_parent,
            runner_port,
            created_at,
            updated_at,
            completed_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetResumableTaskRunsForRunner {
    pub id: String,
    pub task_name: String,
    pub prompt: String,
    pub task_type: String,
    pub status: String,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: String,
    pub auto_continue: bool,
    pub config_id: String,
    pub workflow_name: String,
    pub workflow_id: String,
    pub workflow_type: String,
    pub workspace_id: String,
    pub triggered_by: String,
    pub parent_task_run_id: String,
    pub root_task_run_id: String,
    pub depth: i32,
    pub bridge_id: String,
    pub is_reflection: bool,
    pub reflection_source_task_run_id: String,
    pub is_follow_up: bool,
    pub follow_up_source_task_run_id: String,
    pub is_fixer: bool,
    pub fixer_source_task_run_id: String,
    pub is_meta_optimizer: bool,
    pub is_review: bool,
    pub blocks_parent: bool,
    pub runner_port: i32,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetResumableTaskRunsForRunnerBorrowed<'a> {
    pub id: &'a str,
    pub task_name: &'a str,
    pub prompt: &'a str,
    pub task_type: &'a str,
    pub status: &'a str,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: &'a str,
    pub auto_continue: bool,
    pub config_id: &'a str,
    pub workflow_name: &'a str,
    pub workflow_id: &'a str,
    pub workflow_type: &'a str,
    pub workspace_id: &'a str,
    pub triggered_by: &'a str,
    pub parent_task_run_id: &'a str,
    pub root_task_run_id: &'a str,
    pub depth: i32,
    pub bridge_id: &'a str,
    pub is_reflection: bool,
    pub reflection_source_task_run_id: &'a str,
    pub is_follow_up: bool,
    pub follow_up_source_task_run_id: &'a str,
    pub is_fixer: bool,
    pub fixer_source_task_run_id: &'a str,
    pub is_meta_optimizer: bool,
    pub is_review: bool,
    pub blocks_parent: bool,
    pub runner_port: i32,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetResumableTaskRunsForRunnerBorrowed<'a>> for GetResumableTaskRunsForRunner {
    fn from(
        GetResumableTaskRunsForRunnerBorrowed {
            id,
            task_name,
            prompt,
            task_type,
            status,
            sessions_count,
            max_sessions,
            error_message,
            auto_continue,
            config_id,
            workflow_name,
            workflow_id,
            workflow_type,
            workspace_id,
            triggered_by,
            parent_task_run_id,
            root_task_run_id,
            depth,
            bridge_id,
            is_reflection,
            reflection_source_task_run_id,
            is_follow_up,
            follow_up_source_task_run_id,
            is_fixer,
            fixer_source_task_run_id,
            is_meta_optimizer,
            is_review,
            blocks_parent,
            runner_port,
            created_at,
            updated_at,
            completed_at,
        }: GetResumableTaskRunsForRunnerBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_name: task_name.into(),
            prompt: prompt.into(),
            task_type: task_type.into(),
            status: status.into(),
            sessions_count,
            max_sessions,
            error_message: error_message.into(),
            auto_continue,
            config_id: config_id.into(),
            workflow_name: workflow_name.into(),
            workflow_id: workflow_id.into(),
            workflow_type: workflow_type.into(),
            workspace_id: workspace_id.into(),
            triggered_by: triggered_by.into(),
            parent_task_run_id: parent_task_run_id.into(),
            root_task_run_id: root_task_run_id.into(),
            depth,
            bridge_id: bridge_id.into(),
            is_reflection,
            reflection_source_task_run_id: reflection_source_task_run_id.into(),
            is_follow_up,
            follow_up_source_task_run_id: follow_up_source_task_run_id.into(),
            is_fixer,
            fixer_source_task_run_id: fixer_source_task_run_id.into(),
            is_meta_optimizer,
            is_review,
            blocks_parent,
            runner_port,
            created_at,
            updated_at,
            completed_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetRecentTaskRunsFiltered {
    pub id: String,
    pub task_name: String,
    pub prompt: String,
    pub task_type: String,
    pub status: String,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: String,
    pub auto_continue: bool,
    pub config_id: String,
    pub workflow_name: String,
    pub workflow_id: String,
    pub summary: String,
    pub ai_summary: String,
    pub goal_achieved: bool,
    pub remaining_work: String,
    pub summary_generated_at: String,
    pub workspace_id: String,
    pub triggered_by: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetRecentTaskRunsFilteredBorrowed<'a> {
    pub id: &'a str,
    pub task_name: &'a str,
    pub prompt: &'a str,
    pub task_type: &'a str,
    pub status: &'a str,
    pub sessions_count: i32,
    pub max_sessions: i32,
    pub error_message: &'a str,
    pub auto_continue: bool,
    pub config_id: &'a str,
    pub workflow_name: &'a str,
    pub workflow_id: &'a str,
    pub summary: &'a str,
    pub ai_summary: &'a str,
    pub goal_achieved: bool,
    pub remaining_work: &'a str,
    pub summary_generated_at: &'a str,
    pub workspace_id: &'a str,
    pub triggered_by: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetRecentTaskRunsFilteredBorrowed<'a>> for GetRecentTaskRunsFiltered {
    fn from(
        GetRecentTaskRunsFilteredBorrowed {
            id,
            task_name,
            prompt,
            task_type,
            status,
            sessions_count,
            max_sessions,
            error_message,
            auto_continue,
            config_id,
            workflow_name,
            workflow_id,
            summary,
            ai_summary,
            goal_achieved,
            remaining_work,
            summary_generated_at,
            workspace_id,
            triggered_by,
            created_at,
            updated_at,
            completed_at,
        }: GetRecentTaskRunsFilteredBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_name: task_name.into(),
            prompt: prompt.into(),
            task_type: task_type.into(),
            status: status.into(),
            sessions_count,
            max_sessions,
            error_message: error_message.into(),
            auto_continue,
            config_id: config_id.into(),
            workflow_name: workflow_name.into(),
            workflow_id: workflow_id.into(),
            summary: summary.into(),
            ai_summary: ai_summary.into(),
            goal_achieved,
            remaining_work: remaining_work.into(),
            summary_generated_at: summary_generated_at.into(),
            workspace_id: workspace_id.into(),
            triggered_by: triggered_by.into(),
            created_at,
            updated_at,
            completed_at,
        }
    }
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct StringQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<&str, tokio_postgres::Error>,
    mapper: fn(&str) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> StringQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(self, mapper: fn(&str) -> R) -> StringQuery<'c, 'a, 's, C, R, N> {
        StringQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetTaskRunQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetTaskRunBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunBorrowed) -> R,
    ) -> GetTaskRunQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetRecentTaskRunsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetRecentTaskRunsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetRecentTaskRunsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetRecentTaskRunsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetRecentTaskRunsBorrowed) -> R,
    ) -> GetRecentTaskRunsQuery<'c, 'a, 's, C, R, N> {
        GetRecentTaskRunsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetRunningTaskRunsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetRunningTaskRunsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetRunningTaskRunsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetRunningTaskRunsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetRunningTaskRunsBorrowed) -> R,
    ) -> GetRunningTaskRunsQuery<'c, 'a, 's, C, R, N> {
        GetRunningTaskRunsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetResumableTaskRunsForRunnerQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetResumableTaskRunsForRunnerBorrowed, tokio_postgres::Error>,
    mapper: fn(GetResumableTaskRunsForRunnerBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetResumableTaskRunsForRunnerQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetResumableTaskRunsForRunnerBorrowed) -> R,
    ) -> GetResumableTaskRunsForRunnerQuery<'c, 'a, 's, C, R, N> {
        GetResumableTaskRunsForRunnerQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetRecentTaskRunsFilteredQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetRecentTaskRunsFilteredBorrowed, tokio_postgres::Error>,
    mapper: fn(GetRecentTaskRunsFilteredBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetRecentTaskRunsFilteredQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetRecentTaskRunsFilteredBorrowed) -> R,
    ) -> GetRecentTaskRunsFilteredQuery<'c, 'a, 's, C, R, N> {
        GetRecentTaskRunsFilteredQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row =
            crate::client::async_::one(self.client, self.query, &self.params, self.cached).await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row =
            crate::client::async_::opt(self.client, self.query, &self.params, self.cached).await?;
        Ok(opt_row
            .map(|row| {
                let extracted = (self.extractor)(&row)?;
                Ok((self.mapper)(extracted))
            })
            .transpose()?)
    }
    pub async fn iter(
        self,
    ) -> Result<
        impl futures::Stream<Item = Result<T, tokio_postgres::Error>> + 'c,
        tokio_postgres::Error,
    > {
        let stream = crate::client::async_::raw(
            self.client,
            self.query,
            crate::slice_iter(&self.params),
            self.cached,
        )
        .await?;
        let mapped = stream
            .map(move |res| {
                res.and_then(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct CreateTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_task_run() -> CreateTaskRunStmt {
    CreateTaskRunStmt(
        "INSERT INTO task_runs ( id, task_name, prompt, task_type, status, sessions_count, max_sessions, auto_continue, output_log, execution_steps_json, log_sources_json, config_id, workflow_name, workflow_id, workflow_type, parent_task_run_id, root_task_run_id, depth, workspace_id, triggered_by, bridge_id, is_reflection, reflection_source_task_run_id, is_follow_up, follow_up_source_task_run_id, is_fixer, fixer_source_task_run_id, is_meta_optimizer, runner_port ) VALUES ( $1, $2, $3, $4, 'running', 0, $5, $6, '', $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26 ) RETURNING id",
        None,
    )
}
impl CreateTaskRunStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<
        'c,
        'a,
        's,
        C: GenericClient,
        T1: crate::StringSql,
        T2: crate::StringSql,
        T3: crate::StringSql,
        T4: crate::StringSql,
        T5: crate::StringSql,
        T6: crate::StringSql,
        T7: crate::StringSql,
        T8: crate::StringSql,
        T9: crate::StringSql,
        T10: crate::StringSql,
        T11: crate::StringSql,
        T12: crate::StringSql,
        T13: crate::StringSql,
        T14: crate::StringSql,
        T15: crate::StringSql,
        T16: crate::StringSql,
        T17: crate::StringSql,
        T18: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        task_name: &'a T2,
        prompt: &'a Option<T3>,
        task_type: &'a Option<T4>,
        max_sessions: &'a Option<i32>,
        auto_continue: &'a bool,
        execution_steps_json: &'a Option<T5>,
        log_sources_json: &'a Option<T6>,
        config_id: &'a Option<T7>,
        workflow_name: &'a Option<T8>,
        workflow_id: &'a Option<T9>,
        workflow_type: &'a Option<T10>,
        parent_task_run_id: &'a Option<T11>,
        root_task_run_id: &'a Option<T12>,
        depth: &'a i32,
        workspace_id: &'a Option<T13>,
        triggered_by: &'a Option<T14>,
        bridge_id: &'a Option<T15>,
        is_reflection: &'a Option<bool>,
        reflection_source_task_run_id: &'a Option<T16>,
        is_follow_up: &'a Option<bool>,
        follow_up_source_task_run_id: &'a Option<T17>,
        is_fixer: &'a Option<bool>,
        fixer_source_task_run_id: &'a Option<T18>,
        is_meta_optimizer: &'a bool,
        runner_port: &'a Option<i32>,
    ) -> StringQuery<'c, 'a, 's, C, String, 26> {
        StringQuery {
            client,
            params: [
                id,
                task_name,
                prompt,
                task_type,
                max_sessions,
                auto_continue,
                execution_steps_json,
                log_sources_json,
                config_id,
                workflow_name,
                workflow_id,
                workflow_type,
                parent_task_run_id,
                root_task_run_id,
                depth,
                workspace_id,
                triggered_by,
                bridge_id,
                is_reflection,
                reflection_source_task_run_id,
                is_follow_up,
                follow_up_source_task_run_id,
                is_fixer,
                fixer_source_task_run_id,
                is_meta_optimizer,
                runner_port,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
    T10: crate::StringSql,
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
    T17: crate::StringSql,
    T18: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        CreateTaskRunParams<
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
            T14,
            T15,
            T16,
            T17,
            T18,
        >,
        StringQuery<'c, 'a, 's, C, String, 26>,
        C,
    > for CreateTaskRunStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateTaskRunParams<
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
            T14,
            T15,
            T16,
            T17,
            T18,
        >,
    ) -> StringQuery<'c, 'a, 's, C, String, 26> {
        self.bind(
            client,
            &params.id,
            &params.task_name,
            &params.prompt,
            &params.task_type,
            &params.max_sessions,
            &params.auto_continue,
            &params.execution_steps_json,
            &params.log_sources_json,
            &params.config_id,
            &params.workflow_name,
            &params.workflow_id,
            &params.workflow_type,
            &params.parent_task_run_id,
            &params.root_task_run_id,
            &params.depth,
            &params.workspace_id,
            &params.triggered_by,
            &params.bridge_id,
            &params.is_reflection,
            &params.reflection_source_task_run_id,
            &params.is_follow_up,
            &params.follow_up_source_task_run_id,
            &params.is_fixer,
            &params.fixer_source_task_run_id,
            &params.is_meta_optimizer,
            &params.runner_port,
        )
    }
}
pub struct GetTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run() -> GetTaskRunStmt {
    GetTaskRunStmt(
        "SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type, COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count, COALESCE(max_sessions, 0) as max_sessions, COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue, COALESCE(execution_steps_json, '') as execution_steps_json, COALESCE(log_sources_json, '') as log_sources_json, COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id, COALESCE(summary, ai_summary, '') as summary, COALESCE(ai_summary, '') as ai_summary, COALESCE(goal_achieved, false) as goal_achieved, COALESCE(remaining_work, '') as remaining_work, COALESCE(summary_generated_at::TEXT, '') as summary_generated_at, COALESCE(transition_history_json, '') as transition_history_json, COALESCE(workflow_type, 'task') as workflow_type, COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by, COALESCE(parent_task_run_id, '') as parent_task_run_id, COALESCE(root_task_run_id, '') as root_task_run_id, COALESCE(depth, 0) as depth, COALESCE(bridge_id, '') as bridge_id, COALESCE(result_data, '') as result_data, COALESCE(is_reflection, false) as is_reflection, COALESCE(reflection_source_task_run_id, '') as reflection_source_task_run_id, COALESCE(is_follow_up, false) as is_follow_up, COALESCE(follow_up_source_task_run_id, '') as follow_up_source_task_run_id, COALESCE(is_fixer, false) as is_fixer, COALESCE(fixer_source_task_run_id, '') as fixer_source_task_run_id, COALESCE(is_meta_optimizer, false) as is_meta_optimizer, COALESCE(is_review, false) as is_review, COALESCE(blocks_parent, false) as blocks_parent, COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at FROM task_runs WHERE id = $1",
        None,
    )
}
impl GetTaskRunStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> GetTaskRunQuery<'c, 'a, 's, C, GetTaskRun, 1> {
        GetTaskRunQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<GetTaskRunBorrowed, tokio_postgres::Error> {
                    Ok(GetTaskRunBorrowed {
                        id: row.try_get(0)?,
                        task_name: row.try_get(1)?,
                        prompt: row.try_get(2)?,
                        task_type: row.try_get(3)?,
                        status: row.try_get(4)?,
                        sessions_count: row.try_get(5)?,
                        max_sessions: row.try_get(6)?,
                        error_message: row.try_get(7)?,
                        auto_continue: row.try_get(8)?,
                        execution_steps_json: row.try_get(9)?,
                        log_sources_json: row.try_get(10)?,
                        config_id: row.try_get(11)?,
                        workflow_name: row.try_get(12)?,
                        workflow_id: row.try_get(13)?,
                        summary: row.try_get(14)?,
                        ai_summary: row.try_get(15)?,
                        goal_achieved: row.try_get(16)?,
                        remaining_work: row.try_get(17)?,
                        summary_generated_at: row.try_get(18)?,
                        transition_history_json: row.try_get(19)?,
                        workflow_type: row.try_get(20)?,
                        workspace_id: row.try_get(21)?,
                        triggered_by: row.try_get(22)?,
                        parent_task_run_id: row.try_get(23)?,
                        root_task_run_id: row.try_get(24)?,
                        depth: row.try_get(25)?,
                        bridge_id: row.try_get(26)?,
                        result_data: row.try_get(27)?,
                        is_reflection: row.try_get(28)?,
                        reflection_source_task_run_id: row.try_get(29)?,
                        is_follow_up: row.try_get(30)?,
                        follow_up_source_task_run_id: row.try_get(31)?,
                        is_fixer: row.try_get(32)?,
                        fixer_source_task_run_id: row.try_get(33)?,
                        is_meta_optimizer: row.try_get(34)?,
                        is_review: row.try_get(35)?,
                        blocks_parent: row.try_get(36)?,
                        created_at: row.try_get(37)?,
                        updated_at: row.try_get(38)?,
                        completed_at: row.try_get(39)?,
                    })
                },
            mapper: |it| GetTaskRun::from(it),
        }
    }
}
pub struct GetRecentTaskRunsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_recent_task_runs() -> GetRecentTaskRunsStmt {
    GetRecentTaskRunsStmt(
        "SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type, COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count, COALESCE(max_sessions, 0) as max_sessions, COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue, COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id, COALESCE(summary, ai_summary, '') as summary, COALESCE(ai_summary, '') as ai_summary, COALESCE(goal_achieved, false) as goal_achieved, COALESCE(remaining_work, '') as remaining_work, COALESCE(summary_generated_at::TEXT, '') as summary_generated_at, COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by, COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at FROM task_runs WHERE (workflow_type IS NULL OR workflow_type != 'chat') AND ($1::integer IS NULL OR runner_port IS NULL OR runner_port = $1) ORDER BY updated_at DESC LIMIT $2",
        None,
    )
}
impl GetRecentTaskRunsStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient>(
        &'s self,
        client: &'c C,
        runner_port: &'a Option<i32>,
        max_results: &'a i64,
    ) -> GetRecentTaskRunsQuery<'c, 'a, 's, C, GetRecentTaskRuns, 2> {
        GetRecentTaskRunsQuery {
            client,
            params: [runner_port, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetRecentTaskRunsBorrowed, tokio_postgres::Error> {
                Ok(GetRecentTaskRunsBorrowed {
                    id: row.try_get(0)?,
                    task_name: row.try_get(1)?,
                    prompt: row.try_get(2)?,
                    task_type: row.try_get(3)?,
                    status: row.try_get(4)?,
                    sessions_count: row.try_get(5)?,
                    max_sessions: row.try_get(6)?,
                    error_message: row.try_get(7)?,
                    auto_continue: row.try_get(8)?,
                    config_id: row.try_get(9)?,
                    workflow_name: row.try_get(10)?,
                    workflow_id: row.try_get(11)?,
                    summary: row.try_get(12)?,
                    ai_summary: row.try_get(13)?,
                    goal_achieved: row.try_get(14)?,
                    remaining_work: row.try_get(15)?,
                    summary_generated_at: row.try_get(16)?,
                    workspace_id: row.try_get(17)?,
                    triggered_by: row.try_get(18)?,
                    created_at: row.try_get(19)?,
                    updated_at: row.try_get(20)?,
                    completed_at: row.try_get(21)?,
                })
            },
            mapper: |it| GetRecentTaskRuns::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetRecentTaskRunsParams,
        GetRecentTaskRunsQuery<'c, 'a, 's, C, GetRecentTaskRuns, 2>,
        C,
    > for GetRecentTaskRunsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetRecentTaskRunsParams,
    ) -> GetRecentTaskRunsQuery<'c, 'a, 's, C, GetRecentTaskRuns, 2> {
        self.bind(client, &params.runner_port, &params.max_results)
    }
}
pub struct UpdateTaskRunStatusStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_task_run_status() -> UpdateTaskRunStatusStmt {
    UpdateTaskRunStatusStmt(
        "UPDATE task_runs SET status = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl UpdateTaskRunStatusStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>(
        &'s self,
        client: &'c C,
        status: &'a T1,
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [status, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateTaskRunStatusParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for UpdateTaskRunStatusStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateTaskRunStatusParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.status, &params.id)
    }
}
pub struct CompleteTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn complete_task_run() -> CompleteTaskRunStmt {
    CompleteTaskRunStmt(
        "UPDATE task_runs SET status = 'complete', updated_at = NOW(), completed_at = NOW() WHERE id = $1 RETURNING id",
        None,
    )
}
impl CompleteTaskRunStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct FailTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn fail_task_run() -> FailTaskRunStmt {
    FailTaskRunStmt(
        "UPDATE task_runs SET status = 'failed', error_message = $1, updated_at = NOW(), completed_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl FailTaskRunStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>(
        &'s self,
        client: &'c C,
        error_message: &'a T1,
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [error_message, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        FailTaskRunParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for FailTaskRunStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a FailTaskRunParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.error_message, &params.id)
    }
}
pub struct StopTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn stop_task_run() -> StopTaskRunStmt {
    StopTaskRunStmt(
        "UPDATE task_runs SET status = 'stopped', error_message = $1, updated_at = NOW(), completed_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl StopTaskRunStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>(
        &'s self,
        client: &'c C,
        reason: &'a T1,
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [reason, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        StopTaskRunParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for StopTaskRunStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a StopTaskRunParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.reason, &params.id)
    }
}
pub struct DeleteTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_task_run() -> DeleteTaskRunStmt {
    DeleteTaskRunStmt("DELETE FROM task_runs WHERE id = $1 RETURNING id", None)
}
impl DeleteTaskRunStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct UpdateTaskSummaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_task_summary() -> UpdateTaskSummaryStmt {
    UpdateTaskSummaryStmt(
        "UPDATE task_runs SET summary = $1, ai_summary = $1, goal_achieved = $2, remaining_work = $3, summary_generated_at = $4, updated_at = NOW() WHERE id = $5 RETURNING id",
        None,
    )
}
impl UpdateTaskSummaryStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<
        'c,
        'a,
        's,
        C: GenericClient,
        T1: crate::StringSql,
        T2: crate::StringSql,
        T3: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        summary: &'a Option<T1>,
        goal_achieved: &'a Option<bool>,
        remaining_work: &'a Option<T2>,
        summary_generated_at: &'a chrono::DateTime<chrono::FixedOffset>,
        id: &'a T3,
    ) -> StringQuery<'c, 'a, 's, C, String, 5> {
        StringQuery {
            client,
            params: [
                summary,
                goal_achieved,
                remaining_work,
                summary_generated_at,
                id,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql, T3: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateTaskSummaryParams<T1, T2, T3>,
        StringQuery<'c, 'a, 's, C, String, 5>,
        C,
    > for UpdateTaskSummaryStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateTaskSummaryParams<T1, T2, T3>,
    ) -> StringQuery<'c, 'a, 's, C, String, 5> {
        self.bind(
            client,
            &params.summary,
            &params.goal_achieved,
            &params.remaining_work,
            &params.summary_generated_at,
            &params.id,
        )
    }
}
pub struct GetRunningTaskRunsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_running_task_runs() -> GetRunningTaskRunsStmt {
    GetRunningTaskRunsStmt(
        "SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type, COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count, COALESCE(max_sessions, 0) as max_sessions, COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue, COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id, COALESCE(workflow_type, 'task') as workflow_type, COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by, COALESCE(parent_task_run_id, '') as parent_task_run_id, COALESCE(root_task_run_id, '') as root_task_run_id, COALESCE(depth, 0) as depth, COALESCE(bridge_id, '') as bridge_id, COALESCE(is_reflection, false) as is_reflection, COALESCE(reflection_source_task_run_id, '') as reflection_source_task_run_id, COALESCE(is_follow_up, false) as is_follow_up, COALESCE(follow_up_source_task_run_id, '') as follow_up_source_task_run_id, COALESCE(is_fixer, false) as is_fixer, COALESCE(fixer_source_task_run_id, '') as fixer_source_task_run_id, COALESCE(is_meta_optimizer, false) as is_meta_optimizer, COALESCE(is_review, false) as is_review, COALESCE(blocks_parent, false) as blocks_parent, COALESCE(runner_port, 0) as runner_port, COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at FROM task_runs WHERE status = 'running' AND ($1::integer IS NULL OR runner_port IS NULL OR runner_port = $1) ORDER BY created_at DESC",
        None,
    )
}
impl GetRunningTaskRunsStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient>(
        &'s self,
        client: &'c C,
        runner_port: &'a Option<i32>,
    ) -> GetRunningTaskRunsQuery<'c, 'a, 's, C, GetRunningTaskRuns, 1> {
        GetRunningTaskRunsQuery {
            client,
            params: [runner_port],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetRunningTaskRunsBorrowed, tokio_postgres::Error> {
                Ok(GetRunningTaskRunsBorrowed {
                    id: row.try_get(0)?,
                    task_name: row.try_get(1)?,
                    prompt: row.try_get(2)?,
                    task_type: row.try_get(3)?,
                    status: row.try_get(4)?,
                    sessions_count: row.try_get(5)?,
                    max_sessions: row.try_get(6)?,
                    error_message: row.try_get(7)?,
                    auto_continue: row.try_get(8)?,
                    config_id: row.try_get(9)?,
                    workflow_name: row.try_get(10)?,
                    workflow_id: row.try_get(11)?,
                    workflow_type: row.try_get(12)?,
                    workspace_id: row.try_get(13)?,
                    triggered_by: row.try_get(14)?,
                    parent_task_run_id: row.try_get(15)?,
                    root_task_run_id: row.try_get(16)?,
                    depth: row.try_get(17)?,
                    bridge_id: row.try_get(18)?,
                    is_reflection: row.try_get(19)?,
                    reflection_source_task_run_id: row.try_get(20)?,
                    is_follow_up: row.try_get(21)?,
                    follow_up_source_task_run_id: row.try_get(22)?,
                    is_fixer: row.try_get(23)?,
                    fixer_source_task_run_id: row.try_get(24)?,
                    is_meta_optimizer: row.try_get(25)?,
                    is_review: row.try_get(26)?,
                    blocks_parent: row.try_get(27)?,
                    runner_port: row.try_get(28)?,
                    created_at: row.try_get(29)?,
                    updated_at: row.try_get(30)?,
                    completed_at: row.try_get(31)?,
                })
            },
            mapper: |it| GetRunningTaskRuns::from(it),
        }
    }
}
pub struct GetResumableTaskRunsForRunnerStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_resumable_task_runs_for_runner() -> GetResumableTaskRunsForRunnerStmt {
    GetResumableTaskRunsForRunnerStmt(
        "SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type, COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count, COALESCE(max_sessions, 0) as max_sessions, COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue, COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id, COALESCE(workflow_type, 'task') as workflow_type, COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by, COALESCE(parent_task_run_id, '') as parent_task_run_id, COALESCE(root_task_run_id, '') as root_task_run_id, COALESCE(depth, 0) as depth, COALESCE(bridge_id, '') as bridge_id, COALESCE(is_reflection, false) as is_reflection, COALESCE(reflection_source_task_run_id, '') as reflection_source_task_run_id, COALESCE(is_follow_up, false) as is_follow_up, COALESCE(follow_up_source_task_run_id, '') as follow_up_source_task_run_id, COALESCE(is_fixer, false) as is_fixer, COALESCE(fixer_source_task_run_id, '') as fixer_source_task_run_id, COALESCE(is_meta_optimizer, false) as is_meta_optimizer, COALESCE(is_review, false) as is_review, COALESCE(blocks_parent, false) as blocks_parent, COALESCE(runner_port, 0) as runner_port, COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at FROM task_runs WHERE status = 'running' AND runner_port = $1 ORDER BY created_at DESC",
        None,
    )
}
impl GetResumableTaskRunsForRunnerStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient>(
        &'s self,
        client: &'c C,
        runner_port: &'a i32,
    ) -> GetResumableTaskRunsForRunnerQuery<'c, 'a, 's, C, GetResumableTaskRunsForRunner, 1> {
        GetResumableTaskRunsForRunnerQuery {
            client,
            params: [runner_port],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetResumableTaskRunsForRunnerBorrowed, tokio_postgres::Error> {
                Ok(GetResumableTaskRunsForRunnerBorrowed {
                    id: row.try_get(0)?,
                    task_name: row.try_get(1)?,
                    prompt: row.try_get(2)?,
                    task_type: row.try_get(3)?,
                    status: row.try_get(4)?,
                    sessions_count: row.try_get(5)?,
                    max_sessions: row.try_get(6)?,
                    error_message: row.try_get(7)?,
                    auto_continue: row.try_get(8)?,
                    config_id: row.try_get(9)?,
                    workflow_name: row.try_get(10)?,
                    workflow_id: row.try_get(11)?,
                    workflow_type: row.try_get(12)?,
                    workspace_id: row.try_get(13)?,
                    triggered_by: row.try_get(14)?,
                    parent_task_run_id: row.try_get(15)?,
                    root_task_run_id: row.try_get(16)?,
                    depth: row.try_get(17)?,
                    bridge_id: row.try_get(18)?,
                    is_reflection: row.try_get(19)?,
                    reflection_source_task_run_id: row.try_get(20)?,
                    is_follow_up: row.try_get(21)?,
                    follow_up_source_task_run_id: row.try_get(22)?,
                    is_fixer: row.try_get(23)?,
                    fixer_source_task_run_id: row.try_get(24)?,
                    is_meta_optimizer: row.try_get(25)?,
                    is_review: row.try_get(26)?,
                    blocks_parent: row.try_get(27)?,
                    runner_port: row.try_get(28)?,
                    created_at: row.try_get(29)?,
                    updated_at: row.try_get(30)?,
                    completed_at: row.try_get(31)?,
                })
            },
            mapper: |it| GetResumableTaskRunsForRunner::from(it),
        }
    }
}
pub struct LeaseTaskForResumeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn lease_task_for_resume() -> LeaseTaskForResumeStmt {
    LeaseTaskForResumeStmt(
        "UPDATE task_runs SET updated_at = NOW() WHERE id = $1 AND status = 'running' AND updated_at = $2 RETURNING id",
        None,
    )
}
impl LeaseTaskForResumeStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
        expected_updated_at: &'a chrono::DateTime<chrono::FixedOffset>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [id, expected_updated_at],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        LeaseTaskForResumeParams<T1>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for LeaseTaskForResumeStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a LeaseTaskForResumeParams<T1>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.id, &params.expected_updated_at)
    }
}
pub struct AppendTaskOutputStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn append_task_output() -> AppendTaskOutputStmt {
    AppendTaskOutputStmt(
        "UPDATE task_runs SET output_log = output_log || $1, updated_at = NOW() WHERE id = $2",
        None,
    )
}
impl AppendTaskOutputStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>(
        &'s self,
        client: &'c C,
        output: &'a T1,
        id: &'a T2,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[output, id]).await
    }
}
impl<'a, C: GenericClient + Send + Sync, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        AppendTaskOutputParams<T1, T2>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for AppendTaskOutputStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a AppendTaskOutputParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(client, &params.output, &params.id))
    }
}
pub struct UpdateTaskNameStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_task_name() -> UpdateTaskNameStmt {
    UpdateTaskNameStmt(
        "UPDATE task_runs SET task_name = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl UpdateTaskNameStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>(
        &'s self,
        client: &'c C,
        task_name: &'a T1,
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [task_name, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateTaskNameParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for UpdateTaskNameStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateTaskNameParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.task_name, &params.id)
    }
}
pub struct IncrementSessionsCountStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn increment_sessions_count() -> IncrementSessionsCountStmt {
    IncrementSessionsCountStmt(
        "UPDATE task_runs SET sessions_count = sessions_count + 1, updated_at = NOW() WHERE id = $1",
        None,
    )
}
impl IncrementSessionsCountStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[id]).await
    }
}
pub struct UpdateTaskResultDataStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_task_result_data() -> UpdateTaskResultDataStmt {
    UpdateTaskResultDataStmt(
        "UPDATE task_runs SET result_data = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl UpdateTaskResultDataStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>(
        &'s self,
        client: &'c C,
        result_data: &'a T1,
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [result_data, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateTaskResultDataParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for UpdateTaskResultDataStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateTaskResultDataParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.result_data, &params.id)
    }
}
pub struct GetRecentTaskRunsFilteredStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_recent_task_runs_filtered() -> GetRecentTaskRunsFilteredStmt {
    GetRecentTaskRunsFilteredStmt(
        "SELECT id, task_name, COALESCE(prompt, '') as prompt, COALESCE(task_type, 'task') as task_type, COALESCE(status, 'running') as status, COALESCE(sessions_count, 0) as sessions_count, COALESCE(max_sessions, 0) as max_sessions, COALESCE(error_message, '') as error_message, COALESCE(auto_continue, true) as auto_continue, COALESCE(config_id, '') as config_id, COALESCE(workflow_name, '') as workflow_name, COALESCE(workflow_id, '') as workflow_id, COALESCE(summary, ai_summary, '') as summary, COALESCE(ai_summary, '') as ai_summary, COALESCE(goal_achieved, false) as goal_achieved, COALESCE(remaining_work, '') as remaining_work, COALESCE(summary_generated_at::TEXT, '') as summary_generated_at, COALESCE(workspace_id, '') as workspace_id, COALESCE(triggered_by, '') as triggered_by, COALESCE(created_at, NOW()) as created_at, COALESCE(updated_at, NOW()) as updated_at, COALESCE(completed_at, NOW()) as completed_at FROM task_runs WHERE ($1::text IS NULL OR workflow_type = $1) AND (workflow_type IS NULL OR workflow_type != 'chat') AND ($2::integer IS NULL OR runner_port IS NULL OR runner_port = $2) ORDER BY updated_at DESC LIMIT $3",
        None,
    )
}
impl GetRecentTaskRunsFilteredStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        workflow_type: &'a Option<T1>,
        runner_port: &'a Option<i32>,
        max_results: &'a i64,
    ) -> GetRecentTaskRunsFilteredQuery<'c, 'a, 's, C, GetRecentTaskRunsFiltered, 3> {
        GetRecentTaskRunsFilteredQuery {
            client,
            params: [workflow_type, runner_port, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetRecentTaskRunsFilteredBorrowed, tokio_postgres::Error> {
                Ok(GetRecentTaskRunsFilteredBorrowed {
                    id: row.try_get(0)?,
                    task_name: row.try_get(1)?,
                    prompt: row.try_get(2)?,
                    task_type: row.try_get(3)?,
                    status: row.try_get(4)?,
                    sessions_count: row.try_get(5)?,
                    max_sessions: row.try_get(6)?,
                    error_message: row.try_get(7)?,
                    auto_continue: row.try_get(8)?,
                    config_id: row.try_get(9)?,
                    workflow_name: row.try_get(10)?,
                    workflow_id: row.try_get(11)?,
                    summary: row.try_get(12)?,
                    ai_summary: row.try_get(13)?,
                    goal_achieved: row.try_get(14)?,
                    remaining_work: row.try_get(15)?,
                    summary_generated_at: row.try_get(16)?,
                    workspace_id: row.try_get(17)?,
                    triggered_by: row.try_get(18)?,
                    created_at: row.try_get(19)?,
                    updated_at: row.try_get(20)?,
                    completed_at: row.try_get(21)?,
                })
            },
            mapper: |it| GetRecentTaskRunsFiltered::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetRecentTaskRunsFilteredParams<T1>,
        GetRecentTaskRunsFilteredQuery<'c, 'a, 's, C, GetRecentTaskRunsFiltered, 3>,
        C,
    > for GetRecentTaskRunsFilteredStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetRecentTaskRunsFilteredParams<T1>,
    ) -> GetRecentTaskRunsFilteredQuery<'c, 'a, 's, C, GetRecentTaskRunsFiltered, 3> {
        self.bind(
            client,
            &params.workflow_type,
            &params.runner_port,
            &params.max_results,
        )
    }
}
pub struct GetTaskOutputStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_output() -> GetTaskOutputStmt {
    GetTaskOutputStmt(
        "SELECT COALESCE(output_log, '') as output_log FROM task_runs WHERE id = $1",
        None,
    )
}
impl GetTaskOutputStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        id: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
