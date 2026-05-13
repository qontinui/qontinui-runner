// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct InsertWorkflowVersionParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
> {
    pub id: T1,
    pub workflow_id: T2,
    pub version_number: i32,
    pub parent_version_id: T3,
    pub generation_task_run_id: T4,
    pub workflow_json: T5,
    pub diff_summary: T6,
    pub diff_json: T7,
    pub trigger: T8,
}
#[derive(Debug)]
pub struct InsertStepFindingLinkParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
> {
    pub id: T1,
    pub task_run_id: T2,
    pub step_name: T3,
    pub step_index: i32,
    pub finding_id: T4,
    pub link_type: T5,
    pub confidence: f64,
}
#[derive(Debug)]
pub struct GetFindingsForStepParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub task_run_id: T1,
    pub step_name: T2,
}
#[derive(Debug)]
pub struct InsertStepProvenanceParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
> {
    pub id: T1,
    pub workflow_id: T2,
    pub workflow_version_id: T3,
    pub step_name: T4,
    pub step_index: i32,
    pub phase: T5,
    pub generating_agent: T6,
    pub generation_iteration: i32,
    pub original_step_json: T7,
    pub final_step_json: T8,
    pub ui_bridge_event_ids: T9,
}
#[derive(Debug)]
pub struct GetProvenanceByAgentParams<T1: crate::StringSql> {
    pub generating_agent: T1,
    pub limit: i64,
}
#[derive(Debug)]
pub struct InsertPipelineEventParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
> {
    pub id: T1,
    pub task_run_id: T2,
    pub workflow_id: T3,
    pub event_type: T4,
    pub phase: T5,
    pub iteration: i32,
    pub payload: T6,
    pub duration_ms: i64,
    pub token_count: i64,
    pub validation_errors_before: i32,
    pub validation_errors_after: i32,
}
#[derive(Debug)]
pub struct InsertRuleInfluenceParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
> {
    pub id: T1,
    pub rule_id: T2,
    pub task_run_id: T3,
    pub workflow_id: T4,
    pub influence_type: T5,
    pub evidence: T6,
    pub phase: T7,
}
#[derive(Debug)]
pub struct UpsertCrossRunPatternParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
> {
    pub id: T1,
    pub pattern_type: T2,
    pub signature_hash: T3,
    pub workflow_name: T4,
    pub task_run_id: T5,
    pub affected_components: T6,
    pub pattern_data: T7,
}
#[derive(Debug)]
pub struct ResolvePatternParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub fix_id: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct DetectRecurringFindingsParams<T1: crate::StringSql> {
    pub workflow_name: T1,
    pub min_occurrences: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkflowVersionRow {
    pub id: String,
    pub workflow_id: String,
    pub version_number: i32,
    pub parent_version_id: Option<String>,
    pub generation_task_run_id: Option<String>,
    pub workflow_json: String,
    pub diff_summary: Option<String>,
    pub diff_json: Option<String>,
    pub trigger: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct WorkflowVersionRowBorrowed<'a> {
    pub id: &'a str,
    pub workflow_id: &'a str,
    pub version_number: i32,
    pub parent_version_id: Option<&'a str>,
    pub generation_task_run_id: Option<&'a str>,
    pub workflow_json: &'a str,
    pub diff_summary: Option<&'a str>,
    pub diff_json: Option<&'a str>,
    pub trigger: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<WorkflowVersionRowBorrowed<'a>> for WorkflowVersionRow {
    fn from(
        WorkflowVersionRowBorrowed {
            id,
            workflow_id,
            version_number,
            parent_version_id,
            generation_task_run_id,
            workflow_json,
            diff_summary,
            diff_json,
            trigger,
            created_at,
        }: WorkflowVersionRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            workflow_id: workflow_id.into(),
            version_number,
            parent_version_id: parent_version_id.map(|v| v.into()),
            generation_task_run_id: generation_task_run_id.map(|v| v.into()),
            workflow_json: workflow_json.into(),
            diff_summary: diff_summary.map(|v| v.into()),
            diff_json: diff_json.map(|v| v.into()),
            trigger: trigger.map(|v| v.into()),
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StepFindingLinkRow {
    pub id: String,
    pub task_run_id: String,
    pub step_name: String,
    pub step_index: i32,
    pub finding_id: String,
    pub link_type: String,
    pub confidence: f64,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct StepFindingLinkRowBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub step_name: &'a str,
    pub step_index: i32,
    pub finding_id: &'a str,
    pub link_type: &'a str,
    pub confidence: f64,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<StepFindingLinkRowBorrowed<'a>> for StepFindingLinkRow {
    fn from(
        StepFindingLinkRowBorrowed {
            id,
            task_run_id,
            step_name,
            step_index,
            finding_id,
            link_type,
            confidence,
            created_at,
        }: StepFindingLinkRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            step_name: step_name.into(),
            step_index,
            finding_id: finding_id.into(),
            link_type: link_type.into(),
            confidence,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StepProvenanceRow {
    pub id: String,
    pub workflow_id: String,
    pub workflow_version_id: Option<String>,
    pub step_name: String,
    pub step_index: i32,
    pub phase: String,
    pub generating_agent: String,
    pub generation_iteration: i32,
    pub original_step_json: String,
    pub final_step_json: String,
    pub ui_bridge_event_ids: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct StepProvenanceRowBorrowed<'a> {
    pub id: &'a str,
    pub workflow_id: &'a str,
    pub workflow_version_id: Option<&'a str>,
    pub step_name: &'a str,
    pub step_index: i32,
    pub phase: &'a str,
    pub generating_agent: &'a str,
    pub generation_iteration: i32,
    pub original_step_json: &'a str,
    pub final_step_json: &'a str,
    pub ui_bridge_event_ids: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<StepProvenanceRowBorrowed<'a>> for StepProvenanceRow {
    fn from(
        StepProvenanceRowBorrowed {
            id,
            workflow_id,
            workflow_version_id,
            step_name,
            step_index,
            phase,
            generating_agent,
            generation_iteration,
            original_step_json,
            final_step_json,
            ui_bridge_event_ids,
            created_at,
        }: StepProvenanceRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            workflow_id: workflow_id.into(),
            workflow_version_id: workflow_version_id.map(|v| v.into()),
            step_name: step_name.into(),
            step_index,
            phase: phase.into(),
            generating_agent: generating_agent.into(),
            generation_iteration,
            original_step_json: original_step_json.into(),
            final_step_json: final_step_json.into(),
            ui_bridge_event_ids: ui_bridge_event_ids.map(|v| v.into()),
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PipelineEventRow {
    pub id: String,
    pub task_run_id: String,
    pub workflow_id: String,
    pub event_type: String,
    pub phase: String,
    pub iteration: i32,
    pub payload: Option<String>,
    pub duration_ms: Option<i64>,
    pub token_count: Option<i64>,
    pub validation_errors_before: Option<i32>,
    pub validation_errors_after: Option<i32>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct PipelineEventRowBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub workflow_id: &'a str,
    pub event_type: &'a str,
    pub phase: &'a str,
    pub iteration: i32,
    pub payload: Option<&'a str>,
    pub duration_ms: Option<i64>,
    pub token_count: Option<i64>,
    pub validation_errors_before: Option<i32>,
    pub validation_errors_after: Option<i32>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<PipelineEventRowBorrowed<'a>> for PipelineEventRow {
    fn from(
        PipelineEventRowBorrowed {
            id,
            task_run_id,
            workflow_id,
            event_type,
            phase,
            iteration,
            payload,
            duration_ms,
            token_count,
            validation_errors_before,
            validation_errors_after,
            created_at,
        }: PipelineEventRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            workflow_id: workflow_id.into(),
            event_type: event_type.into(),
            phase: phase.into(),
            iteration,
            payload: payload.map(|v| v.into()),
            duration_ms,
            token_count,
            validation_errors_before,
            validation_errors_after,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PhaseStatsRow {
    pub phase: String,
    pub event_count: i64,
    pub avg_duration_ms: rust_decimal::Decimal,
    pub total_token_count: rust_decimal::Decimal,
    pub avg_errors_before: rust_decimal::Decimal,
    pub avg_errors_after: rust_decimal::Decimal,
}
pub struct PhaseStatsRowBorrowed<'a> {
    pub phase: &'a str,
    pub event_count: i64,
    pub avg_duration_ms: rust_decimal::Decimal,
    pub total_token_count: rust_decimal::Decimal,
    pub avg_errors_before: rust_decimal::Decimal,
    pub avg_errors_after: rust_decimal::Decimal,
}
impl<'a> From<PhaseStatsRowBorrowed<'a>> for PhaseStatsRow {
    fn from(
        PhaseStatsRowBorrowed {
            phase,
            event_count,
            avg_duration_ms,
            total_token_count,
            avg_errors_before,
            avg_errors_after,
        }: PhaseStatsRowBorrowed<'a>,
    ) -> Self {
        Self {
            phase: phase.into(),
            event_count,
            avg_duration_ms,
            total_token_count,
            avg_errors_before,
            avg_errors_after,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RuleInfluenceRow {
    pub id: String,
    pub rule_id: String,
    pub task_run_id: String,
    pub workflow_id: String,
    pub influence_type: String,
    pub evidence: Option<String>,
    pub phase: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct RuleInfluenceRowBorrowed<'a> {
    pub id: &'a str,
    pub rule_id: &'a str,
    pub task_run_id: &'a str,
    pub workflow_id: &'a str,
    pub influence_type: &'a str,
    pub evidence: Option<&'a str>,
    pub phase: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<RuleInfluenceRowBorrowed<'a>> for RuleInfluenceRow {
    fn from(
        RuleInfluenceRowBorrowed {
            id,
            rule_id,
            task_run_id,
            workflow_id,
            influence_type,
            evidence,
            phase,
            created_at,
        }: RuleInfluenceRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            rule_id: rule_id.into(),
            task_run_id: task_run_id.into(),
            workflow_id: workflow_id.into(),
            influence_type: influence_type.into(),
            evidence: evidence.map(|v| v.into()),
            phase: phase.into(),
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IneffectiveRuleRow {
    pub rule_id: String,
    pub no_effect_count: i64,
    pub prevented_error_count: i64,
    pub total_loaded_count: i64,
}
pub struct IneffectiveRuleRowBorrowed<'a> {
    pub rule_id: &'a str,
    pub no_effect_count: i64,
    pub prevented_error_count: i64,
    pub total_loaded_count: i64,
}
impl<'a> From<IneffectiveRuleRowBorrowed<'a>> for IneffectiveRuleRow {
    fn from(
        IneffectiveRuleRowBorrowed {
            rule_id,
            no_effect_count,
            prevented_error_count,
            total_loaded_count,
        }: IneffectiveRuleRowBorrowed<'a>,
    ) -> Self {
        Self {
            rule_id: rule_id.into(),
            no_effect_count,
            prevented_error_count,
            total_loaded_count,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CrossRunPatternRow {
    pub id: String,
    pub pattern_type: String,
    pub signature_hash: String,
    pub workflow_name: Option<String>,
    pub occurrence_count: i32,
    pub first_seen_task_run_id: Option<String>,
    pub last_seen_task_run_id: Option<String>,
    pub first_seen_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub last_seen_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub affected_components: Option<String>,
    pub pattern_data: Option<String>,
    pub status: String,
    pub resolved_by_fix_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct CrossRunPatternRowBorrowed<'a> {
    pub id: &'a str,
    pub pattern_type: &'a str,
    pub signature_hash: &'a str,
    pub workflow_name: Option<&'a str>,
    pub occurrence_count: i32,
    pub first_seen_task_run_id: Option<&'a str>,
    pub last_seen_task_run_id: Option<&'a str>,
    pub first_seen_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub last_seen_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub affected_components: Option<&'a str>,
    pub pattern_data: Option<&'a str>,
    pub status: &'a str,
    pub resolved_by_fix_id: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<CrossRunPatternRowBorrowed<'a>> for CrossRunPatternRow {
    fn from(
        CrossRunPatternRowBorrowed {
            id,
            pattern_type,
            signature_hash,
            workflow_name,
            occurrence_count,
            first_seen_task_run_id,
            last_seen_task_run_id,
            first_seen_at,
            last_seen_at,
            affected_components,
            pattern_data,
            status,
            resolved_by_fix_id,
            created_at,
            updated_at,
        }: CrossRunPatternRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            pattern_type: pattern_type.into(),
            signature_hash: signature_hash.into(),
            workflow_name: workflow_name.map(|v| v.into()),
            occurrence_count,
            first_seen_task_run_id: first_seen_task_run_id.map(|v| v.into()),
            last_seen_task_run_id: last_seen_task_run_id.map(|v| v.into()),
            first_seen_at,
            last_seen_at,
            affected_components: affected_components.map(|v| v.into()),
            pattern_data: pattern_data.map(|v| v.into()),
            status: status.into(),
            resolved_by_fix_id: resolved_by_fix_id.map(|v| v.into()),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecurringFindingRow {
    pub workflow_name: Option<String>,
    pub signature_hash: String,
    pub occurrence_count: i64,
}
pub struct RecurringFindingRowBorrowed<'a> {
    pub workflow_name: Option<&'a str>,
    pub signature_hash: &'a str,
    pub occurrence_count: i64,
}
impl<'a> From<RecurringFindingRowBorrowed<'a>> for RecurringFindingRow {
    fn from(
        RecurringFindingRowBorrowed {
            workflow_name,
            signature_hash,
            occurrence_count,
        }: RecurringFindingRowBorrowed<'a>,
    ) -> Self {
        Self {
            workflow_name: workflow_name.map(|v| v.into()),
            signature_hash: signature_hash.into(),
            occurrence_count,
        }
    }
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct WorkflowVersionRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<WorkflowVersionRowBorrowed, tokio_postgres::Error>,
    mapper: fn(WorkflowVersionRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> WorkflowVersionRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(WorkflowVersionRowBorrowed) -> R,
    ) -> WorkflowVersionRowQuery<'c, 'a, 's, C, R, N> {
        WorkflowVersionRowQuery {
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
pub struct StepFindingLinkRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<StepFindingLinkRowBorrowed, tokio_postgres::Error>,
    mapper: fn(StepFindingLinkRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> StepFindingLinkRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(StepFindingLinkRowBorrowed) -> R,
    ) -> StepFindingLinkRowQuery<'c, 'a, 's, C, R, N> {
        StepFindingLinkRowQuery {
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
pub struct StepProvenanceRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<StepProvenanceRowBorrowed, tokio_postgres::Error>,
    mapper: fn(StepProvenanceRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> StepProvenanceRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(StepProvenanceRowBorrowed) -> R,
    ) -> StepProvenanceRowQuery<'c, 'a, 's, C, R, N> {
        StepProvenanceRowQuery {
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
pub struct PipelineEventRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<PipelineEventRowBorrowed, tokio_postgres::Error>,
    mapper: fn(PipelineEventRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> PipelineEventRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(PipelineEventRowBorrowed) -> R,
    ) -> PipelineEventRowQuery<'c, 'a, 's, C, R, N> {
        PipelineEventRowQuery {
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
pub struct PhaseStatsRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<PhaseStatsRowBorrowed, tokio_postgres::Error>,
    mapper: fn(PhaseStatsRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> PhaseStatsRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(PhaseStatsRowBorrowed) -> R,
    ) -> PhaseStatsRowQuery<'c, 'a, 's, C, R, N> {
        PhaseStatsRowQuery {
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
pub struct RuleInfluenceRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<RuleInfluenceRowBorrowed, tokio_postgres::Error>,
    mapper: fn(RuleInfluenceRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> RuleInfluenceRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(RuleInfluenceRowBorrowed) -> R,
    ) -> RuleInfluenceRowQuery<'c, 'a, 's, C, R, N> {
        RuleInfluenceRowQuery {
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
pub struct IneffectiveRuleRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<IneffectiveRuleRowBorrowed, tokio_postgres::Error>,
    mapper: fn(IneffectiveRuleRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> IneffectiveRuleRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(IneffectiveRuleRowBorrowed) -> R,
    ) -> IneffectiveRuleRowQuery<'c, 'a, 's, C, R, N> {
        IneffectiveRuleRowQuery {
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
pub struct CrossRunPatternRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<CrossRunPatternRowBorrowed, tokio_postgres::Error>,
    mapper: fn(CrossRunPatternRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> CrossRunPatternRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(CrossRunPatternRowBorrowed) -> R,
    ) -> CrossRunPatternRowQuery<'c, 'a, 's, C, R, N> {
        CrossRunPatternRowQuery {
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
pub struct RecurringFindingRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<RecurringFindingRowBorrowed, tokio_postgres::Error>,
    mapper: fn(RecurringFindingRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> RecurringFindingRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(RecurringFindingRowBorrowed) -> R,
    ) -> RecurringFindingRowQuery<'c, 'a, 's, C, R, N> {
        RecurringFindingRowQuery {
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
pub struct InsertWorkflowVersionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_workflow_version() -> InsertWorkflowVersionStmt {
    InsertWorkflowVersionStmt(
        "INSERT INTO workflow_versions (id, workflow_id, version_number, parent_version_id, generation_task_run_id, workflow_json, diff_summary, diff_json, trigger, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())",
        None,
    )
}
impl InsertWorkflowVersionStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<
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
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        workflow_id: &'a T2,
        version_number: &'a i32,
        parent_version_id: &'a T3,
        generation_task_run_id: &'a T4,
        workflow_json: &'a T5,
        diff_summary: &'a T6,
        diff_json: &'a T7,
        trigger: &'a T8,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    workflow_id,
                    version_number,
                    parent_version_id,
                    generation_task_run_id,
                    workflow_json,
                    diff_summary,
                    diff_json,
                    trigger,
                ],
            )
            .await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        InsertWorkflowVersionParams<T1, T2, T3, T4, T5, T6, T7, T8>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for InsertWorkflowVersionStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a InsertWorkflowVersionParams<T1, T2, T3, T4, T5, T6, T7, T8>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.workflow_id,
            &params.version_number,
            &params.parent_version_id,
            &params.generation_task_run_id,
            &params.workflow_json,
            &params.diff_summary,
            &params.diff_json,
            &params.trigger,
        ))
    }
}
pub struct GetWorkflowVersionsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_workflow_versions() -> GetWorkflowVersionsStmt {
    GetWorkflowVersionsStmt(
        "SELECT id, workflow_id, version_number, parent_version_id, generation_task_run_id, workflow_json, diff_summary, diff_json, trigger, created_at FROM workflow_versions WHERE workflow_id = $1 ORDER BY version_number DESC",
        None,
    )
}
impl GetWorkflowVersionsStmt {
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
        workflow_id: &'a T1,
    ) -> WorkflowVersionRowQuery<'c, 'a, 's, C, WorkflowVersionRow, 1> {
        WorkflowVersionRowQuery {
            client,
            params: [workflow_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<WorkflowVersionRowBorrowed, tokio_postgres::Error> {
                Ok(WorkflowVersionRowBorrowed {
                    id: row.try_get(0)?,
                    workflow_id: row.try_get(1)?,
                    version_number: row.try_get(2)?,
                    parent_version_id: row.try_get(3)?,
                    generation_task_run_id: row.try_get(4)?,
                    workflow_json: row.try_get(5)?,
                    diff_summary: row.try_get(6)?,
                    diff_json: row.try_get(7)?,
                    trigger: row.try_get(8)?,
                    created_at: row.try_get(9)?,
                })
            },
            mapper: |it| WorkflowVersionRow::from(it),
        }
    }
}
pub struct GetLatestWorkflowVersionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_latest_workflow_version() -> GetLatestWorkflowVersionStmt {
    GetLatestWorkflowVersionStmt(
        "SELECT id, workflow_id, version_number, parent_version_id, generation_task_run_id, workflow_json, diff_summary, diff_json, trigger, created_at FROM workflow_versions WHERE workflow_id = $1 ORDER BY version_number DESC LIMIT 1",
        None,
    )
}
impl GetLatestWorkflowVersionStmt {
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
        workflow_id: &'a T1,
    ) -> WorkflowVersionRowQuery<'c, 'a, 's, C, WorkflowVersionRow, 1> {
        WorkflowVersionRowQuery {
            client,
            params: [workflow_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<WorkflowVersionRowBorrowed, tokio_postgres::Error> {
                Ok(WorkflowVersionRowBorrowed {
                    id: row.try_get(0)?,
                    workflow_id: row.try_get(1)?,
                    version_number: row.try_get(2)?,
                    parent_version_id: row.try_get(3)?,
                    generation_task_run_id: row.try_get(4)?,
                    workflow_json: row.try_get(5)?,
                    diff_summary: row.try_get(6)?,
                    diff_json: row.try_get(7)?,
                    trigger: row.try_get(8)?,
                    created_at: row.try_get(9)?,
                })
            },
            mapper: |it| WorkflowVersionRow::from(it),
        }
    }
}
pub struct InsertStepFindingLinkStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_step_finding_link() -> InsertStepFindingLinkStmt {
    InsertStepFindingLinkStmt(
        "INSERT INTO step_finding_links (id, task_run_id, step_name, step_index, finding_id, link_type, confidence, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
        None,
    )
}
impl InsertStepFindingLinkStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<
        'c,
        'a,
        's,
        C: GenericClient,
        T1: crate::StringSql,
        T2: crate::StringSql,
        T3: crate::StringSql,
        T4: crate::StringSql,
        T5: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        task_run_id: &'a T2,
        step_name: &'a T3,
        step_index: &'a i32,
        finding_id: &'a T4,
        link_type: &'a T5,
        confidence: &'a f64,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    task_run_id,
                    step_name,
                    step_index,
                    finding_id,
                    link_type,
                    confidence,
                ],
            )
            .await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        InsertStepFindingLinkParams<T1, T2, T3, T4, T5>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for InsertStepFindingLinkStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a InsertStepFindingLinkParams<T1, T2, T3, T4, T5>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.step_name,
            &params.step_index,
            &params.finding_id,
            &params.link_type,
            &params.confidence,
        ))
    }
}
pub struct GetFindingsForStepStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_findings_for_step() -> GetFindingsForStepStmt {
    GetFindingsForStepStmt(
        "SELECT id, task_run_id, step_name, step_index, finding_id, link_type, confidence, created_at FROM step_finding_links WHERE task_run_id = $1 AND step_name = $2",
        None,
    )
}
impl GetFindingsForStepStmt {
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
        task_run_id: &'a T1,
        step_name: &'a T2,
    ) -> StepFindingLinkRowQuery<'c, 'a, 's, C, StepFindingLinkRow, 2> {
        StepFindingLinkRowQuery {
            client,
            params: [task_run_id, step_name],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<StepFindingLinkRowBorrowed, tokio_postgres::Error> {
                Ok(StepFindingLinkRowBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    step_name: row.try_get(2)?,
                    step_index: row.try_get(3)?,
                    finding_id: row.try_get(4)?,
                    link_type: row.try_get(5)?,
                    confidence: row.try_get(6)?,
                    created_at: row.try_get(7)?,
                })
            },
            mapper: |it| StepFindingLinkRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetFindingsForStepParams<T1, T2>,
        StepFindingLinkRowQuery<'c, 'a, 's, C, StepFindingLinkRow, 2>,
        C,
    > for GetFindingsForStepStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetFindingsForStepParams<T1, T2>,
    ) -> StepFindingLinkRowQuery<'c, 'a, 's, C, StepFindingLinkRow, 2> {
        self.bind(client, &params.task_run_id, &params.step_name)
    }
}
pub struct GetStepsForFindingStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_steps_for_finding() -> GetStepsForFindingStmt {
    GetStepsForFindingStmt(
        "SELECT id, task_run_id, step_name, step_index, finding_id, link_type, confidence, created_at FROM step_finding_links WHERE finding_id = $1",
        None,
    )
}
impl GetStepsForFindingStmt {
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
        finding_id: &'a T1,
    ) -> StepFindingLinkRowQuery<'c, 'a, 's, C, StepFindingLinkRow, 1> {
        StepFindingLinkRowQuery {
            client,
            params: [finding_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<StepFindingLinkRowBorrowed, tokio_postgres::Error> {
                Ok(StepFindingLinkRowBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    step_name: row.try_get(2)?,
                    step_index: row.try_get(3)?,
                    finding_id: row.try_get(4)?,
                    link_type: row.try_get(5)?,
                    confidence: row.try_get(6)?,
                    created_at: row.try_get(7)?,
                })
            },
            mapper: |it| StepFindingLinkRow::from(it),
        }
    }
}
pub struct InsertStepProvenanceStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_step_provenance() -> InsertStepProvenanceStmt {
    InsertStepProvenanceStmt(
        "INSERT INTO step_provenance (id, workflow_id, workflow_version_id, step_name, step_index, phase, generating_agent, generation_iteration, original_step_json, final_step_json, ui_bridge_event_ids, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())",
        None,
    )
}
impl InsertStepProvenanceStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<
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
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        workflow_id: &'a T2,
        workflow_version_id: &'a T3,
        step_name: &'a T4,
        step_index: &'a i32,
        phase: &'a T5,
        generating_agent: &'a T6,
        generation_iteration: &'a i32,
        original_step_json: &'a T7,
        final_step_json: &'a T8,
        ui_bridge_event_ids: &'a T9,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    workflow_id,
                    workflow_version_id,
                    step_name,
                    step_index,
                    phase,
                    generating_agent,
                    generation_iteration,
                    original_step_json,
                    final_step_json,
                    ui_bridge_event_ids,
                ],
            )
            .await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        InsertStepProvenanceParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for InsertStepProvenanceStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a InsertStepProvenanceParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.workflow_id,
            &params.workflow_version_id,
            &params.step_name,
            &params.step_index,
            &params.phase,
            &params.generating_agent,
            &params.generation_iteration,
            &params.original_step_json,
            &params.final_step_json,
            &params.ui_bridge_event_ids,
        ))
    }
}
pub struct GetProvenanceForWorkflowStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_provenance_for_workflow() -> GetProvenanceForWorkflowStmt {
    GetProvenanceForWorkflowStmt(
        "SELECT id, workflow_id, workflow_version_id, step_name, step_index, phase, generating_agent, generation_iteration, original_step_json, final_step_json, ui_bridge_event_ids, created_at FROM step_provenance WHERE workflow_id = $1 ORDER BY created_at DESC",
        None,
    )
}
impl GetProvenanceForWorkflowStmt {
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
        workflow_id: &'a T1,
    ) -> StepProvenanceRowQuery<'c, 'a, 's, C, StepProvenanceRow, 1> {
        StepProvenanceRowQuery {
            client,
            params: [workflow_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<StepProvenanceRowBorrowed, tokio_postgres::Error> {
                Ok(StepProvenanceRowBorrowed {
                    id: row.try_get(0)?,
                    workflow_id: row.try_get(1)?,
                    workflow_version_id: row.try_get(2)?,
                    step_name: row.try_get(3)?,
                    step_index: row.try_get(4)?,
                    phase: row.try_get(5)?,
                    generating_agent: row.try_get(6)?,
                    generation_iteration: row.try_get(7)?,
                    original_step_json: row.try_get(8)?,
                    final_step_json: row.try_get(9)?,
                    ui_bridge_event_ids: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                })
            },
            mapper: |it| StepProvenanceRow::from(it),
        }
    }
}
pub struct GetProvenanceByAgentStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_provenance_by_agent() -> GetProvenanceByAgentStmt {
    GetProvenanceByAgentStmt(
        "SELECT id, workflow_id, workflow_version_id, step_name, step_index, phase, generating_agent, generation_iteration, original_step_json, final_step_json, ui_bridge_event_ids, created_at FROM step_provenance WHERE generating_agent = $1 ORDER BY created_at DESC LIMIT $2",
        None,
    )
}
impl GetProvenanceByAgentStmt {
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
        generating_agent: &'a T1,
        limit: &'a i64,
    ) -> StepProvenanceRowQuery<'c, 'a, 's, C, StepProvenanceRow, 2> {
        StepProvenanceRowQuery {
            client,
            params: [generating_agent, limit],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<StepProvenanceRowBorrowed, tokio_postgres::Error> {
                Ok(StepProvenanceRowBorrowed {
                    id: row.try_get(0)?,
                    workflow_id: row.try_get(1)?,
                    workflow_version_id: row.try_get(2)?,
                    step_name: row.try_get(3)?,
                    step_index: row.try_get(4)?,
                    phase: row.try_get(5)?,
                    generating_agent: row.try_get(6)?,
                    generation_iteration: row.try_get(7)?,
                    original_step_json: row.try_get(8)?,
                    final_step_json: row.try_get(9)?,
                    ui_bridge_event_ids: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                })
            },
            mapper: |it| StepProvenanceRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetProvenanceByAgentParams<T1>,
        StepProvenanceRowQuery<'c, 'a, 's, C, StepProvenanceRow, 2>,
        C,
    > for GetProvenanceByAgentStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetProvenanceByAgentParams<T1>,
    ) -> StepProvenanceRowQuery<'c, 'a, 's, C, StepProvenanceRow, 2> {
        self.bind(client, &params.generating_agent, &params.limit)
    }
}
pub struct InsertPipelineEventStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_pipeline_event() -> InsertPipelineEventStmt {
    InsertPipelineEventStmt(
        "INSERT INTO generation_pipeline_events (id, task_run_id, workflow_id, event_type, phase, iteration, payload, duration_ms, token_count, validation_errors_before, validation_errors_after, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())",
        None,
    )
}
impl InsertPipelineEventStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<
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
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        task_run_id: &'a T2,
        workflow_id: &'a T3,
        event_type: &'a T4,
        phase: &'a T5,
        iteration: &'a i32,
        payload: &'a T6,
        duration_ms: &'a i64,
        token_count: &'a i64,
        validation_errors_before: &'a i32,
        validation_errors_after: &'a i32,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    task_run_id,
                    workflow_id,
                    event_type,
                    phase,
                    iteration,
                    payload,
                    duration_ms,
                    token_count,
                    validation_errors_before,
                    validation_errors_after,
                ],
            )
            .await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        InsertPipelineEventParams<T1, T2, T3, T4, T5, T6>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for InsertPipelineEventStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a InsertPipelineEventParams<T1, T2, T3, T4, T5, T6>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.workflow_id,
            &params.event_type,
            &params.phase,
            &params.iteration,
            &params.payload,
            &params.duration_ms,
            &params.token_count,
            &params.validation_errors_before,
            &params.validation_errors_after,
        ))
    }
}
pub struct GetPipelineEventsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_pipeline_events() -> GetPipelineEventsStmt {
    GetPipelineEventsStmt(
        "SELECT id, task_run_id, workflow_id, event_type, phase, iteration, payload, duration_ms, token_count, validation_errors_before, validation_errors_after, created_at FROM generation_pipeline_events WHERE task_run_id = $1 ORDER BY created_at",
        None,
    )
}
impl GetPipelineEventsStmt {
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
    ) -> PipelineEventRowQuery<'c, 'a, 's, C, PipelineEventRow, 1> {
        PipelineEventRowQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<PipelineEventRowBorrowed, tokio_postgres::Error> {
                Ok(PipelineEventRowBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    workflow_id: row.try_get(2)?,
                    event_type: row.try_get(3)?,
                    phase: row.try_get(4)?,
                    iteration: row.try_get(5)?,
                    payload: row.try_get(6)?,
                    duration_ms: row.try_get(7)?,
                    token_count: row.try_get(8)?,
                    validation_errors_before: row.try_get(9)?,
                    validation_errors_after: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                })
            },
            mapper: |it| PipelineEventRow::from(it),
        }
    }
}
pub struct GetPhaseStatsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_phase_stats() -> GetPhaseStatsStmt {
    GetPhaseStatsStmt(
        "SELECT phase, COUNT(*) as event_count, AVG(duration_ms) as avg_duration_ms, SUM(COALESCE(token_count, 0)) as total_token_count, AVG(validation_errors_before) as avg_errors_before, AVG(validation_errors_after) as avg_errors_after FROM generation_pipeline_events WHERE workflow_id = $1 AND event_type = 'phase_end' GROUP BY phase",
        None,
    )
}
impl GetPhaseStatsStmt {
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
        workflow_id: &'a T1,
    ) -> PhaseStatsRowQuery<'c, 'a, 's, C, PhaseStatsRow, 1> {
        PhaseStatsRowQuery {
            client,
            params: [workflow_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<PhaseStatsRowBorrowed, tokio_postgres::Error> {
                    Ok(PhaseStatsRowBorrowed {
                        phase: row.try_get(0)?,
                        event_count: row.try_get(1)?,
                        avg_duration_ms: row.try_get(2)?,
                        total_token_count: row.try_get(3)?,
                        avg_errors_before: row.try_get(4)?,
                        avg_errors_after: row.try_get(5)?,
                    })
                },
            mapper: |it| PhaseStatsRow::from(it),
        }
    }
}
pub struct InsertRuleInfluenceStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_rule_influence() -> InsertRuleInfluenceStmt {
    InsertRuleInfluenceStmt(
        "INSERT INTO rule_influence_log (id, rule_id, task_run_id, workflow_id, influence_type, evidence, phase, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
        None,
    )
}
impl InsertRuleInfluenceStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<
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
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        rule_id: &'a T2,
        task_run_id: &'a T3,
        workflow_id: &'a T4,
        influence_type: &'a T5,
        evidence: &'a T6,
        phase: &'a T7,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    rule_id,
                    task_run_id,
                    workflow_id,
                    influence_type,
                    evidence,
                    phase,
                ],
            )
            .await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        InsertRuleInfluenceParams<T1, T2, T3, T4, T5, T6, T7>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for InsertRuleInfluenceStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a InsertRuleInfluenceParams<T1, T2, T3, T4, T5, T6, T7>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.rule_id,
            &params.task_run_id,
            &params.workflow_id,
            &params.influence_type,
            &params.evidence,
            &params.phase,
        ))
    }
}
pub struct GetRuleInfluencesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_rule_influences() -> GetRuleInfluencesStmt {
    GetRuleInfluencesStmt(
        "SELECT id, rule_id, task_run_id, workflow_id, influence_type, evidence, phase, created_at FROM rule_influence_log WHERE rule_id = $1 ORDER BY created_at DESC",
        None,
    )
}
impl GetRuleInfluencesStmt {
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
        rule_id: &'a T1,
    ) -> RuleInfluenceRowQuery<'c, 'a, 's, C, RuleInfluenceRow, 1> {
        RuleInfluenceRowQuery {
            client,
            params: [rule_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<RuleInfluenceRowBorrowed, tokio_postgres::Error> {
                Ok(RuleInfluenceRowBorrowed {
                    id: row.try_get(0)?,
                    rule_id: row.try_get(1)?,
                    task_run_id: row.try_get(2)?,
                    workflow_id: row.try_get(3)?,
                    influence_type: row.try_get(4)?,
                    evidence: row.try_get(5)?,
                    phase: row.try_get(6)?,
                    created_at: row.try_get(7)?,
                })
            },
            mapper: |it| RuleInfluenceRow::from(it),
        }
    }
}
pub struct GetIneffectiveRulesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_ineffective_rules() -> GetIneffectiveRulesStmt {
    GetIneffectiveRulesStmt(
        "SELECT rule_id, COUNT(*) FILTER (WHERE influence_type = 'no_effect') as no_effect_count, COUNT(*) FILTER (WHERE influence_type = 'prevented_error') as prevented_error_count, COUNT(*) as total_loaded_count FROM rule_influence_log GROUP BY rule_id HAVING COUNT(*) FILTER (WHERE influence_type = 'no_effect') >= $1 AND COUNT(*) FILTER (WHERE influence_type = 'prevented_error') = 0",
        None,
    )
}
impl GetIneffectiveRulesStmt {
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
        min_no_effect_count: &'a i64,
    ) -> IneffectiveRuleRowQuery<'c, 'a, 's, C, IneffectiveRuleRow, 1> {
        IneffectiveRuleRowQuery {
            client,
            params: [min_no_effect_count],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<IneffectiveRuleRowBorrowed, tokio_postgres::Error> {
                Ok(IneffectiveRuleRowBorrowed {
                    rule_id: row.try_get(0)?,
                    no_effect_count: row.try_get(1)?,
                    prevented_error_count: row.try_get(2)?,
                    total_loaded_count: row.try_get(3)?,
                })
            },
            mapper: |it| IneffectiveRuleRow::from(it),
        }
    }
}
pub struct UpsertCrossRunPatternStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn upsert_cross_run_pattern() -> UpsertCrossRunPatternStmt {
    UpsertCrossRunPatternStmt(
        "INSERT INTO cross_run_patterns (id, pattern_type, signature_hash, workflow_name, first_seen_task_run_id, last_seen_task_run_id, first_seen_at, last_seen_at, affected_components, pattern_data, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $5, NOW(), NOW(), $6, $7, NOW(), NOW()) ON CONFLICT (pattern_type, signature_hash) DO UPDATE SET occurrence_count = cross_run_patterns.occurrence_count + 1, last_seen_task_run_id = $5, last_seen_at = NOW(), affected_components = COALESCE($6, cross_run_patterns.affected_components), pattern_data = COALESCE($7, cross_run_patterns.pattern_data), updated_at = NOW()",
        None,
    )
}
impl UpsertCrossRunPatternStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<
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
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        pattern_type: &'a T2,
        signature_hash: &'a T3,
        workflow_name: &'a T4,
        task_run_id: &'a T5,
        affected_components: &'a T6,
        pattern_data: &'a T7,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    pattern_type,
                    signature_hash,
                    workflow_name,
                    task_run_id,
                    affected_components,
                    pattern_data,
                ],
            )
            .await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        UpsertCrossRunPatternParams<T1, T2, T3, T4, T5, T6, T7>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for UpsertCrossRunPatternStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a UpsertCrossRunPatternParams<T1, T2, T3, T4, T5, T6, T7>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.pattern_type,
            &params.signature_hash,
            &params.workflow_name,
            &params.task_run_id,
            &params.affected_components,
            &params.pattern_data,
        ))
    }
}
pub struct GetActivePatternsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_active_patterns() -> GetActivePatternsStmt {
    GetActivePatternsStmt(
        "SELECT id, pattern_type, signature_hash, workflow_name, occurrence_count, first_seen_task_run_id, last_seen_task_run_id, first_seen_at, last_seen_at, affected_components, pattern_data, status, resolved_by_fix_id, created_at, updated_at FROM cross_run_patterns WHERE status = 'active' AND (workflow_name = $1 OR $1 IS NULL) ORDER BY occurrence_count DESC",
        None,
    )
}
impl GetActivePatternsStmt {
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
        workflow_name: &'a T1,
    ) -> CrossRunPatternRowQuery<'c, 'a, 's, C, CrossRunPatternRow, 1> {
        CrossRunPatternRowQuery {
            client,
            params: [workflow_name],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<CrossRunPatternRowBorrowed, tokio_postgres::Error> {
                Ok(CrossRunPatternRowBorrowed {
                    id: row.try_get(0)?,
                    pattern_type: row.try_get(1)?,
                    signature_hash: row.try_get(2)?,
                    workflow_name: row.try_get(3)?,
                    occurrence_count: row.try_get(4)?,
                    first_seen_task_run_id: row.try_get(5)?,
                    last_seen_task_run_id: row.try_get(6)?,
                    first_seen_at: row.try_get(7)?,
                    last_seen_at: row.try_get(8)?,
                    affected_components: row.try_get(9)?,
                    pattern_data: row.try_get(10)?,
                    status: row.try_get(11)?,
                    resolved_by_fix_id: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    updated_at: row.try_get(14)?,
                })
            },
            mapper: |it| CrossRunPatternRow::from(it),
        }
    }
}
pub struct ResolvePatternStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn resolve_pattern() -> ResolvePatternStmt {
    ResolvePatternStmt(
        "UPDATE cross_run_patterns SET status = 'resolved', resolved_by_fix_id = $1, updated_at = NOW() WHERE id = $2",
        None,
    )
}
impl ResolvePatternStmt {
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
        fix_id: &'a T1,
        id: &'a T2,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[fix_id, id]).await
    }
}
impl<'a, C: GenericClient + Send + Sync, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        ResolvePatternParams<T1, T2>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for ResolvePatternStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a ResolvePatternParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(client, &params.fix_id, &params.id))
    }
}
pub struct DetectRecurringFindingsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn detect_recurring_findings() -> DetectRecurringFindingsStmt {
    DetectRecurringFindingsStmt(
        "SELECT $1 as workflow_name, f.signature_hash, COUNT(DISTINCT f.task_run_id) as occurrence_count FROM task_run_findings f JOIN task_runs t ON f.task_run_id = t.id WHERE t.workflow_name = $1 AND f.signature_hash IS NOT NULL GROUP BY f.signature_hash HAVING COUNT(DISTINCT f.task_run_id) >= $2",
        None,
    )
}
impl DetectRecurringFindingsStmt {
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
        workflow_name: &'a T1,
        min_occurrences: &'a i64,
    ) -> RecurringFindingRowQuery<'c, 'a, 's, C, RecurringFindingRow, 2> {
        RecurringFindingRowQuery {
            client,
            params: [workflow_name, min_occurrences],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<RecurringFindingRowBorrowed, tokio_postgres::Error> {
                Ok(RecurringFindingRowBorrowed {
                    workflow_name: row.try_get(0)?,
                    signature_hash: row.try_get(1)?,
                    occurrence_count: row.try_get(2)?,
                })
            },
            mapper: |it| RecurringFindingRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        DetectRecurringFindingsParams<T1>,
        RecurringFindingRowQuery<'c, 'a, 's, C, RecurringFindingRow, 2>,
        C,
    > for DetectRecurringFindingsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a DetectRecurringFindingsParams<T1>,
    ) -> RecurringFindingRowQuery<'c, 'a, 's, C, RecurringFindingRow, 2> {
        self.bind(client, &params.workflow_name, &params.min_occurrences)
    }
}
