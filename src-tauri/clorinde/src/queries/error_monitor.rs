// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct GetUnresolvedErrorsParams<T1: crate::StringSql> {
    pub task_run_id: Option<T1>,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct GetUnresolvedErrorsForTaskParams<T1: crate::StringSql> {
    pub task_run_id: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct UpdateErrorStatusParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub status: T1,
    pub resolution_notes: Option<T2>,
    pub acknowledged_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub resolved_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub id: i64,
}
#[derive(Debug)]
pub struct MarkResolvedByTaskParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub task_run_id: T1,
    pub resolution_notes: Option<T2>,
    pub id: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetUnresolvedErrors {
    pub id: i64,
    pub log_source_id: i64,
    pub log_source_name: String,
    pub task_run_id: String,
    pub workflow_step_id: String,
    pub log_timestamp: String,
    pub captured_at: String,
    pub severity: String,
    pub error_type: String,
    pub error_code: String,
    pub message: String,
    pub stack_trace: String,
    pub context_lines: String,
    pub raw_entry: String,
    pub file_path: String,
    pub line_number: i32,
    pub column_number: i32,
    pub function_name: String,
    pub signature_hash: String,
    pub occurrence_count: i32,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub status: String,
    pub finding_id: String,
    pub resolved_by_task_run_id: String,
    pub resolution_notes: String,
    pub acknowledged_at: String,
    pub resolved_at: String,
    pub workflow_name: String,
    pub trace_id: String,
}
pub struct GetUnresolvedErrorsBorrowed<'a> {
    pub id: i64,
    pub log_source_id: i64,
    pub log_source_name: &'a str,
    pub task_run_id: &'a str,
    pub workflow_step_id: &'a str,
    pub log_timestamp: &'a str,
    pub captured_at: &'a str,
    pub severity: &'a str,
    pub error_type: &'a str,
    pub error_code: &'a str,
    pub message: &'a str,
    pub stack_trace: &'a str,
    pub context_lines: &'a str,
    pub raw_entry: &'a str,
    pub file_path: &'a str,
    pub line_number: i32,
    pub column_number: i32,
    pub function_name: &'a str,
    pub signature_hash: &'a str,
    pub occurrence_count: i32,
    pub first_seen_at: &'a str,
    pub last_seen_at: &'a str,
    pub status: &'a str,
    pub finding_id: &'a str,
    pub resolved_by_task_run_id: &'a str,
    pub resolution_notes: &'a str,
    pub acknowledged_at: &'a str,
    pub resolved_at: &'a str,
    pub workflow_name: &'a str,
    pub trace_id: &'a str,
}
impl<'a> From<GetUnresolvedErrorsBorrowed<'a>> for GetUnresolvedErrors {
    fn from(
        GetUnresolvedErrorsBorrowed {
            id,
            log_source_id,
            log_source_name,
            task_run_id,
            workflow_step_id,
            log_timestamp,
            captured_at,
            severity,
            error_type,
            error_code,
            message,
            stack_trace,
            context_lines,
            raw_entry,
            file_path,
            line_number,
            column_number,
            function_name,
            signature_hash,
            occurrence_count,
            first_seen_at,
            last_seen_at,
            status,
            finding_id,
            resolved_by_task_run_id,
            resolution_notes,
            acknowledged_at,
            resolved_at,
            workflow_name,
            trace_id,
        }: GetUnresolvedErrorsBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            log_source_id,
            log_source_name: log_source_name.into(),
            task_run_id: task_run_id.into(),
            workflow_step_id: workflow_step_id.into(),
            log_timestamp: log_timestamp.into(),
            captured_at: captured_at.into(),
            severity: severity.into(),
            error_type: error_type.into(),
            error_code: error_code.into(),
            message: message.into(),
            stack_trace: stack_trace.into(),
            context_lines: context_lines.into(),
            raw_entry: raw_entry.into(),
            file_path: file_path.into(),
            line_number,
            column_number,
            function_name: function_name.into(),
            signature_hash: signature_hash.into(),
            occurrence_count,
            first_seen_at: first_seen_at.into(),
            last_seen_at: last_seen_at.into(),
            status: status.into(),
            finding_id: finding_id.into(),
            resolved_by_task_run_id: resolved_by_task_run_id.into(),
            resolution_notes: resolution_notes.into(),
            acknowledged_at: acknowledged_at.into(),
            resolved_at: resolved_at.into(),
            workflow_name: workflow_name.into(),
            trace_id: trace_id.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetUnresolvedErrorsForTask {
    pub id: i64,
    pub log_source_id: i64,
    pub log_source_name: String,
    pub task_run_id: String,
    pub workflow_step_id: String,
    pub log_timestamp: String,
    pub captured_at: String,
    pub severity: String,
    pub error_type: String,
    pub error_code: String,
    pub message: String,
    pub stack_trace: String,
    pub context_lines: String,
    pub raw_entry: String,
    pub file_path: String,
    pub line_number: i32,
    pub column_number: i32,
    pub function_name: String,
    pub signature_hash: String,
    pub occurrence_count: i32,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub status: String,
    pub finding_id: String,
    pub resolved_by_task_run_id: String,
    pub resolution_notes: String,
    pub acknowledged_at: String,
    pub resolved_at: String,
    pub workflow_name: String,
    pub trace_id: String,
}
pub struct GetUnresolvedErrorsForTaskBorrowed<'a> {
    pub id: i64,
    pub log_source_id: i64,
    pub log_source_name: &'a str,
    pub task_run_id: &'a str,
    pub workflow_step_id: &'a str,
    pub log_timestamp: &'a str,
    pub captured_at: &'a str,
    pub severity: &'a str,
    pub error_type: &'a str,
    pub error_code: &'a str,
    pub message: &'a str,
    pub stack_trace: &'a str,
    pub context_lines: &'a str,
    pub raw_entry: &'a str,
    pub file_path: &'a str,
    pub line_number: i32,
    pub column_number: i32,
    pub function_name: &'a str,
    pub signature_hash: &'a str,
    pub occurrence_count: i32,
    pub first_seen_at: &'a str,
    pub last_seen_at: &'a str,
    pub status: &'a str,
    pub finding_id: &'a str,
    pub resolved_by_task_run_id: &'a str,
    pub resolution_notes: &'a str,
    pub acknowledged_at: &'a str,
    pub resolved_at: &'a str,
    pub workflow_name: &'a str,
    pub trace_id: &'a str,
}
impl<'a> From<GetUnresolvedErrorsForTaskBorrowed<'a>> for GetUnresolvedErrorsForTask {
    fn from(
        GetUnresolvedErrorsForTaskBorrowed {
            id,
            log_source_id,
            log_source_name,
            task_run_id,
            workflow_step_id,
            log_timestamp,
            captured_at,
            severity,
            error_type,
            error_code,
            message,
            stack_trace,
            context_lines,
            raw_entry,
            file_path,
            line_number,
            column_number,
            function_name,
            signature_hash,
            occurrence_count,
            first_seen_at,
            last_seen_at,
            status,
            finding_id,
            resolved_by_task_run_id,
            resolution_notes,
            acknowledged_at,
            resolved_at,
            workflow_name,
            trace_id,
        }: GetUnresolvedErrorsForTaskBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            log_source_id,
            log_source_name: log_source_name.into(),
            task_run_id: task_run_id.into(),
            workflow_step_id: workflow_step_id.into(),
            log_timestamp: log_timestamp.into(),
            captured_at: captured_at.into(),
            severity: severity.into(),
            error_type: error_type.into(),
            error_code: error_code.into(),
            message: message.into(),
            stack_trace: stack_trace.into(),
            context_lines: context_lines.into(),
            raw_entry: raw_entry.into(),
            file_path: file_path.into(),
            line_number,
            column_number,
            function_name: function_name.into(),
            signature_hash: signature_hash.into(),
            occurrence_count,
            first_seen_at: first_seen_at.into(),
            last_seen_at: last_seen_at.into(),
            status: status.into(),
            finding_id: finding_id.into(),
            resolved_by_task_run_id: resolved_by_task_run_id.into(),
            resolution_notes: resolution_notes.into(),
            acknowledged_at: acknowledged_at.into(),
            resolved_at: resolved_at.into(),
            workflow_name: workflow_name.into(),
            trace_id: trace_id.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct GetErrorSummary {
    pub total: i32,
    pub new_count: i32,
    pub unresolved_count: i32,
    pub critical_count: i32,
    pub error_count: i32,
    pub warning_count: i32,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetErrorSummaryBySource {
    pub log_source_name: String,
    pub cnt: i64,
}
pub struct GetErrorSummaryBySourceBorrowed<'a> {
    pub log_source_name: &'a str,
    pub cnt: i64,
}
impl<'a> From<GetErrorSummaryBySourceBorrowed<'a>> for GetErrorSummaryBySource {
    fn from(
        GetErrorSummaryBySourceBorrowed {
            log_source_name,
            cnt,
        }: GetErrorSummaryBySourceBorrowed<'a>,
    ) -> Self {
        Self {
            log_source_name: log_source_name.into(),
            cnt,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetErrorSummaryByType {
    pub error_type: String,
    pub cnt: i64,
}
pub struct GetErrorSummaryByTypeBorrowed<'a> {
    pub error_type: &'a str,
    pub cnt: i64,
}
impl<'a> From<GetErrorSummaryByTypeBorrowed<'a>> for GetErrorSummaryByType {
    fn from(
        GetErrorSummaryByTypeBorrowed { error_type, cnt }: GetErrorSummaryByTypeBorrowed<'a>,
    ) -> Self {
        Self {
            error_type: error_type.into(),
            cnt,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetErrorSummaryByStatus {
    pub status: String,
    pub cnt: i64,
}
pub struct GetErrorSummaryByStatusBorrowed<'a> {
    pub status: &'a str,
    pub cnt: i64,
}
impl<'a> From<GetErrorSummaryByStatusBorrowed<'a>> for GetErrorSummaryByStatus {
    fn from(
        GetErrorSummaryByStatusBorrowed { status, cnt }: GetErrorSummaryByStatusBorrowed<'a>,
    ) -> Self {
        Self {
            status: status.into(),
            cnt,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetErrorDebugContext {
    pub id: i64,
    pub log_source_id: i64,
    pub log_source_name: String,
    pub task_run_id: String,
    pub workflow_step_id: String,
    pub log_timestamp: String,
    pub captured_at: String,
    pub severity: String,
    pub error_type: String,
    pub error_code: String,
    pub message: String,
    pub stack_trace: String,
    pub context_lines: String,
    pub raw_entry: String,
    pub file_path: String,
    pub line_number: i32,
    pub column_number: i32,
    pub function_name: String,
    pub signature_hash: String,
    pub occurrence_count: i32,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub status: String,
    pub finding_id: String,
    pub resolved_by_task_run_id: String,
    pub resolution_notes: String,
    pub acknowledged_at: String,
    pub resolved_at: String,
    pub workflow_name: String,
    pub trace_id: String,
}
pub struct GetErrorDebugContextBorrowed<'a> {
    pub id: i64,
    pub log_source_id: i64,
    pub log_source_name: &'a str,
    pub task_run_id: &'a str,
    pub workflow_step_id: &'a str,
    pub log_timestamp: &'a str,
    pub captured_at: &'a str,
    pub severity: &'a str,
    pub error_type: &'a str,
    pub error_code: &'a str,
    pub message: &'a str,
    pub stack_trace: &'a str,
    pub context_lines: &'a str,
    pub raw_entry: &'a str,
    pub file_path: &'a str,
    pub line_number: i32,
    pub column_number: i32,
    pub function_name: &'a str,
    pub signature_hash: &'a str,
    pub occurrence_count: i32,
    pub first_seen_at: &'a str,
    pub last_seen_at: &'a str,
    pub status: &'a str,
    pub finding_id: &'a str,
    pub resolved_by_task_run_id: &'a str,
    pub resolution_notes: &'a str,
    pub acknowledged_at: &'a str,
    pub resolved_at: &'a str,
    pub workflow_name: &'a str,
    pub trace_id: &'a str,
}
impl<'a> From<GetErrorDebugContextBorrowed<'a>> for GetErrorDebugContext {
    fn from(
        GetErrorDebugContextBorrowed {
            id,
            log_source_id,
            log_source_name,
            task_run_id,
            workflow_step_id,
            log_timestamp,
            captured_at,
            severity,
            error_type,
            error_code,
            message,
            stack_trace,
            context_lines,
            raw_entry,
            file_path,
            line_number,
            column_number,
            function_name,
            signature_hash,
            occurrence_count,
            first_seen_at,
            last_seen_at,
            status,
            finding_id,
            resolved_by_task_run_id,
            resolution_notes,
            acknowledged_at,
            resolved_at,
            workflow_name,
            trace_id,
        }: GetErrorDebugContextBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            log_source_id,
            log_source_name: log_source_name.into(),
            task_run_id: task_run_id.into(),
            workflow_step_id: workflow_step_id.into(),
            log_timestamp: log_timestamp.into(),
            captured_at: captured_at.into(),
            severity: severity.into(),
            error_type: error_type.into(),
            error_code: error_code.into(),
            message: message.into(),
            stack_trace: stack_trace.into(),
            context_lines: context_lines.into(),
            raw_entry: raw_entry.into(),
            file_path: file_path.into(),
            line_number,
            column_number,
            function_name: function_name.into(),
            signature_hash: signature_hash.into(),
            occurrence_count,
            first_seen_at: first_seen_at.into(),
            last_seen_at: last_seen_at.into(),
            status: status.into(),
            finding_id: finding_id.into(),
            resolved_by_task_run_id: resolved_by_task_run_id.into(),
            resolution_notes: resolution_notes.into(),
            acknowledged_at: acknowledged_at.into(),
            resolved_at: resolved_at.into(),
            workflow_name: workflow_name.into(),
            trace_id: trace_id.into(),
        }
    }
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct GetUnresolvedErrorsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetUnresolvedErrorsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetUnresolvedErrorsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetUnresolvedErrorsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetUnresolvedErrorsBorrowed) -> R,
    ) -> GetUnresolvedErrorsQuery<'c, 'a, 's, C, R, N> {
        GetUnresolvedErrorsQuery {
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
pub struct GetUnresolvedErrorsForTaskQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetUnresolvedErrorsForTaskBorrowed, tokio_postgres::Error>,
    mapper: fn(GetUnresolvedErrorsForTaskBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetUnresolvedErrorsForTaskQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetUnresolvedErrorsForTaskBorrowed) -> R,
    ) -> GetUnresolvedErrorsForTaskQuery<'c, 'a, 's, C, R, N> {
        GetUnresolvedErrorsForTaskQuery {
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
pub struct GetErrorSummaryQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetErrorSummary, tokio_postgres::Error>,
    mapper: fn(GetErrorSummary) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetErrorSummaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetErrorSummary) -> R,
    ) -> GetErrorSummaryQuery<'c, 'a, 's, C, R, N> {
        GetErrorSummaryQuery {
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
pub struct GetErrorSummaryBySourceQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetErrorSummaryBySourceBorrowed, tokio_postgres::Error>,
    mapper: fn(GetErrorSummaryBySourceBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetErrorSummaryBySourceQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetErrorSummaryBySourceBorrowed) -> R,
    ) -> GetErrorSummaryBySourceQuery<'c, 'a, 's, C, R, N> {
        GetErrorSummaryBySourceQuery {
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
pub struct GetErrorSummaryByTypeQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetErrorSummaryByTypeBorrowed, tokio_postgres::Error>,
    mapper: fn(GetErrorSummaryByTypeBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetErrorSummaryByTypeQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetErrorSummaryByTypeBorrowed) -> R,
    ) -> GetErrorSummaryByTypeQuery<'c, 'a, 's, C, R, N> {
        GetErrorSummaryByTypeQuery {
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
pub struct GetErrorSummaryByStatusQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetErrorSummaryByStatusBorrowed, tokio_postgres::Error>,
    mapper: fn(GetErrorSummaryByStatusBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetErrorSummaryByStatusQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetErrorSummaryByStatusBorrowed) -> R,
    ) -> GetErrorSummaryByStatusQuery<'c, 'a, 's, C, R, N> {
        GetErrorSummaryByStatusQuery {
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
pub struct GetErrorDebugContextQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetErrorDebugContextBorrowed, tokio_postgres::Error>,
    mapper: fn(GetErrorDebugContextBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetErrorDebugContextQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetErrorDebugContextBorrowed) -> R,
    ) -> GetErrorDebugContextQuery<'c, 'a, 's, C, R, N> {
        GetErrorDebugContextQuery {
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
pub struct GetUnresolvedErrorsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_unresolved_errors() -> GetUnresolvedErrorsStmt {
    GetUnresolvedErrorsStmt(
        "SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id, e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code, e.message, e.stack_trace, e.context_lines, e.raw_entry, e.file_path, e.line_number, e.column_number, e.function_name, e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT, e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes, e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id FROM error_events e LEFT JOIN task_runs tr ON e.task_run_id = tr.id WHERE e.status IN ('new', 'acknowledged', 'in_progress', 'promoted') AND ($1::TEXT IS NULL OR e.task_run_id = $1) ORDER BY CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END, e.occurrence_count DESC, e.last_seen_at DESC LIMIT $2",
        None,
    )
}
impl GetUnresolvedErrorsStmt {
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
        task_run_id: &'a Option<T1>,
        max_results: &'a i64,
    ) -> GetUnresolvedErrorsQuery<'c, 'a, 's, C, GetUnresolvedErrors, 2> {
        GetUnresolvedErrorsQuery {
            client,
            params: [task_run_id, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetUnresolvedErrorsBorrowed, tokio_postgres::Error> {
                Ok(GetUnresolvedErrorsBorrowed {
                    id: row.try_get(0)?,
                    log_source_id: row.try_get(1)?,
                    log_source_name: row.try_get(2)?,
                    task_run_id: row.try_get(3)?,
                    workflow_step_id: row.try_get(4)?,
                    log_timestamp: row.try_get(5)?,
                    captured_at: row.try_get(6)?,
                    severity: row.try_get(7)?,
                    error_type: row.try_get(8)?,
                    error_code: row.try_get(9)?,
                    message: row.try_get(10)?,
                    stack_trace: row.try_get(11)?,
                    context_lines: row.try_get(12)?,
                    raw_entry: row.try_get(13)?,
                    file_path: row.try_get(14)?,
                    line_number: row.try_get(15)?,
                    column_number: row.try_get(16)?,
                    function_name: row.try_get(17)?,
                    signature_hash: row.try_get(18)?,
                    occurrence_count: row.try_get(19)?,
                    first_seen_at: row.try_get(20)?,
                    last_seen_at: row.try_get(21)?,
                    status: row.try_get(22)?,
                    finding_id: row.try_get(23)?,
                    resolved_by_task_run_id: row.try_get(24)?,
                    resolution_notes: row.try_get(25)?,
                    acknowledged_at: row.try_get(26)?,
                    resolved_at: row.try_get(27)?,
                    workflow_name: row.try_get(28)?,
                    trace_id: row.try_get(29)?,
                })
            },
            mapper: |it| GetUnresolvedErrors::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetUnresolvedErrorsParams<T1>,
        GetUnresolvedErrorsQuery<'c, 'a, 's, C, GetUnresolvedErrors, 2>,
        C,
    > for GetUnresolvedErrorsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetUnresolvedErrorsParams<T1>,
    ) -> GetUnresolvedErrorsQuery<'c, 'a, 's, C, GetUnresolvedErrors, 2> {
        self.bind(client, &params.task_run_id, &params.max_results)
    }
}
pub struct GetUnresolvedErrorsForTaskStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_unresolved_errors_for_task() -> GetUnresolvedErrorsForTaskStmt {
    GetUnresolvedErrorsForTaskStmt(
        "SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id, e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code, e.message, e.stack_trace, e.context_lines, e.raw_entry, e.file_path, e.line_number, e.column_number, e.function_name, e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT, e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes, e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id FROM error_events e LEFT JOIN task_runs tr ON e.task_run_id = tr.id WHERE e.status IN ('new', 'acknowledged', 'in_progress', 'promoted') AND e.task_run_id = $1 ORDER BY CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END, e.occurrence_count DESC, e.last_seen_at DESC LIMIT $2",
        None,
    )
}
impl GetUnresolvedErrorsForTaskStmt {
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
        max_results: &'a i64,
    ) -> GetUnresolvedErrorsForTaskQuery<'c, 'a, 's, C, GetUnresolvedErrorsForTask, 2> {
        GetUnresolvedErrorsForTaskQuery {
            client,
            params: [task_run_id, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetUnresolvedErrorsForTaskBorrowed, tokio_postgres::Error> {
                Ok(GetUnresolvedErrorsForTaskBorrowed {
                    id: row.try_get(0)?,
                    log_source_id: row.try_get(1)?,
                    log_source_name: row.try_get(2)?,
                    task_run_id: row.try_get(3)?,
                    workflow_step_id: row.try_get(4)?,
                    log_timestamp: row.try_get(5)?,
                    captured_at: row.try_get(6)?,
                    severity: row.try_get(7)?,
                    error_type: row.try_get(8)?,
                    error_code: row.try_get(9)?,
                    message: row.try_get(10)?,
                    stack_trace: row.try_get(11)?,
                    context_lines: row.try_get(12)?,
                    raw_entry: row.try_get(13)?,
                    file_path: row.try_get(14)?,
                    line_number: row.try_get(15)?,
                    column_number: row.try_get(16)?,
                    function_name: row.try_get(17)?,
                    signature_hash: row.try_get(18)?,
                    occurrence_count: row.try_get(19)?,
                    first_seen_at: row.try_get(20)?,
                    last_seen_at: row.try_get(21)?,
                    status: row.try_get(22)?,
                    finding_id: row.try_get(23)?,
                    resolved_by_task_run_id: row.try_get(24)?,
                    resolution_notes: row.try_get(25)?,
                    acknowledged_at: row.try_get(26)?,
                    resolved_at: row.try_get(27)?,
                    workflow_name: row.try_get(28)?,
                    trace_id: row.try_get(29)?,
                })
            },
            mapper: |it| GetUnresolvedErrorsForTask::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetUnresolvedErrorsForTaskParams<T1>,
        GetUnresolvedErrorsForTaskQuery<'c, 'a, 's, C, GetUnresolvedErrorsForTask, 2>,
        C,
    > for GetUnresolvedErrorsForTaskStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetUnresolvedErrorsForTaskParams<T1>,
    ) -> GetUnresolvedErrorsForTaskQuery<'c, 'a, 's, C, GetUnresolvedErrorsForTask, 2> {
        self.bind(client, &params.task_run_id, &params.max_results)
    }
}
pub struct GetErrorSummaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_error_summary() -> GetErrorSummaryStmt {
    GetErrorSummaryStmt(
        "SELECT COUNT(*)::INTEGER as total, COUNT(*) FILTER (WHERE status = 'new')::INTEGER as new_count, COUNT(*) FILTER (WHERE status IN ('new', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as unresolved_count, COUNT(*) FILTER (WHERE severity = 'critical' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as critical_count, COUNT(*) FILTER (WHERE severity = 'error' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as error_count, COUNT(*) FILTER (WHERE severity = 'warning' AND status IN ('new', 'acknowledged', 'in_progress', 'promoted'))::INTEGER as warning_count FROM error_events WHERE ($1::TEXT IS NULL OR task_run_id = $1)",
        None,
    )
}
impl GetErrorSummaryStmt {
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
        task_run_id: &'a Option<T1>,
    ) -> GetErrorSummaryQuery<'c, 'a, 's, C, GetErrorSummary, 1> {
        GetErrorSummaryQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<GetErrorSummary, tokio_postgres::Error> {
                    Ok(GetErrorSummary {
                        total: row.try_get(0)?,
                        new_count: row.try_get(1)?,
                        unresolved_count: row.try_get(2)?,
                        critical_count: row.try_get(3)?,
                        error_count: row.try_get(4)?,
                        warning_count: row.try_get(5)?,
                    })
                },
            mapper: |it| GetErrorSummary::from(it),
        }
    }
}
pub struct GetErrorSummaryBySourceStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_error_summary_by_source() -> GetErrorSummaryBySourceStmt {
    GetErrorSummaryBySourceStmt(
        "SELECT log_source_name, COUNT(*)::BIGINT as cnt FROM error_events WHERE log_source_name IS NOT NULL AND ($1::TEXT IS NULL OR task_run_id = $1) GROUP BY log_source_name",
        None,
    )
}
impl GetErrorSummaryBySourceStmt {
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
        task_run_id: &'a Option<T1>,
    ) -> GetErrorSummaryBySourceQuery<'c, 'a, 's, C, GetErrorSummaryBySource, 1> {
        GetErrorSummaryBySourceQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetErrorSummaryBySourceBorrowed, tokio_postgres::Error> {
                Ok(GetErrorSummaryBySourceBorrowed {
                    log_source_name: row.try_get(0)?,
                    cnt: row.try_get(1)?,
                })
            },
            mapper: |it| GetErrorSummaryBySource::from(it),
        }
    }
}
pub struct GetErrorSummaryByTypeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_error_summary_by_type() -> GetErrorSummaryByTypeStmt {
    GetErrorSummaryByTypeStmt(
        "SELECT error_type, COUNT(*)::BIGINT as cnt FROM error_events WHERE error_type IS NOT NULL AND ($1::TEXT IS NULL OR task_run_id = $1) GROUP BY error_type",
        None,
    )
}
impl GetErrorSummaryByTypeStmt {
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
        task_run_id: &'a Option<T1>,
    ) -> GetErrorSummaryByTypeQuery<'c, 'a, 's, C, GetErrorSummaryByType, 1> {
        GetErrorSummaryByTypeQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetErrorSummaryByTypeBorrowed, tokio_postgres::Error> {
                Ok(GetErrorSummaryByTypeBorrowed {
                    error_type: row.try_get(0)?,
                    cnt: row.try_get(1)?,
                })
            },
            mapper: |it| GetErrorSummaryByType::from(it),
        }
    }
}
pub struct GetErrorSummaryByStatusStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_error_summary_by_status() -> GetErrorSummaryByStatusStmt {
    GetErrorSummaryByStatusStmt(
        "SELECT status, COUNT(*)::BIGINT as cnt FROM error_events WHERE status IS NOT NULL AND ($1::TEXT IS NULL OR task_run_id = $1) GROUP BY status",
        None,
    )
}
impl GetErrorSummaryByStatusStmt {
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
        task_run_id: &'a Option<T1>,
    ) -> GetErrorSummaryByStatusQuery<'c, 'a, 's, C, GetErrorSummaryByStatus, 1> {
        GetErrorSummaryByStatusQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetErrorSummaryByStatusBorrowed, tokio_postgres::Error> {
                Ok(GetErrorSummaryByStatusBorrowed {
                    status: row.try_get(0)?,
                    cnt: row.try_get(1)?,
                })
            },
            mapper: |it| GetErrorSummaryByStatus::from(it),
        }
    }
}
pub struct UpdateErrorStatusStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_error_status() -> UpdateErrorStatusStmt {
    UpdateErrorStatusStmt(
        "UPDATE error_events SET status = $1, resolution_notes = COALESCE($2, resolution_notes), acknowledged_at = COALESCE($3::TIMESTAMPTZ, acknowledged_at), resolved_at = COALESCE($4::TIMESTAMPTZ, resolved_at) WHERE id = $5",
        None,
    )
}
impl UpdateErrorStatusStmt {
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
        status: &'a T1,
        resolution_notes: &'a Option<T2>,
        acknowledged_at: &'a Option<chrono::DateTime<chrono::FixedOffset>>,
        resolved_at: &'a Option<chrono::DateTime<chrono::FixedOffset>>,
        id: &'a i64,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[status, resolution_notes, acknowledged_at, resolved_at, id],
            )
            .await
    }
}
impl<'a, C: GenericClient + Send + Sync, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        UpdateErrorStatusParams<T1, T2>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for UpdateErrorStatusStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a UpdateErrorStatusParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.status,
            &params.resolution_notes,
            &params.acknowledged_at,
            &params.resolved_at,
            &params.id,
        ))
    }
}
pub struct MarkResolvedByTaskStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn mark_resolved_by_task() -> MarkResolvedByTaskStmt {
    MarkResolvedByTaskStmt(
        "UPDATE error_events SET status = 'resolved', resolved_by_task_run_id = $1, resolution_notes = $2, resolved_at = NOW() WHERE id = $3",
        None,
    )
}
impl MarkResolvedByTaskStmt {
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
        task_run_id: &'a T1,
        resolution_notes: &'a Option<T2>,
        id: &'a i64,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(self.0, &[task_run_id, resolution_notes, id])
            .await
    }
}
impl<'a, C: GenericClient + Send + Sync, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        MarkResolvedByTaskParams<T1, T2>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for MarkResolvedByTaskStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a MarkResolvedByTaskParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.task_run_id,
            &params.resolution_notes,
            &params.id,
        ))
    }
}
pub struct GetErrorDebugContextStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_error_debug_context() -> GetErrorDebugContextStmt {
    GetErrorDebugContextStmt(
        "SELECT e.id, e.log_source_id, e.log_source_name, e.task_run_id, e.workflow_step_id, e.log_timestamp::TEXT, e.captured_at::TEXT, e.severity, e.error_type, e.error_code, e.message, e.stack_trace, e.context_lines, e.raw_entry, e.file_path, e.line_number, e.column_number, e.function_name, e.signature_hash, e.occurrence_count, e.first_seen_at::TEXT, e.last_seen_at::TEXT, e.status, e.finding_id, e.resolved_by_task_run_id, e.resolution_notes, e.acknowledged_at::TEXT, e.resolved_at::TEXT, tr.workflow_name, e.trace_id FROM error_events e LEFT JOIN task_runs tr ON e.task_run_id = tr.id WHERE e.status IN ('new', 'acknowledged', 'in_progress', 'promoted') AND ($1::TEXT IS NULL OR e.task_run_id = $1) ORDER BY CASE e.severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END, e.occurrence_count DESC, e.last_seen_at DESC LIMIT 50",
        None,
    )
}
impl GetErrorDebugContextStmt {
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
        task_run_id: &'a Option<T1>,
    ) -> GetErrorDebugContextQuery<'c, 'a, 's, C, GetErrorDebugContext, 1> {
        GetErrorDebugContextQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetErrorDebugContextBorrowed, tokio_postgres::Error> {
                Ok(GetErrorDebugContextBorrowed {
                    id: row.try_get(0)?,
                    log_source_id: row.try_get(1)?,
                    log_source_name: row.try_get(2)?,
                    task_run_id: row.try_get(3)?,
                    workflow_step_id: row.try_get(4)?,
                    log_timestamp: row.try_get(5)?,
                    captured_at: row.try_get(6)?,
                    severity: row.try_get(7)?,
                    error_type: row.try_get(8)?,
                    error_code: row.try_get(9)?,
                    message: row.try_get(10)?,
                    stack_trace: row.try_get(11)?,
                    context_lines: row.try_get(12)?,
                    raw_entry: row.try_get(13)?,
                    file_path: row.try_get(14)?,
                    line_number: row.try_get(15)?,
                    column_number: row.try_get(16)?,
                    function_name: row.try_get(17)?,
                    signature_hash: row.try_get(18)?,
                    occurrence_count: row.try_get(19)?,
                    first_seen_at: row.try_get(20)?,
                    last_seen_at: row.try_get(21)?,
                    status: row.try_get(22)?,
                    finding_id: row.try_get(23)?,
                    resolved_by_task_run_id: row.try_get(24)?,
                    resolution_notes: row.try_get(25)?,
                    acknowledged_at: row.try_get(26)?,
                    resolved_at: row.try_get(27)?,
                    workflow_name: row.try_get(28)?,
                    trace_id: row.try_get(29)?,
                })
            },
            mapper: |it| GetErrorDebugContext::from(it),
        }
    }
}
