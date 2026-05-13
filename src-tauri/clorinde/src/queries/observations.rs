// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct SaveObservationParams<
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
> {
    pub title: T1,
    pub content: T2,
    pub observation_type: T3,
    pub scope: T4,
    pub topic_key: Option<T5>,
    pub content_hash: T6,
    pub project_id: Option<T7>,
    pub workflow_id: Option<T8>,
    pub task_run_id: Option<T9>,
    pub session_id: Option<T10>,
}
#[derive(Debug)]
pub struct UpsertObservationByTopicKeyParams<
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
> {
    pub title: T1,
    pub content: T2,
    pub observation_type: T3,
    pub scope: T4,
    pub topic_key: T5,
    pub content_hash: T6,
    pub project_id: Option<T7>,
    pub workflow_id: Option<T8>,
    pub task_run_id: Option<T9>,
    pub session_id: Option<T10>,
}
#[derive(Debug)]
pub struct SearchObservationsParams<T1: crate::StringSql> {
    pub query: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct SearchObservationsByProjectParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub query: T1,
    pub project_id: T2,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct GetProjectContextParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub project_id: T1,
    pub observation_type: Option<T2>,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct UpdateObservationParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
> {
    pub title: Option<T1>,
    pub content: Option<T2>,
    pub observation_type: Option<T3>,
    pub content_hash: Option<T4>,
    pub id: i64,
}
#[derive(Debug)]
pub struct CleanupStaleObservationsParams<T1: crate::StringSql> {
    pub max_revision_count: i32,
    pub retention_days: T1,
}
#[derive(Clone, Copy, Debug)]
pub struct SupersedeObservationParams {
    pub new_observation_id: i64,
    pub id: i64,
}
#[derive(Debug)]
pub struct SearchObservationsTemporalParams<T1: crate::StringSql> {
    pub query: Option<T1>,
    pub from_time: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub to_time: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub as_of: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub max_results: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct MostRevisedTopicsParams {
    pub from_time: chrono::DateTime<chrono::FixedOffset>,
    pub to_time: chrono::DateTime<chrono::FixedOffset>,
    pub max_results: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct SnapshotObservationsAtParams {
    pub as_of: chrono::DateTime<chrono::FixedOffset>,
    pub max_results: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct UpdateObservationImportanceParams {
    pub importance: f64,
    pub decay_rate: f64,
    pub id: i64,
}
#[derive(Debug)]
pub struct SaveMentalModelParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::ArraySql<Item = i64>,
    T8: crate::StringSql,
    T9: crate::StringSql,
    T10: crate::StringSql,
    T11: crate::StringSql,
> {
    pub title: T1,
    pub content: T2,
    pub observation_type: T3,
    pub scope: T4,
    pub topic_key: Option<T5>,
    pub content_hash: T6,
    pub importance: f64,
    pub decay_rate: f64,
    pub consolidated_from: Option<T7>,
    pub project_id: Option<T8>,
    pub workflow_id: Option<T9>,
    pub task_run_id: Option<T10>,
    pub session_id: Option<T11>,
}
#[derive(Clone, Copy, Debug)]
pub struct ReduceObservationImportanceParams {
    pub factor: f64,
    pub id: i64,
}
#[derive(Debug)]
pub struct GetObservationsByTypeForConsolidationParams<T1: crate::StringSql> {
    pub observation_type: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct CompleteConsolidationLogParams<T1: crate::StringSql> {
    pub observations_scanned: i32,
    pub groups_found: i32,
    pub models_created: i32,
    pub observations_merged: i32,
    pub observations_decayed: i32,
    pub observations_archived: i32,
    pub error: Option<T1>,
    pub id: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetObservation {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub observation_type: String,
    pub scope: String,
    pub topic_key: Option<String>,
    pub content_hash: String,
    pub revision_count: i32,
    pub duplicate_count: i32,
    pub project_id: Option<String>,
    pub workflow_id: Option<String>,
    pub task_run_id: Option<String>,
    pub session_id: Option<String>,
    pub is_deleted: bool,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub superseded_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetObservationBorrowed<'a> {
    pub id: i64,
    pub title: &'a str,
    pub content: &'a str,
    pub observation_type: &'a str,
    pub scope: &'a str,
    pub topic_key: Option<&'a str>,
    pub content_hash: &'a str,
    pub revision_count: i32,
    pub duplicate_count: i32,
    pub project_id: Option<&'a str>,
    pub workflow_id: Option<&'a str>,
    pub task_run_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub is_deleted: bool,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub superseded_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetObservationBorrowed<'a>> for GetObservation {
    fn from(
        GetObservationBorrowed {
            id,
            title,
            content,
            observation_type,
            scope,
            topic_key,
            content_hash,
            revision_count,
            duplicate_count,
            project_id,
            workflow_id,
            task_run_id,
            session_id,
            is_deleted,
            valid_from,
            valid_until,
            superseded_by,
            created_at,
            updated_at,
        }: GetObservationBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            content: content.into(),
            observation_type: observation_type.into(),
            scope: scope.into(),
            topic_key: topic_key.map(|v| v.into()),
            content_hash: content_hash.into(),
            revision_count,
            duplicate_count,
            project_id: project_id.map(|v| v.into()),
            workflow_id: workflow_id.map(|v| v.into()),
            task_run_id: task_run_id.map(|v| v.into()),
            session_id: session_id.map(|v| v.into()),
            is_deleted,
            valid_from,
            valid_until,
            superseded_by,
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObservationSearchRow {
    pub id: i64,
    pub title: String,
    pub content_preview: String,
    pub observation_type: String,
    pub scope: String,
    pub topic_key: Option<String>,
    pub revision_count: i32,
    pub project_id: Option<String>,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub superseded_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: f32,
}
pub struct ObservationSearchRowBorrowed<'a> {
    pub id: i64,
    pub title: &'a str,
    pub content_preview: &'a str,
    pub observation_type: &'a str,
    pub scope: &'a str,
    pub topic_key: Option<&'a str>,
    pub revision_count: i32,
    pub project_id: Option<&'a str>,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub superseded_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: f32,
}
impl<'a> From<ObservationSearchRowBorrowed<'a>> for ObservationSearchRow {
    fn from(
        ObservationSearchRowBorrowed {
            id,
            title,
            content_preview,
            observation_type,
            scope,
            topic_key,
            revision_count,
            project_id,
            valid_from,
            valid_until,
            superseded_by,
            created_at,
            updated_at,
            rank,
        }: ObservationSearchRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            content_preview: content_preview.into(),
            observation_type: observation_type.into(),
            scope: scope.into(),
            topic_key: topic_key.map(|v| v.into()),
            revision_count,
            project_id: project_id.map(|v| v.into()),
            valid_from,
            valid_until,
            superseded_by,
            created_at,
            updated_at,
            rank,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct FindDuplicate {
    pub id: i64,
    pub duplicate_count: i32,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObservationPreview {
    pub id: i64,
    pub title: String,
    pub content_preview: String,
    pub observation_type: String,
    pub scope: String,
    pub topic_key: Option<String>,
    pub revision_count: i32,
    pub project_id: Option<String>,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub superseded_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ObservationPreviewBorrowed<'a> {
    pub id: i64,
    pub title: &'a str,
    pub content_preview: &'a str,
    pub observation_type: &'a str,
    pub scope: &'a str,
    pub topic_key: Option<&'a str>,
    pub revision_count: i32,
    pub project_id: Option<&'a str>,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub superseded_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ObservationPreviewBorrowed<'a>> for ObservationPreview {
    fn from(
        ObservationPreviewBorrowed {
            id,
            title,
            content_preview,
            observation_type,
            scope,
            topic_key,
            revision_count,
            project_id,
            valid_from,
            valid_until,
            superseded_by,
            created_at,
            updated_at,
        }: ObservationPreviewBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            content_preview: content_preview.into(),
            observation_type: observation_type.into(),
            scope: scope.into(),
            topic_key: topic_key.map(|v| v.into()),
            revision_count,
            project_id: project_id.map(|v| v.into()),
            valid_from,
            valid_until,
            superseded_by,
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetObservationStats {
    pub observation_type: String,
    pub count: i64,
    pub latest_updated: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetObservationStatsBorrowed<'a> {
    pub observation_type: &'a str,
    pub count: i64,
    pub latest_updated: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetObservationStatsBorrowed<'a>> for GetObservationStats {
    fn from(
        GetObservationStatsBorrowed {
            observation_type,
            count,
            latest_updated,
        }: GetObservationStatsBorrowed<'a>,
    ) -> Self {
        Self {
            observation_type: observation_type.into(),
            count,
            latest_updated,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TemporalSearchRow {
    pub id: i64,
    pub title: String,
    pub content_preview: String,
    pub observation_type: String,
    pub scope: String,
    pub topic_key: Option<String>,
    pub revision_count: i32,
    pub project_id: Option<String>,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub superseded_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: f32,
}
pub struct TemporalSearchRowBorrowed<'a> {
    pub id: i64,
    pub title: &'a str,
    pub content_preview: &'a str,
    pub observation_type: &'a str,
    pub scope: &'a str,
    pub topic_key: Option<&'a str>,
    pub revision_count: i32,
    pub project_id: Option<&'a str>,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub superseded_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: f32,
}
impl<'a> From<TemporalSearchRowBorrowed<'a>> for TemporalSearchRow {
    fn from(
        TemporalSearchRowBorrowed {
            id,
            title,
            content_preview,
            observation_type,
            scope,
            topic_key,
            revision_count,
            project_id,
            valid_from,
            valid_until,
            superseded_by,
            created_at,
            updated_at,
            rank,
        }: TemporalSearchRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            content_preview: content_preview.into(),
            observation_type: observation_type.into(),
            scope: scope.into(),
            topic_key: topic_key.map(|v| v.into()),
            revision_count,
            project_id: project_id.map(|v| v.into()),
            valid_from,
            valid_until,
            superseded_by,
            created_at,
            updated_at,
            rank,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WeeklyTrendRow {
    pub observation_type: String,
    pub week_start: chrono::DateTime<chrono::FixedOffset>,
    pub count: i64,
}
pub struct WeeklyTrendRowBorrowed<'a> {
    pub observation_type: &'a str,
    pub week_start: chrono::DateTime<chrono::FixedOffset>,
    pub count: i64,
}
impl<'a> From<WeeklyTrendRowBorrowed<'a>> for WeeklyTrendRow {
    fn from(
        WeeklyTrendRowBorrowed {
            observation_type,
            week_start,
            count,
        }: WeeklyTrendRowBorrowed<'a>,
    ) -> Self {
        Self {
            observation_type: observation_type.into(),
            week_start,
            count,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TopicRevisionRow {
    pub topic_key: Option<String>,
    pub title: String,
    pub revisions: i64,
}
pub struct TopicRevisionRowBorrowed<'a> {
    pub topic_key: Option<&'a str>,
    pub title: &'a str,
    pub revisions: i64,
}
impl<'a> From<TopicRevisionRowBorrowed<'a>> for TopicRevisionRow {
    fn from(
        TopicRevisionRowBorrowed {
            topic_key,
            title,
            revisions,
        }: TopicRevisionRowBorrowed<'a>,
    ) -> Self {
        Self {
            topic_key: topic_key.map(|v| v.into()),
            title: title.into(),
            revisions,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObservationHistoryRow {
    pub id: i64,
    pub observation_id: i64,
    pub title: String,
    pub content_preview: String,
    pub content_hash: String,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: chrono::DateTime<chrono::FixedOffset>,
    pub revision_number: i32,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ObservationHistoryRowBorrowed<'a> {
    pub id: i64,
    pub observation_id: i64,
    pub title: &'a str,
    pub content_preview: &'a str,
    pub content_hash: &'a str,
    pub valid_from: chrono::DateTime<chrono::FixedOffset>,
    pub valid_until: chrono::DateTime<chrono::FixedOffset>,
    pub revision_number: i32,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ObservationHistoryRowBorrowed<'a>> for ObservationHistoryRow {
    fn from(
        ObservationHistoryRowBorrowed {
            id,
            observation_id,
            title,
            content_preview,
            content_hash,
            valid_from,
            valid_until,
            revision_number,
            created_at,
        }: ObservationHistoryRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            observation_id,
            title: title.into(),
            content_preview: content_preview.into(),
            content_hash: content_hash.into(),
            valid_from,
            valid_until,
            revision_number,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConsolidationObservation {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub observation_type: String,
    pub scope: String,
    pub topic_key: Option<String>,
    pub content_hash: String,
    pub revision_count: i32,
    pub duplicate_count: i32,
    pub importance: f64,
    pub access_count: i32,
    pub decay_rate: f64,
    pub is_mental_model: bool,
    pub consolidated_from: Option<Vec<i64>>,
    pub project_id: Option<String>,
    pub workflow_id: Option<String>,
    pub task_run_id: Option<String>,
    pub session_id: Option<String>,
    pub last_accessed_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ConsolidationObservationBorrowed<'a> {
    pub id: i64,
    pub title: &'a str,
    pub content: &'a str,
    pub observation_type: &'a str,
    pub scope: &'a str,
    pub topic_key: Option<&'a str>,
    pub content_hash: &'a str,
    pub revision_count: i32,
    pub duplicate_count: i32,
    pub importance: f64,
    pub access_count: i32,
    pub decay_rate: f64,
    pub is_mental_model: bool,
    pub consolidated_from: Option<crate::ArrayIterator<'a, i64>>,
    pub project_id: Option<&'a str>,
    pub workflow_id: Option<&'a str>,
    pub task_run_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub last_accessed_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ConsolidationObservationBorrowed<'a>> for ConsolidationObservation {
    fn from(
        ConsolidationObservationBorrowed {
            id,
            title,
            content,
            observation_type,
            scope,
            topic_key,
            content_hash,
            revision_count,
            duplicate_count,
            importance,
            access_count,
            decay_rate,
            is_mental_model,
            consolidated_from,
            project_id,
            workflow_id,
            task_run_id,
            session_id,
            last_accessed_at,
            created_at,
            updated_at,
        }: ConsolidationObservationBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            content: content.into(),
            observation_type: observation_type.into(),
            scope: scope.into(),
            topic_key: topic_key.map(|v| v.into()),
            content_hash: content_hash.into(),
            revision_count,
            duplicate_count,
            importance,
            access_count,
            decay_rate,
            is_mental_model,
            consolidated_from: consolidated_from.map(|v| v.map(|v| v).collect()),
            project_id: project_id.map(|v| v.into()),
            workflow_id: workflow_id.map(|v| v.into()),
            task_run_id: task_run_id.map(|v| v.into()),
            session_id: session_id.map(|v| v.into()),
            last_accessed_at,
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetDecayPreview {
    pub id: i64,
    pub title: String,
    pub observation_type: String,
    pub importance: f64,
    pub decay_rate: f64,
    pub is_mental_model: bool,
    pub last_activity: chrono::DateTime<chrono::FixedOffset>,
    pub current_retention: f64,
}
pub struct GetDecayPreviewBorrowed<'a> {
    pub id: i64,
    pub title: &'a str,
    pub observation_type: &'a str,
    pub importance: f64,
    pub decay_rate: f64,
    pub is_mental_model: bool,
    pub last_activity: chrono::DateTime<chrono::FixedOffset>,
    pub current_retention: f64,
}
impl<'a> From<GetDecayPreviewBorrowed<'a>> for GetDecayPreview {
    fn from(
        GetDecayPreviewBorrowed {
            id,
            title,
            observation_type,
            importance,
            decay_rate,
            is_mental_model,
            last_activity,
            current_retention,
        }: GetDecayPreviewBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            observation_type: observation_type.into(),
            importance,
            decay_rate,
            is_mental_model,
            last_activity,
            current_retention,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConsolidationLogRow {
    pub id: i64,
    pub started_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub observations_scanned: i32,
    pub groups_found: i32,
    pub models_created: i32,
    pub observations_merged: i32,
    pub observations_decayed: i32,
    pub observations_archived: i32,
    pub error: Option<String>,
}
pub struct ConsolidationLogRowBorrowed<'a> {
    pub id: i64,
    pub started_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub observations_scanned: i32,
    pub groups_found: i32,
    pub models_created: i32,
    pub observations_merged: i32,
    pub observations_decayed: i32,
    pub observations_archived: i32,
    pub error: Option<&'a str>,
}
impl<'a> From<ConsolidationLogRowBorrowed<'a>> for ConsolidationLogRow {
    fn from(
        ConsolidationLogRowBorrowed {
            id,
            started_at,
            completed_at,
            observations_scanned,
            groups_found,
            models_created,
            observations_merged,
            observations_decayed,
            observations_archived,
            error,
        }: ConsolidationLogRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            started_at,
            completed_at,
            observations_scanned,
            groups_found,
            models_created,
            observations_merged,
            observations_decayed,
            observations_archived,
            error: error.map(|v| v.into()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct LastConsolidationRow {
    pub last_run: Option<chrono::DateTime<chrono::FixedOffset>>,
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct MemoryHealthRow {
    pub total_observations: i64,
    pub total_mental_models: i64,
    pub decay_queue_size: i64,
    pub avg_importance: Option<f64>,
    pub avg_access_count: Option<f64>,
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct I64Query<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<i64, tokio_postgres::Error>,
    mapper: fn(i64) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> I64Query<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(self, mapper: fn(i64) -> R) -> I64Query<'c, 'a, 's, C, R, N> {
        I64Query {
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
pub struct GetObservationQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetObservationBorrowed, tokio_postgres::Error>,
    mapper: fn(GetObservationBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetObservationQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetObservationBorrowed) -> R,
    ) -> GetObservationQuery<'c, 'a, 's, C, R, N> {
        GetObservationQuery {
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
pub struct ObservationSearchRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ObservationSearchRowBorrowed, tokio_postgres::Error>,
    mapper: fn(ObservationSearchRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ObservationSearchRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ObservationSearchRowBorrowed) -> R,
    ) -> ObservationSearchRowQuery<'c, 'a, 's, C, R, N> {
        ObservationSearchRowQuery {
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
pub struct FindDuplicateQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<FindDuplicate, tokio_postgres::Error>,
    mapper: fn(FindDuplicate) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> FindDuplicateQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(self, mapper: fn(FindDuplicate) -> R) -> FindDuplicateQuery<'c, 'a, 's, C, R, N> {
        FindDuplicateQuery {
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
pub struct ObservationPreviewQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ObservationPreviewBorrowed, tokio_postgres::Error>,
    mapper: fn(ObservationPreviewBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ObservationPreviewQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ObservationPreviewBorrowed) -> R,
    ) -> ObservationPreviewQuery<'c, 'a, 's, C, R, N> {
        ObservationPreviewQuery {
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
pub struct GetObservationStatsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetObservationStatsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetObservationStatsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetObservationStatsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetObservationStatsBorrowed) -> R,
    ) -> GetObservationStatsQuery<'c, 'a, 's, C, R, N> {
        GetObservationStatsQuery {
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
pub struct TemporalSearchRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<TemporalSearchRowBorrowed, tokio_postgres::Error>,
    mapper: fn(TemporalSearchRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> TemporalSearchRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(TemporalSearchRowBorrowed) -> R,
    ) -> TemporalSearchRowQuery<'c, 'a, 's, C, R, N> {
        TemporalSearchRowQuery {
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
pub struct WeeklyTrendRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<WeeklyTrendRowBorrowed, tokio_postgres::Error>,
    mapper: fn(WeeklyTrendRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> WeeklyTrendRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(WeeklyTrendRowBorrowed) -> R,
    ) -> WeeklyTrendRowQuery<'c, 'a, 's, C, R, N> {
        WeeklyTrendRowQuery {
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
pub struct TopicRevisionRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<TopicRevisionRowBorrowed, tokio_postgres::Error>,
    mapper: fn(TopicRevisionRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> TopicRevisionRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(TopicRevisionRowBorrowed) -> R,
    ) -> TopicRevisionRowQuery<'c, 'a, 's, C, R, N> {
        TopicRevisionRowQuery {
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
pub struct ObservationHistoryRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ObservationHistoryRowBorrowed, tokio_postgres::Error>,
    mapper: fn(ObservationHistoryRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ObservationHistoryRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ObservationHistoryRowBorrowed) -> R,
    ) -> ObservationHistoryRowQuery<'c, 'a, 's, C, R, N> {
        ObservationHistoryRowQuery {
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
pub struct ConsolidationObservationQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ConsolidationObservationBorrowed, tokio_postgres::Error>,
    mapper: fn(ConsolidationObservationBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ConsolidationObservationQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ConsolidationObservationBorrowed) -> R,
    ) -> ConsolidationObservationQuery<'c, 'a, 's, C, R, N> {
        ConsolidationObservationQuery {
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
pub struct GetDecayPreviewQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetDecayPreviewBorrowed, tokio_postgres::Error>,
    mapper: fn(GetDecayPreviewBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetDecayPreviewQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetDecayPreviewBorrowed) -> R,
    ) -> GetDecayPreviewQuery<'c, 'a, 's, C, R, N> {
        GetDecayPreviewQuery {
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
pub struct ConsolidationLogRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ConsolidationLogRowBorrowed, tokio_postgres::Error>,
    mapper: fn(ConsolidationLogRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ConsolidationLogRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ConsolidationLogRowBorrowed) -> R,
    ) -> ConsolidationLogRowQuery<'c, 'a, 's, C, R, N> {
        ConsolidationLogRowQuery {
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
pub struct LastConsolidationRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<LastConsolidationRow, tokio_postgres::Error>,
    mapper: fn(LastConsolidationRow) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> LastConsolidationRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(LastConsolidationRow) -> R,
    ) -> LastConsolidationRowQuery<'c, 'a, 's, C, R, N> {
        LastConsolidationRowQuery {
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
pub struct MemoryHealthRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<MemoryHealthRow, tokio_postgres::Error>,
    mapper: fn(MemoryHealthRow) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> MemoryHealthRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(MemoryHealthRow) -> R,
    ) -> MemoryHealthRowQuery<'c, 'a, 's, C, R, N> {
        MemoryHealthRowQuery {
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
pub struct SaveObservationStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_observation() -> SaveObservationStmt {
    SaveObservationStmt(
        "INSERT INTO observations (title, content, observation_type, scope, topic_key, content_hash, project_id, workflow_id, task_run_id, session_id, valid_from) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW()) RETURNING id",
        None,
    )
}
impl SaveObservationStmt {
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
    >(
        &'s self,
        client: &'c C,
        title: &'a T1,
        content: &'a T2,
        observation_type: &'a T3,
        scope: &'a T4,
        topic_key: &'a Option<T5>,
        content_hash: &'a T6,
        project_id: &'a Option<T7>,
        workflow_id: &'a Option<T8>,
        task_run_id: &'a Option<T9>,
        session_id: &'a Option<T10>,
    ) -> I64Query<'c, 'a, 's, C, i64, 10> {
        I64Query {
            client,
            params: [
                title,
                content,
                observation_type,
                scope,
                topic_key,
                content_hash,
                project_id,
                workflow_id,
                task_run_id,
                session_id,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SaveObservationParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
        I64Query<'c, 'a, 's, C, i64, 10>,
        C,
    > for SaveObservationStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SaveObservationParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
    ) -> I64Query<'c, 'a, 's, C, i64, 10> {
        self.bind(
            client,
            &params.title,
            &params.content,
            &params.observation_type,
            &params.scope,
            &params.topic_key,
            &params.content_hash,
            &params.project_id,
            &params.workflow_id,
            &params.task_run_id,
            &params.session_id,
        )
    }
}
pub struct SnapshotBeforeUpsertStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn snapshot_before_upsert() -> SnapshotBeforeUpsertStmt {
    SnapshotBeforeUpsertStmt(
        "INSERT INTO observation_history (observation_id, title, content, content_hash, valid_from, valid_until, revision_number) SELECT id, title, content, content_hash, valid_from, NOW(), revision_count FROM observations WHERE topic_key = $1 AND topic_key IS NOT NULL AND NOT is_deleted",
        None,
    )
}
impl SnapshotBeforeUpsertStmt {
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
        topic_key: &'a Option<T1>,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[topic_key]).await
    }
}
pub struct UpsertObservationByTopicKeyStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn upsert_observation_by_topic_key() -> UpsertObservationByTopicKeyStmt {
    UpsertObservationByTopicKeyStmt(
        "INSERT INTO observations (title, content, observation_type, scope, topic_key, content_hash, project_id, workflow_id, task_run_id, session_id, valid_from) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW()) ON CONFLICT (topic_key) WHERE topic_key IS NOT NULL AND NOT is_deleted DO UPDATE SET title = EXCLUDED.title, content = EXCLUDED.content, content_hash = EXCLUDED.content_hash, observation_type = EXCLUDED.observation_type, revision_count = observations.revision_count + 1, valid_from = NOW(), updated_at = NOW() RETURNING id",
        None,
    )
}
impl UpsertObservationByTopicKeyStmt {
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
    >(
        &'s self,
        client: &'c C,
        title: &'a T1,
        content: &'a T2,
        observation_type: &'a T3,
        scope: &'a T4,
        topic_key: &'a T5,
        content_hash: &'a T6,
        project_id: &'a Option<T7>,
        workflow_id: &'a Option<T8>,
        task_run_id: &'a Option<T9>,
        session_id: &'a Option<T10>,
    ) -> I64Query<'c, 'a, 's, C, i64, 10> {
        I64Query {
            client,
            params: [
                title,
                content,
                observation_type,
                scope,
                topic_key,
                content_hash,
                project_id,
                workflow_id,
                task_run_id,
                session_id,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpsertObservationByTopicKeyParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
        I64Query<'c, 'a, 's, C, i64, 10>,
        C,
    > for UpsertObservationByTopicKeyStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpsertObservationByTopicKeyParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
    ) -> I64Query<'c, 'a, 's, C, i64, 10> {
        self.bind(
            client,
            &params.title,
            &params.content,
            &params.observation_type,
            &params.scope,
            &params.topic_key,
            &params.content_hash,
            &params.project_id,
            &params.workflow_id,
            &params.task_run_id,
            &params.session_id,
        )
    }
}
pub struct GetObservationStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_observation() -> GetObservationStmt {
    GetObservationStmt(
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, project_id, workflow_id, task_run_id, session_id, is_deleted, valid_from, valid_until, superseded_by, created_at, updated_at FROM observations WHERE id = $1 AND NOT is_deleted",
        None,
    )
}
impl GetObservationStmt {
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
        id: &'a i64,
    ) -> GetObservationQuery<'c, 'a, 's, C, GetObservation, 1> {
        GetObservationQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetObservationBorrowed, tokio_postgres::Error> {
                Ok(GetObservationBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    content_hash: row.try_get(6)?,
                    revision_count: row.try_get(7)?,
                    duplicate_count: row.try_get(8)?,
                    project_id: row.try_get(9)?,
                    workflow_id: row.try_get(10)?,
                    task_run_id: row.try_get(11)?,
                    session_id: row.try_get(12)?,
                    is_deleted: row.try_get(13)?,
                    valid_from: row.try_get(14)?,
                    valid_until: row.try_get(15)?,
                    superseded_by: row.try_get(16)?,
                    created_at: row.try_get(17)?,
                    updated_at: row.try_get(18)?,
                })
            },
            mapper: |it| GetObservation::from(it),
        }
    }
}
pub struct SearchObservationsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_observations() -> SearchObservationsStmt {
    SearchObservationsStmt(
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, valid_from, valid_until, superseded_by, created_at, updated_at, ts_rank(to_tsvector('english', title || ' ' || content), plainto_tsquery('english', $1)) as rank FROM observations WHERE NOT is_deleted AND to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', $1) ORDER BY rank DESC LIMIT $2",
        None,
    )
}
impl SearchObservationsStmt {
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
        query: &'a T1,
        max_results: &'a i64,
    ) -> ObservationSearchRowQuery<'c, 'a, 's, C, ObservationSearchRow, 2> {
        ObservationSearchRowQuery {
            client,
            params: [query, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ObservationSearchRowBorrowed, tokio_postgres::Error> {
                Ok(ObservationSearchRowBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content_preview: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    revision_count: row.try_get(6)?,
                    project_id: row.try_get(7)?,
                    valid_from: row.try_get(8)?,
                    valid_until: row.try_get(9)?,
                    superseded_by: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                    updated_at: row.try_get(12)?,
                    rank: row.try_get(13)?,
                })
            },
            mapper: |it| ObservationSearchRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SearchObservationsParams<T1>,
        ObservationSearchRowQuery<'c, 'a, 's, C, ObservationSearchRow, 2>,
        C,
    > for SearchObservationsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SearchObservationsParams<T1>,
    ) -> ObservationSearchRowQuery<'c, 'a, 's, C, ObservationSearchRow, 2> {
        self.bind(client, &params.query, &params.max_results)
    }
}
pub struct SearchObservationsByProjectStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_observations_by_project() -> SearchObservationsByProjectStmt {
    SearchObservationsByProjectStmt(
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, valid_from, valid_until, superseded_by, created_at, updated_at, ts_rank(to_tsvector('english', title || ' ' || content), plainto_tsquery('english', $1)) as rank FROM observations WHERE NOT is_deleted AND project_id = $2 AND to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', $1) ORDER BY rank DESC LIMIT $3",
        None,
    )
}
impl SearchObservationsByProjectStmt {
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
        query: &'a T1,
        project_id: &'a T2,
        max_results: &'a i64,
    ) -> ObservationSearchRowQuery<'c, 'a, 's, C, ObservationSearchRow, 3> {
        ObservationSearchRowQuery {
            client,
            params: [query, project_id, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ObservationSearchRowBorrowed, tokio_postgres::Error> {
                Ok(ObservationSearchRowBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content_preview: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    revision_count: row.try_get(6)?,
                    project_id: row.try_get(7)?,
                    valid_from: row.try_get(8)?,
                    valid_until: row.try_get(9)?,
                    superseded_by: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                    updated_at: row.try_get(12)?,
                    rank: row.try_get(13)?,
                })
            },
            mapper: |it| ObservationSearchRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SearchObservationsByProjectParams<T1, T2>,
        ObservationSearchRowQuery<'c, 'a, 's, C, ObservationSearchRow, 3>,
        C,
    > for SearchObservationsByProjectStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SearchObservationsByProjectParams<T1, T2>,
    ) -> ObservationSearchRowQuery<'c, 'a, 's, C, ObservationSearchRow, 3> {
        self.bind(
            client,
            &params.query,
            &params.project_id,
            &params.max_results,
        )
    }
}
pub struct FindDuplicateStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn find_duplicate() -> FindDuplicateStmt {
    FindDuplicateStmt(
        "SELECT id, duplicate_count FROM observations WHERE content_hash = $1 AND NOT is_deleted AND created_at > NOW() - INTERVAL '15 minutes' LIMIT 1",
        None,
    )
}
impl FindDuplicateStmt {
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
        content_hash: &'a T1,
    ) -> FindDuplicateQuery<'c, 'a, 's, C, FindDuplicate, 1> {
        FindDuplicateQuery {
            client,
            params: [content_hash],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row: &tokio_postgres::Row| -> Result<FindDuplicate, tokio_postgres::Error> {
                Ok(FindDuplicate {
                    id: row.try_get(0)?,
                    duplicate_count: row.try_get(1)?,
                })
            },
            mapper: |it| FindDuplicate::from(it),
        }
    }
}
pub struct IncrementDuplicateCountStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn increment_duplicate_count() -> IncrementDuplicateCountStmt {
    IncrementDuplicateCountStmt(
        "UPDATE observations SET duplicate_count = duplicate_count + 1 WHERE id = $1",
        None,
    )
}
impl IncrementDuplicateCountStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<'c, 'a, 's, C: GenericClient>(
        &'s self,
        client: &'c C,
        id: &'a i64,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[id]).await
    }
}
pub struct GetProjectContextStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_project_context() -> GetProjectContextStmt {
    GetProjectContextStmt(
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, valid_from, valid_until, superseded_by, created_at, updated_at FROM observations WHERE NOT is_deleted AND (project_id = $1 OR scope = 'global') AND ($2::text IS NULL OR observation_type = $2) ORDER BY updated_at DESC LIMIT $3",
        None,
    )
}
impl GetProjectContextStmt {
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
        project_id: &'a T1,
        observation_type: &'a Option<T2>,
        max_results: &'a i64,
    ) -> ObservationPreviewQuery<'c, 'a, 's, C, ObservationPreview, 3> {
        ObservationPreviewQuery {
            client,
            params: [project_id, observation_type, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ObservationPreviewBorrowed, tokio_postgres::Error> {
                Ok(ObservationPreviewBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content_preview: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    revision_count: row.try_get(6)?,
                    project_id: row.try_get(7)?,
                    valid_from: row.try_get(8)?,
                    valid_until: row.try_get(9)?,
                    superseded_by: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                    updated_at: row.try_get(12)?,
                })
            },
            mapper: |it| ObservationPreview::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetProjectContextParams<T1, T2>,
        ObservationPreviewQuery<'c, 'a, 's, C, ObservationPreview, 3>,
        C,
    > for GetProjectContextStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetProjectContextParams<T1, T2>,
    ) -> ObservationPreviewQuery<'c, 'a, 's, C, ObservationPreview, 3> {
        self.bind(
            client,
            &params.project_id,
            &params.observation_type,
            &params.max_results,
        )
    }
}
pub struct UpdateObservationStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_observation() -> UpdateObservationStmt {
    UpdateObservationStmt(
        "UPDATE observations SET title = COALESCE($1, title), content = COALESCE($2, content), observation_type = COALESCE($3, observation_type), content_hash = COALESCE($4, content_hash), updated_at = NOW() WHERE id = $5 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl UpdateObservationStmt {
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
    >(
        &'s self,
        client: &'c C,
        title: &'a Option<T1>,
        content: &'a Option<T2>,
        observation_type: &'a Option<T3>,
        content_hash: &'a Option<T4>,
        id: &'a i64,
    ) -> I64Query<'c, 'a, 's, C, i64, 5> {
        I64Query {
            client,
            params: [title, content, observation_type, content_hash, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateObservationParams<T1, T2, T3, T4>,
        I64Query<'c, 'a, 's, C, i64, 5>,
        C,
    > for UpdateObservationStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateObservationParams<T1, T2, T3, T4>,
    ) -> I64Query<'c, 'a, 's, C, i64, 5> {
        self.bind(
            client,
            &params.title,
            &params.content,
            &params.observation_type,
            &params.content_hash,
            &params.id,
        )
    }
}
pub struct SoftDeleteObservationStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn soft_delete_observation() -> SoftDeleteObservationStmt {
    SoftDeleteObservationStmt(
        "UPDATE observations SET is_deleted = true, updated_at = NOW() WHERE id = $1 RETURNING id",
        None,
    )
}
impl SoftDeleteObservationStmt {
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
        id: &'a i64,
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct GetObservationsByTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_observations_by_task_run() -> GetObservationsByTaskRunStmt {
    GetObservationsByTaskRunStmt(
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, valid_from, valid_until, superseded_by, created_at, updated_at FROM observations WHERE task_run_id = $1 AND NOT is_deleted ORDER BY created_at ASC",
        None,
    )
}
impl GetObservationsByTaskRunStmt {
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
        task_run_id: &'a T1,
    ) -> ObservationPreviewQuery<'c, 'a, 's, C, ObservationPreview, 1> {
        ObservationPreviewQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ObservationPreviewBorrowed, tokio_postgres::Error> {
                Ok(ObservationPreviewBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content_preview: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    revision_count: row.try_get(6)?,
                    project_id: row.try_get(7)?,
                    valid_from: row.try_get(8)?,
                    valid_until: row.try_get(9)?,
                    superseded_by: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                    updated_at: row.try_get(12)?,
                })
            },
            mapper: |it| ObservationPreview::from(it),
        }
    }
}
pub struct GetAllObservationsFullStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_all_observations_full() -> GetAllObservationsFullStmt {
    GetAllObservationsFullStmt(
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, project_id, workflow_id, task_run_id, session_id, is_deleted, valid_from, valid_until, superseded_by, created_at, updated_at FROM observations WHERE NOT is_deleted ORDER BY updated_at DESC LIMIT $1",
        None,
    )
}
impl GetAllObservationsFullStmt {
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
        max_results: &'a i64,
    ) -> GetObservationQuery<'c, 'a, 's, C, GetObservation, 1> {
        GetObservationQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetObservationBorrowed, tokio_postgres::Error> {
                Ok(GetObservationBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    content_hash: row.try_get(6)?,
                    revision_count: row.try_get(7)?,
                    duplicate_count: row.try_get(8)?,
                    project_id: row.try_get(9)?,
                    workflow_id: row.try_get(10)?,
                    task_run_id: row.try_get(11)?,
                    session_id: row.try_get(12)?,
                    is_deleted: row.try_get(13)?,
                    valid_from: row.try_get(14)?,
                    valid_until: row.try_get(15)?,
                    superseded_by: row.try_get(16)?,
                    created_at: row.try_get(17)?,
                    updated_at: row.try_get(18)?,
                })
            },
            mapper: |it| GetObservation::from(it),
        }
    }
}
pub struct CleanupStaleObservationsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn cleanup_stale_observations() -> CleanupStaleObservationsStmt {
    CleanupStaleObservationsStmt(
        "UPDATE observations SET is_deleted = true, updated_at = NOW() WHERE NOT is_deleted AND revision_count <= $1 AND duplicate_count = 0 AND updated_at < NOW() - ($2 || ' days')::interval RETURNING id",
        None,
    )
}
impl CleanupStaleObservationsStmt {
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
        max_revision_count: &'a i32,
        retention_days: &'a T1,
    ) -> I64Query<'c, 'a, 's, C, i64, 2> {
        I64Query {
            client,
            params: [max_revision_count, retention_days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        CleanupStaleObservationsParams<T1>,
        I64Query<'c, 'a, 's, C, i64, 2>,
        C,
    > for CleanupStaleObservationsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CleanupStaleObservationsParams<T1>,
    ) -> I64Query<'c, 'a, 's, C, i64, 2> {
        self.bind(client, &params.max_revision_count, &params.retention_days)
    }
}
pub struct GetObservationStatsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_observation_stats() -> GetObservationStatsStmt {
    GetObservationStatsStmt(
        "SELECT observation_type, COUNT(*)::bigint as count, MAX(updated_at) as latest_updated FROM observations WHERE NOT is_deleted GROUP BY observation_type ORDER BY count DESC",
        None,
    )
}
impl GetObservationStatsStmt {
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
    ) -> GetObservationStatsQuery<'c, 'a, 's, C, GetObservationStats, 0> {
        GetObservationStatsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetObservationStatsBorrowed, tokio_postgres::Error> {
                Ok(GetObservationStatsBorrowed {
                    observation_type: row.try_get(0)?,
                    count: row.try_get(1)?,
                    latest_updated: row.try_get(2)?,
                })
            },
            mapper: |it| GetObservationStats::from(it),
        }
    }
}
pub struct SupersedeObservationStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn supersede_observation() -> SupersedeObservationStmt {
    SupersedeObservationStmt(
        "UPDATE observations SET superseded_by = $1, valid_until = NOW(), updated_at = NOW() WHERE id = $2 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl SupersedeObservationStmt {
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
        new_observation_id: &'a i64,
        id: &'a i64,
    ) -> I64Query<'c, 'a, 's, C, i64, 2> {
        I64Query {
            client,
            params: [new_observation_id, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SupersedeObservationParams,
        I64Query<'c, 'a, 's, C, i64, 2>,
        C,
    > for SupersedeObservationStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SupersedeObservationParams,
    ) -> I64Query<'c, 'a, 's, C, i64, 2> {
        self.bind(client, &params.new_observation_id, &params.id)
    }
}
pub struct SearchObservationsTemporalStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_observations_temporal() -> SearchObservationsTemporalStmt {
    SearchObservationsTemporalStmt(
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, valid_from, valid_until, superseded_by, created_at, updated_at, ts_rank(to_tsvector('english', title || ' ' || content), plainto_tsquery('english', COALESCE($1, ''))) as rank FROM observations WHERE NOT is_deleted AND ($1::text IS NULL OR to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', $1)) AND ($2::timestamptz IS NULL OR valid_from >= $2) AND ($3::timestamptz IS NULL OR valid_from <= $3) AND ($4::timestamptz IS NULL OR (valid_from <= $4 AND (valid_until IS NULL OR valid_until >= $4))) ORDER BY CASE WHEN valid_until IS NULL THEN 0 ELSE 1 END, valid_from DESC LIMIT $5",
        None,
    )
}
impl SearchObservationsTemporalStmt {
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
        query: &'a Option<T1>,
        from_time: &'a Option<chrono::DateTime<chrono::FixedOffset>>,
        to_time: &'a Option<chrono::DateTime<chrono::FixedOffset>>,
        as_of: &'a Option<chrono::DateTime<chrono::FixedOffset>>,
        max_results: &'a i64,
    ) -> TemporalSearchRowQuery<'c, 'a, 's, C, TemporalSearchRow, 5> {
        TemporalSearchRowQuery {
            client,
            params: [query, from_time, to_time, as_of, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<TemporalSearchRowBorrowed, tokio_postgres::Error> {
                Ok(TemporalSearchRowBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content_preview: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    revision_count: row.try_get(6)?,
                    project_id: row.try_get(7)?,
                    valid_from: row.try_get(8)?,
                    valid_until: row.try_get(9)?,
                    superseded_by: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                    updated_at: row.try_get(12)?,
                    rank: row.try_get(13)?,
                })
            },
            mapper: |it| TemporalSearchRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SearchObservationsTemporalParams<T1>,
        TemporalSearchRowQuery<'c, 'a, 's, C, TemporalSearchRow, 5>,
        C,
    > for SearchObservationsTemporalStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SearchObservationsTemporalParams<T1>,
    ) -> TemporalSearchRowQuery<'c, 'a, 's, C, TemporalSearchRow, 5> {
        self.bind(
            client,
            &params.query,
            &params.from_time,
            &params.to_time,
            &params.as_of,
            &params.max_results,
        )
    }
}
pub struct ObservationWeeklyTrendStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn observation_weekly_trend() -> ObservationWeeklyTrendStmt {
    ObservationWeeklyTrendStmt(
        "SELECT observation_type, date_trunc('week', created_at)::timestamptz as week_start, COUNT(*)::bigint as count FROM observations WHERE NOT is_deleted AND created_at >= NOW() - ($1 || ' weeks')::interval GROUP BY observation_type, date_trunc('week', created_at) ORDER BY week_start DESC, count DESC",
        None,
    )
}
impl ObservationWeeklyTrendStmt {
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
        weeks: &'a T1,
    ) -> WeeklyTrendRowQuery<'c, 'a, 's, C, WeeklyTrendRow, 1> {
        WeeklyTrendRowQuery {
            client,
            params: [weeks],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<WeeklyTrendRowBorrowed, tokio_postgres::Error> {
                Ok(WeeklyTrendRowBorrowed {
                    observation_type: row.try_get(0)?,
                    week_start: row.try_get(1)?,
                    count: row.try_get(2)?,
                })
            },
            mapper: |it| WeeklyTrendRow::from(it),
        }
    }
}
pub struct MostRevisedTopicsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn most_revised_topics() -> MostRevisedTopicsStmt {
    MostRevisedTopicsStmt(
        "SELECT t.topic_key, t.title, t.revisions FROM ( SELECT DISTINCT ON (topic_key) topic_key, title, revision_count::bigint as revisions FROM observations WHERE NOT is_deleted AND topic_key IS NOT NULL AND updated_at >= $1 AND updated_at <= $2 ORDER BY topic_key, updated_at DESC ) t ORDER BY t.revisions DESC LIMIT $3",
        None,
    )
}
impl MostRevisedTopicsStmt {
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
        from_time: &'a chrono::DateTime<chrono::FixedOffset>,
        to_time: &'a chrono::DateTime<chrono::FixedOffset>,
        max_results: &'a i64,
    ) -> TopicRevisionRowQuery<'c, 'a, 's, C, TopicRevisionRow, 3> {
        TopicRevisionRowQuery {
            client,
            params: [from_time, to_time, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<TopicRevisionRowBorrowed, tokio_postgres::Error> {
                Ok(TopicRevisionRowBorrowed {
                    topic_key: row.try_get(0)?,
                    title: row.try_get(1)?,
                    revisions: row.try_get(2)?,
                })
            },
            mapper: |it| TopicRevisionRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        MostRevisedTopicsParams,
        TopicRevisionRowQuery<'c, 'a, 's, C, TopicRevisionRow, 3>,
        C,
    > for MostRevisedTopicsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a MostRevisedTopicsParams,
    ) -> TopicRevisionRowQuery<'c, 'a, 's, C, TopicRevisionRow, 3> {
        self.bind(
            client,
            &params.from_time,
            &params.to_time,
            &params.max_results,
        )
    }
}
pub struct GetObservationHistoryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_observation_history() -> GetObservationHistoryStmt {
    GetObservationHistoryStmt(
        "SELECT id, observation_id, title, LEFT(content, 500) as content_preview, content_hash, valid_from, valid_until, revision_number, created_at FROM observation_history WHERE observation_id = $1 ORDER BY revision_number DESC",
        None,
    )
}
impl GetObservationHistoryStmt {
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
        observation_id: &'a i64,
    ) -> ObservationHistoryRowQuery<'c, 'a, 's, C, ObservationHistoryRow, 1> {
        ObservationHistoryRowQuery {
            client,
            params: [observation_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ObservationHistoryRowBorrowed, tokio_postgres::Error> {
                Ok(ObservationHistoryRowBorrowed {
                    id: row.try_get(0)?,
                    observation_id: row.try_get(1)?,
                    title: row.try_get(2)?,
                    content_preview: row.try_get(3)?,
                    content_hash: row.try_get(4)?,
                    valid_from: row.try_get(5)?,
                    valid_until: row.try_get(6)?,
                    revision_number: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                })
            },
            mapper: |it| ObservationHistoryRow::from(it),
        }
    }
}
pub struct SnapshotObservationsAtStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn snapshot_observations_at() -> SnapshotObservationsAtStmt {
    SnapshotObservationsAtStmt(
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, project_id, workflow_id, task_run_id, session_id, is_deleted, valid_from, valid_until, superseded_by, created_at, updated_at FROM observations WHERE NOT is_deleted AND valid_from <= $1 AND (valid_until IS NULL OR valid_until >= $1) ORDER BY valid_from DESC LIMIT $2",
        None,
    )
}
impl SnapshotObservationsAtStmt {
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
        as_of: &'a chrono::DateTime<chrono::FixedOffset>,
        max_results: &'a i64,
    ) -> GetObservationQuery<'c, 'a, 's, C, GetObservation, 2> {
        GetObservationQuery {
            client,
            params: [as_of, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetObservationBorrowed, tokio_postgres::Error> {
                Ok(GetObservationBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    content_hash: row.try_get(6)?,
                    revision_count: row.try_get(7)?,
                    duplicate_count: row.try_get(8)?,
                    project_id: row.try_get(9)?,
                    workflow_id: row.try_get(10)?,
                    task_run_id: row.try_get(11)?,
                    session_id: row.try_get(12)?,
                    is_deleted: row.try_get(13)?,
                    valid_from: row.try_get(14)?,
                    valid_until: row.try_get(15)?,
                    superseded_by: row.try_get(16)?,
                    created_at: row.try_get(17)?,
                    updated_at: row.try_get(18)?,
                })
            },
            mapper: |it| GetObservation::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SnapshotObservationsAtParams,
        GetObservationQuery<'c, 'a, 's, C, GetObservation, 2>,
        C,
    > for SnapshotObservationsAtStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SnapshotObservationsAtParams,
    ) -> GetObservationQuery<'c, 'a, 's, C, GetObservation, 2> {
        self.bind(client, &params.as_of, &params.max_results)
    }
}
pub struct GetObservationsForConsolidationStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_observations_for_consolidation() -> GetObservationsForConsolidationStmt {
    GetObservationsForConsolidationStmt(
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, importance, access_count, decay_rate, is_mental_model, consolidated_from, project_id, workflow_id, task_run_id, session_id, last_accessed_at, created_at, updated_at FROM observations WHERE NOT is_deleted AND NOT is_mental_model ORDER BY importance DESC, updated_at DESC LIMIT $1",
        None,
    )
}
impl GetObservationsForConsolidationStmt {
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
        max_results: &'a i64,
    ) -> ConsolidationObservationQuery<'c, 'a, 's, C, ConsolidationObservation, 1> {
        ConsolidationObservationQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ConsolidationObservationBorrowed, tokio_postgres::Error> {
                Ok(ConsolidationObservationBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    content_hash: row.try_get(6)?,
                    revision_count: row.try_get(7)?,
                    duplicate_count: row.try_get(8)?,
                    importance: row.try_get(9)?,
                    access_count: row.try_get(10)?,
                    decay_rate: row.try_get(11)?,
                    is_mental_model: row.try_get(12)?,
                    consolidated_from: row.try_get(13)?,
                    project_id: row.try_get(14)?,
                    workflow_id: row.try_get(15)?,
                    task_run_id: row.try_get(16)?,
                    session_id: row.try_get(17)?,
                    last_accessed_at: row.try_get(18)?,
                    created_at: row.try_get(19)?,
                    updated_at: row.try_get(20)?,
                })
            },
            mapper: |it| ConsolidationObservation::from(it),
        }
    }
}
pub struct GetMentalModelsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_mental_models() -> GetMentalModelsStmt {
    GetMentalModelsStmt(
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, importance, access_count, decay_rate, is_mental_model, consolidated_from, project_id, workflow_id, task_run_id, session_id, last_accessed_at, created_at, updated_at FROM observations WHERE NOT is_deleted AND is_mental_model ORDER BY importance DESC, updated_at DESC LIMIT $1",
        None,
    )
}
impl GetMentalModelsStmt {
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
        max_results: &'a i64,
    ) -> ConsolidationObservationQuery<'c, 'a, 's, C, ConsolidationObservation, 1> {
        ConsolidationObservationQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ConsolidationObservationBorrowed, tokio_postgres::Error> {
                Ok(ConsolidationObservationBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    content_hash: row.try_get(6)?,
                    revision_count: row.try_get(7)?,
                    duplicate_count: row.try_get(8)?,
                    importance: row.try_get(9)?,
                    access_count: row.try_get(10)?,
                    decay_rate: row.try_get(11)?,
                    is_mental_model: row.try_get(12)?,
                    consolidated_from: row.try_get(13)?,
                    project_id: row.try_get(14)?,
                    workflow_id: row.try_get(15)?,
                    task_run_id: row.try_get(16)?,
                    session_id: row.try_get(17)?,
                    last_accessed_at: row.try_get(18)?,
                    created_at: row.try_get(19)?,
                    updated_at: row.try_get(20)?,
                })
            },
            mapper: |it| ConsolidationObservation::from(it),
        }
    }
}
pub struct UpdateObservationImportanceStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_observation_importance() -> UpdateObservationImportanceStmt {
    UpdateObservationImportanceStmt(
        "UPDATE observations SET importance = $1, decay_rate = $2, updated_at = NOW() WHERE id = $3 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl UpdateObservationImportanceStmt {
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
        importance: &'a f64,
        decay_rate: &'a f64,
        id: &'a i64,
    ) -> I64Query<'c, 'a, 's, C, i64, 3> {
        I64Query {
            client,
            params: [importance, decay_rate, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateObservationImportanceParams,
        I64Query<'c, 'a, 's, C, i64, 3>,
        C,
    > for UpdateObservationImportanceStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateObservationImportanceParams,
    ) -> I64Query<'c, 'a, 's, C, i64, 3> {
        self.bind(client, &params.importance, &params.decay_rate, &params.id)
    }
}
pub struct RecordObservationAccessStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn record_observation_access() -> RecordObservationAccessStmt {
    RecordObservationAccessStmt(
        "UPDATE observations SET last_accessed_at = NOW(), access_count = access_count + 1, updated_at = NOW() WHERE id = $1 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl RecordObservationAccessStmt {
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
        id: &'a i64,
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct SaveMentalModelStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_mental_model() -> SaveMentalModelStmt {
    SaveMentalModelStmt(
        "INSERT INTO observations (title, content, observation_type, scope, topic_key, content_hash, importance, decay_rate, is_mental_model, consolidated_from, project_id, workflow_id, task_run_id, session_id, valid_from) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, true, $9, $10, $11, $12, $13, NOW()) RETURNING id",
        None,
    )
}
impl SaveMentalModelStmt {
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
        T7: crate::ArraySql<Item = i64>,
        T8: crate::StringSql,
        T9: crate::StringSql,
        T10: crate::StringSql,
        T11: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        title: &'a T1,
        content: &'a T2,
        observation_type: &'a T3,
        scope: &'a T4,
        topic_key: &'a Option<T5>,
        content_hash: &'a T6,
        importance: &'a f64,
        decay_rate: &'a f64,
        consolidated_from: &'a Option<T7>,
        project_id: &'a Option<T8>,
        workflow_id: &'a Option<T9>,
        task_run_id: &'a Option<T10>,
        session_id: &'a Option<T11>,
    ) -> I64Query<'c, 'a, 's, C, i64, 13> {
        I64Query {
            client,
            params: [
                title,
                content,
                observation_type,
                scope,
                topic_key,
                content_hash,
                importance,
                decay_rate,
                consolidated_from,
                project_id,
                workflow_id,
                task_run_id,
                session_id,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
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
    T7: crate::ArraySql<Item = i64>,
    T8: crate::StringSql,
    T9: crate::StringSql,
    T10: crate::StringSql,
    T11: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SaveMentalModelParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11>,
        I64Query<'c, 'a, 's, C, i64, 13>,
        C,
    > for SaveMentalModelStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SaveMentalModelParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11>,
    ) -> I64Query<'c, 'a, 's, C, i64, 13> {
        self.bind(
            client,
            &params.title,
            &params.content,
            &params.observation_type,
            &params.scope,
            &params.topic_key,
            &params.content_hash,
            &params.importance,
            &params.decay_rate,
            &params.consolidated_from,
            &params.project_id,
            &params.workflow_id,
            &params.task_run_id,
            &params.session_id,
        )
    }
}
pub struct ReduceObservationImportanceStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn reduce_observation_importance() -> ReduceObservationImportanceStmt {
    ReduceObservationImportanceStmt(
        "UPDATE observations SET importance = importance * $1, updated_at = NOW() WHERE id = $2 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl ReduceObservationImportanceStmt {
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
        factor: &'a f64,
        id: &'a i64,
    ) -> I64Query<'c, 'a, 's, C, i64, 2> {
        I64Query {
            client,
            params: [factor, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        ReduceObservationImportanceParams,
        I64Query<'c, 'a, 's, C, i64, 2>,
        C,
    > for ReduceObservationImportanceStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a ReduceObservationImportanceParams,
    ) -> I64Query<'c, 'a, 's, C, i64, 2> {
        self.bind(client, &params.factor, &params.id)
    }
}
pub struct DecayAndArchiveObservationsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn decay_and_archive_observations() -> DecayAndArchiveObservationsStmt {
    DecayAndArchiveObservationsStmt(
        "UPDATE observations SET is_deleted = true, updated_at = NOW() WHERE NOT is_deleted AND NOT is_mental_model AND importance * EXP(-decay_rate * EXTRACT(EPOCH FROM (NOW() - COALESCE(last_accessed_at, updated_at))) / 86400.0) < $1 RETURNING id",
        None,
    )
}
impl DecayAndArchiveObservationsStmt {
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
        retention_threshold: &'a f64,
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [retention_threshold],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct DecayAndArchiveMentalModelsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn decay_and_archive_mental_models() -> DecayAndArchiveMentalModelsStmt {
    DecayAndArchiveMentalModelsStmt(
        "UPDATE observations SET is_deleted = true, updated_at = NOW() WHERE NOT is_deleted AND is_mental_model AND importance * EXP(-decay_rate * EXTRACT(EPOCH FROM (NOW() - COALESCE(last_accessed_at, updated_at))) / 86400.0) < $1 RETURNING id",
        None,
    )
}
impl DecayAndArchiveMentalModelsStmt {
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
        retention_threshold: &'a f64,
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [retention_threshold],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct GetObservationsByTopicPrefixStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_observations_by_topic_prefix() -> GetObservationsByTopicPrefixStmt {
    GetObservationsByTopicPrefixStmt(
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, importance, access_count, decay_rate, is_mental_model, consolidated_from, project_id, workflow_id, task_run_id, session_id, last_accessed_at, created_at, updated_at FROM observations WHERE NOT is_deleted AND NOT is_mental_model AND topic_key IS NOT NULL AND topic_key LIKE $1 || '%' ORDER BY importance DESC",
        None,
    )
}
impl GetObservationsByTopicPrefixStmt {
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
        prefix: &'a T1,
    ) -> ConsolidationObservationQuery<'c, 'a, 's, C, ConsolidationObservation, 1> {
        ConsolidationObservationQuery {
            client,
            params: [prefix],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ConsolidationObservationBorrowed, tokio_postgres::Error> {
                Ok(ConsolidationObservationBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    content_hash: row.try_get(6)?,
                    revision_count: row.try_get(7)?,
                    duplicate_count: row.try_get(8)?,
                    importance: row.try_get(9)?,
                    access_count: row.try_get(10)?,
                    decay_rate: row.try_get(11)?,
                    is_mental_model: row.try_get(12)?,
                    consolidated_from: row.try_get(13)?,
                    project_id: row.try_get(14)?,
                    workflow_id: row.try_get(15)?,
                    task_run_id: row.try_get(16)?,
                    session_id: row.try_get(17)?,
                    last_accessed_at: row.try_get(18)?,
                    created_at: row.try_get(19)?,
                    updated_at: row.try_get(20)?,
                })
            },
            mapper: |it| ConsolidationObservation::from(it),
        }
    }
}
pub struct GetObservationsByTypeForConsolidationStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn get_observations_by_type_for_consolidation() -> GetObservationsByTypeForConsolidationStmt {
    GetObservationsByTypeForConsolidationStmt(
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, importance, access_count, decay_rate, is_mental_model, consolidated_from, project_id, workflow_id, task_run_id, session_id, last_accessed_at, created_at, updated_at FROM observations WHERE NOT is_deleted AND NOT is_mental_model AND observation_type = $1 ORDER BY importance DESC LIMIT $2",
        None,
    )
}
impl GetObservationsByTypeForConsolidationStmt {
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
        observation_type: &'a T1,
        max_results: &'a i64,
    ) -> ConsolidationObservationQuery<'c, 'a, 's, C, ConsolidationObservation, 2> {
        ConsolidationObservationQuery {
            client,
            params: [observation_type, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ConsolidationObservationBorrowed, tokio_postgres::Error> {
                Ok(ConsolidationObservationBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    content: row.try_get(2)?,
                    observation_type: row.try_get(3)?,
                    scope: row.try_get(4)?,
                    topic_key: row.try_get(5)?,
                    content_hash: row.try_get(6)?,
                    revision_count: row.try_get(7)?,
                    duplicate_count: row.try_get(8)?,
                    importance: row.try_get(9)?,
                    access_count: row.try_get(10)?,
                    decay_rate: row.try_get(11)?,
                    is_mental_model: row.try_get(12)?,
                    consolidated_from: row.try_get(13)?,
                    project_id: row.try_get(14)?,
                    workflow_id: row.try_get(15)?,
                    task_run_id: row.try_get(16)?,
                    session_id: row.try_get(17)?,
                    last_accessed_at: row.try_get(18)?,
                    created_at: row.try_get(19)?,
                    updated_at: row.try_get(20)?,
                })
            },
            mapper: |it| ConsolidationObservation::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetObservationsByTypeForConsolidationParams<T1>,
        ConsolidationObservationQuery<'c, 'a, 's, C, ConsolidationObservation, 2>,
        C,
    > for GetObservationsByTypeForConsolidationStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetObservationsByTypeForConsolidationParams<T1>,
    ) -> ConsolidationObservationQuery<'c, 'a, 's, C, ConsolidationObservation, 2> {
        self.bind(client, &params.observation_type, &params.max_results)
    }
}
pub struct GetDecayPreviewStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_decay_preview() -> GetDecayPreviewStmt {
    GetDecayPreviewStmt(
        "SELECT id, title, observation_type, importance, decay_rate, is_mental_model, COALESCE(last_accessed_at, updated_at) as last_activity, importance * EXP(-decay_rate * EXTRACT(EPOCH FROM (NOW() - COALESCE(last_accessed_at, updated_at))) / 86400.0) as current_retention FROM observations WHERE NOT is_deleted ORDER BY current_retention ASC LIMIT $1",
        None,
    )
}
impl GetDecayPreviewStmt {
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
        max_results: &'a i64,
    ) -> GetDecayPreviewQuery<'c, 'a, 's, C, GetDecayPreview, 1> {
        GetDecayPreviewQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetDecayPreviewBorrowed, tokio_postgres::Error> {
                Ok(GetDecayPreviewBorrowed {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    observation_type: row.try_get(2)?,
                    importance: row.try_get(3)?,
                    decay_rate: row.try_get(4)?,
                    is_mental_model: row.try_get(5)?,
                    last_activity: row.try_get(6)?,
                    current_retention: row.try_get(7)?,
                })
            },
            mapper: |it| GetDecayPreview::from(it),
        }
    }
}
pub struct InsertConsolidationLogStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_consolidation_log() -> InsertConsolidationLogStmt {
    InsertConsolidationLogStmt(
        "INSERT INTO memory_consolidation_log (started_at) VALUES (NOW()) RETURNING id",
        None,
    )
}
impl InsertConsolidationLogStmt {
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
    ) -> I64Query<'c, 'a, 's, C, i64, 0> {
        I64Query {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct CompleteConsolidationLogStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn complete_consolidation_log() -> CompleteConsolidationLogStmt {
    CompleteConsolidationLogStmt(
        "UPDATE memory_consolidation_log SET completed_at = NOW(), observations_scanned = $1, groups_found = $2, models_created = $3, observations_merged = $4, observations_decayed = $5, observations_archived = $6, error = $7 WHERE id = $8",
        None,
    )
}
impl CompleteConsolidationLogStmt {
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
        observations_scanned: &'a i32,
        groups_found: &'a i32,
        models_created: &'a i32,
        observations_merged: &'a i32,
        observations_decayed: &'a i32,
        observations_archived: &'a i32,
        error: &'a Option<T1>,
        id: &'a i64,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    observations_scanned,
                    groups_found,
                    models_created,
                    observations_merged,
                    observations_decayed,
                    observations_archived,
                    error,
                    id,
                ],
            )
            .await
    }
}
impl<'a, C: GenericClient + Send + Sync, T1: crate::StringSql>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        CompleteConsolidationLogParams<T1>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for CompleteConsolidationLogStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a CompleteConsolidationLogParams<T1>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.observations_scanned,
            &params.groups_found,
            &params.models_created,
            &params.observations_merged,
            &params.observations_decayed,
            &params.observations_archived,
            &params.error,
            &params.id,
        ))
    }
}
pub struct GetConsolidationLogStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_consolidation_log() -> GetConsolidationLogStmt {
    GetConsolidationLogStmt(
        "SELECT id, started_at, completed_at, observations_scanned, groups_found, models_created, observations_merged, observations_decayed, observations_archived, error FROM memory_consolidation_log ORDER BY started_at DESC LIMIT $1",
        None,
    )
}
impl GetConsolidationLogStmt {
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
        max_results: &'a i64,
    ) -> ConsolidationLogRowQuery<'c, 'a, 's, C, ConsolidationLogRow, 1> {
        ConsolidationLogRowQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ConsolidationLogRowBorrowed, tokio_postgres::Error> {
                Ok(ConsolidationLogRowBorrowed {
                    id: row.try_get(0)?,
                    started_at: row.try_get(1)?,
                    completed_at: row.try_get(2)?,
                    observations_scanned: row.try_get(3)?,
                    groups_found: row.try_get(4)?,
                    models_created: row.try_get(5)?,
                    observations_merged: row.try_get(6)?,
                    observations_decayed: row.try_get(7)?,
                    observations_archived: row.try_get(8)?,
                    error: row.try_get(9)?,
                })
            },
            mapper: |it| ConsolidationLogRow::from(it),
        }
    }
}
pub struct GetLastConsolidationTimeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_last_consolidation_time() -> GetLastConsolidationTimeStmt {
    GetLastConsolidationTimeStmt(
        "SELECT MAX(started_at) as last_run FROM memory_consolidation_log WHERE completed_at IS NOT NULL AND error IS NULL",
        None,
    )
}
impl GetLastConsolidationTimeStmt {
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
    ) -> LastConsolidationRowQuery<'c, 'a, 's, C, LastConsolidationRow, 0> {
        LastConsolidationRowQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<LastConsolidationRow, tokio_postgres::Error> {
                    Ok(LastConsolidationRow {
                        last_run: row.try_get(0)?,
                    })
                },
            mapper: |it| LastConsolidationRow::from(it),
        }
    }
}
pub struct GetMemoryHealthStatsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_memory_health_stats() -> GetMemoryHealthStatsStmt {
    GetMemoryHealthStatsStmt(
        "SELECT COUNT(*) FILTER (WHERE NOT is_mental_model)::bigint as total_observations, COUNT(*) FILTER (WHERE is_mental_model)::bigint as total_mental_models, COUNT(*) FILTER (WHERE importance * EXP(-decay_rate * EXTRACT(EPOCH FROM (NOW() - COALESCE(last_accessed_at, updated_at))) / 86400.0) < 0.05)::bigint as decay_queue_size, AVG(importance) as avg_importance, AVG(access_count)::double precision as avg_access_count FROM observations WHERE NOT is_deleted",
        None,
    )
}
impl GetMemoryHealthStatsStmt {
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
    ) -> MemoryHealthRowQuery<'c, 'a, 's, C, MemoryHealthRow, 0> {
        MemoryHealthRowQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<MemoryHealthRow, tokio_postgres::Error> {
                    Ok(MemoryHealthRow {
                        total_observations: row.try_get(0)?,
                        total_mental_models: row.try_get(1)?,
                        decay_queue_size: row.try_get(2)?,
                        avg_importance: row.try_get(3)?,
                        avg_access_count: row.try_get(4)?,
                    })
                },
            mapper: |it| MemoryHealthRow::from(it),
        }
    }
}
