// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreateTaskRunEventParams<
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
    pub task_run_id: T1,
    pub event_type: T2,
    pub event_subtype: Option<T3>,
    pub message: T4,
    pub data: Option<T5>,
    pub workflow_name: Option<T6>,
    pub state_name: Option<T7>,
    pub action_id: Option<T8>,
    pub timestamp: T9,
    pub duration_ms: Option<i64>,
}
#[derive(Debug)]
pub struct GetTaskRunEventsByTypeParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub task_run_id: T1,
    pub event_type: T2,
}
#[derive(Debug)]
pub struct GetTaskRunEventsLimitedParams<T1: crate::StringSql> {
    pub task_run_id: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct CreateTaskRunScreenshotParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
> {
    pub id: T1,
    pub task_run_id: T2,
    pub event_id: Option<i64>,
    pub file_path: T3,
    pub screenshot_type: T4,
    pub template_name: Option<T5>,
    pub confidence: Option<f64>,
    pub match_location: Option<T6>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub file_size_bytes: Option<i64>,
}
#[derive(Debug)]
pub struct GetTaskRunScreenshotsByTypeParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub task_run_id: T1,
    pub screenshot_type: T2,
}
#[derive(Debug)]
pub struct CreateTaskRunPlaywrightResultParams<
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
> {
    pub id: T1,
    pub task_run_id: T2,
    pub test_name: T3,
    pub spec_file: Option<T4>,
    pub status: T5,
    pub duration_ms: Option<i64>,
    pub stdout: Option<T6>,
    pub stderr: Option<T7>,
    pub console_output: Option<T8>,
    pub page_snapshot: Option<T9>,
    pub error_message: Option<T10>,
    pub failure_screenshot_path: Option<T11>,
    pub assertions_passed: i32,
    pub assertions_failed: i32,
}
#[derive(Debug)]
pub struct CreateTaskRunApiRequestParams<
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
> {
    pub id: T1,
    pub task_run_id: T2,
    pub step_id: T3,
    pub step_name: Option<T4>,
    pub method: T5,
    pub url: T6,
    pub resolved_url: T7,
    pub request_headers: Option<T8>,
    pub request_body: Option<T9>,
    pub status_code: i32,
    pub status_text: Option<T10>,
    pub response_headers: Option<T11>,
    pub response_time_ms: i64,
    pub response_body_type: T12,
    pub response_body: Option<T13>,
    pub response_size_bytes: Option<i64>,
    pub extractions: Option<T14>,
    pub assertions: Option<T15>,
    pub success: bool,
    pub error_message: Option<T16>,
}
#[derive(Debug)]
pub struct CreateTaskRunAwasStepParams<
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
    pub id: T1,
    pub task_run_id: T2,
    pub step_id: Option<T3>,
    pub step_name: Option<T4>,
    pub step_type: T5,
    pub url: Option<T6>,
    pub action_id: Option<T7>,
    pub parameters: Option<T8>,
    pub response_data: Option<T9>,
    pub success: bool,
    pub error_message: Option<T10>,
    pub duration_ms: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunEventsAll {
    pub id: i64,
    pub task_run_id: String,
    pub event_type: String,
    pub event_subtype: String,
    pub message: String,
    pub data: String,
    pub workflow_name: String,
    pub state_name: String,
    pub action_id: String,
    pub timestamp: String,
    pub duration_ms: i64,
}
pub struct GetTaskRunEventsAllBorrowed<'a> {
    pub id: i64,
    pub task_run_id: &'a str,
    pub event_type: &'a str,
    pub event_subtype: &'a str,
    pub message: &'a str,
    pub data: &'a str,
    pub workflow_name: &'a str,
    pub state_name: &'a str,
    pub action_id: &'a str,
    pub timestamp: &'a str,
    pub duration_ms: i64,
}
impl<'a> From<GetTaskRunEventsAllBorrowed<'a>> for GetTaskRunEventsAll {
    fn from(
        GetTaskRunEventsAllBorrowed {
            id,
            task_run_id,
            event_type,
            event_subtype,
            message,
            data,
            workflow_name,
            state_name,
            action_id,
            timestamp,
            duration_ms,
        }: GetTaskRunEventsAllBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            task_run_id: task_run_id.into(),
            event_type: event_type.into(),
            event_subtype: event_subtype.into(),
            message: message.into(),
            data: data.into(),
            workflow_name: workflow_name.into(),
            state_name: state_name.into(),
            action_id: action_id.into(),
            timestamp: timestamp.into(),
            duration_ms,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunEventsByType {
    pub id: i64,
    pub task_run_id: String,
    pub event_type: String,
    pub event_subtype: String,
    pub message: String,
    pub data: String,
    pub workflow_name: String,
    pub state_name: String,
    pub action_id: String,
    pub timestamp: String,
    pub duration_ms: i64,
}
pub struct GetTaskRunEventsByTypeBorrowed<'a> {
    pub id: i64,
    pub task_run_id: &'a str,
    pub event_type: &'a str,
    pub event_subtype: &'a str,
    pub message: &'a str,
    pub data: &'a str,
    pub workflow_name: &'a str,
    pub state_name: &'a str,
    pub action_id: &'a str,
    pub timestamp: &'a str,
    pub duration_ms: i64,
}
impl<'a> From<GetTaskRunEventsByTypeBorrowed<'a>> for GetTaskRunEventsByType {
    fn from(
        GetTaskRunEventsByTypeBorrowed {
            id,
            task_run_id,
            event_type,
            event_subtype,
            message,
            data,
            workflow_name,
            state_name,
            action_id,
            timestamp,
            duration_ms,
        }: GetTaskRunEventsByTypeBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            task_run_id: task_run_id.into(),
            event_type: event_type.into(),
            event_subtype: event_subtype.into(),
            message: message.into(),
            data: data.into(),
            workflow_name: workflow_name.into(),
            state_name: state_name.into(),
            action_id: action_id.into(),
            timestamp: timestamp.into(),
            duration_ms,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunEventsLimited {
    pub id: i64,
    pub task_run_id: String,
    pub event_type: String,
    pub event_subtype: String,
    pub message: String,
    pub data: String,
    pub workflow_name: String,
    pub state_name: String,
    pub action_id: String,
    pub timestamp: String,
    pub duration_ms: i64,
}
pub struct GetTaskRunEventsLimitedBorrowed<'a> {
    pub id: i64,
    pub task_run_id: &'a str,
    pub event_type: &'a str,
    pub event_subtype: &'a str,
    pub message: &'a str,
    pub data: &'a str,
    pub workflow_name: &'a str,
    pub state_name: &'a str,
    pub action_id: &'a str,
    pub timestamp: &'a str,
    pub duration_ms: i64,
}
impl<'a> From<GetTaskRunEventsLimitedBorrowed<'a>> for GetTaskRunEventsLimited {
    fn from(
        GetTaskRunEventsLimitedBorrowed {
            id,
            task_run_id,
            event_type,
            event_subtype,
            message,
            data,
            workflow_name,
            state_name,
            action_id,
            timestamp,
            duration_ms,
        }: GetTaskRunEventsLimitedBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            task_run_id: task_run_id.into(),
            event_type: event_type.into(),
            event_subtype: event_subtype.into(),
            message: message.into(),
            data: data.into(),
            workflow_name: workflow_name.into(),
            state_name: state_name.into(),
            action_id: action_id.into(),
            timestamp: timestamp.into(),
            duration_ms,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunScreenshotsAll {
    pub id: String,
    pub task_run_id: String,
    pub event_id: i64,
    pub file_path: String,
    pub screenshot_type: String,
    pub template_name: String,
    pub confidence: f64,
    pub match_location: String,
    pub width: i32,
    pub height: i32,
    pub file_size_bytes: i64,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTaskRunScreenshotsAllBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub event_id: i64,
    pub file_path: &'a str,
    pub screenshot_type: &'a str,
    pub template_name: &'a str,
    pub confidence: f64,
    pub match_location: &'a str,
    pub width: i32,
    pub height: i32,
    pub file_size_bytes: i64,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTaskRunScreenshotsAllBorrowed<'a>> for GetTaskRunScreenshotsAll {
    fn from(
        GetTaskRunScreenshotsAllBorrowed {
            id,
            task_run_id,
            event_id,
            file_path,
            screenshot_type,
            template_name,
            confidence,
            match_location,
            width,
            height,
            file_size_bytes,
            created_at,
        }: GetTaskRunScreenshotsAllBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            event_id,
            file_path: file_path.into(),
            screenshot_type: screenshot_type.into(),
            template_name: template_name.into(),
            confidence,
            match_location: match_location.into(),
            width,
            height,
            file_size_bytes,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunScreenshotsByType {
    pub id: String,
    pub task_run_id: String,
    pub event_id: i64,
    pub file_path: String,
    pub screenshot_type: String,
    pub template_name: String,
    pub confidence: f64,
    pub match_location: String,
    pub width: i32,
    pub height: i32,
    pub file_size_bytes: i64,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTaskRunScreenshotsByTypeBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub event_id: i64,
    pub file_path: &'a str,
    pub screenshot_type: &'a str,
    pub template_name: &'a str,
    pub confidence: f64,
    pub match_location: &'a str,
    pub width: i32,
    pub height: i32,
    pub file_size_bytes: i64,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTaskRunScreenshotsByTypeBorrowed<'a>> for GetTaskRunScreenshotsByType {
    fn from(
        GetTaskRunScreenshotsByTypeBorrowed {
            id,
            task_run_id,
            event_id,
            file_path,
            screenshot_type,
            template_name,
            confidence,
            match_location,
            width,
            height,
            file_size_bytes,
            created_at,
        }: GetTaskRunScreenshotsByTypeBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            event_id,
            file_path: file_path.into(),
            screenshot_type: screenshot_type.into(),
            template_name: template_name.into(),
            confidence,
            match_location: match_location.into(),
            width,
            height,
            file_size_bytes,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunPlaywrightResultsAll {
    pub id: String,
    pub task_run_id: String,
    pub test_name: String,
    pub spec_file: String,
    pub status: String,
    pub duration_ms: i64,
    pub stdout: String,
    pub stderr: String,
    pub console_output: String,
    pub page_snapshot: String,
    pub error_message: String,
    pub failure_screenshot_path: String,
    pub assertions_passed: i32,
    pub assertions_failed: i32,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTaskRunPlaywrightResultsAllBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub test_name: &'a str,
    pub spec_file: &'a str,
    pub status: &'a str,
    pub duration_ms: i64,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub console_output: &'a str,
    pub page_snapshot: &'a str,
    pub error_message: &'a str,
    pub failure_screenshot_path: &'a str,
    pub assertions_passed: i32,
    pub assertions_failed: i32,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTaskRunPlaywrightResultsAllBorrowed<'a>> for GetTaskRunPlaywrightResultsAll {
    fn from(
        GetTaskRunPlaywrightResultsAllBorrowed {
            id,
            task_run_id,
            test_name,
            spec_file,
            status,
            duration_ms,
            stdout,
            stderr,
            console_output,
            page_snapshot,
            error_message,
            failure_screenshot_path,
            assertions_passed,
            assertions_failed,
            created_at,
        }: GetTaskRunPlaywrightResultsAllBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            test_name: test_name.into(),
            spec_file: spec_file.into(),
            status: status.into(),
            duration_ms,
            stdout: stdout.into(),
            stderr: stderr.into(),
            console_output: console_output.into(),
            page_snapshot: page_snapshot.into(),
            error_message: error_message.into(),
            failure_screenshot_path: failure_screenshot_path.into(),
            assertions_passed,
            assertions_failed,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunApiRequestsAll {
    pub id: String,
    pub task_run_id: String,
    pub step_id: String,
    pub step_name: String,
    pub method: String,
    pub url: String,
    pub resolved_url: String,
    pub request_headers: String,
    pub request_body: String,
    pub status_code: i32,
    pub status_text: String,
    pub response_headers: String,
    pub response_time_ms: i64,
    pub response_body_type: String,
    pub response_body: String,
    pub response_size_bytes: i64,
    pub extractions: String,
    pub assertions: String,
    pub success: bool,
    pub error_message: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTaskRunApiRequestsAllBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub step_id: &'a str,
    pub step_name: &'a str,
    pub method: &'a str,
    pub url: &'a str,
    pub resolved_url: &'a str,
    pub request_headers: &'a str,
    pub request_body: &'a str,
    pub status_code: i32,
    pub status_text: &'a str,
    pub response_headers: &'a str,
    pub response_time_ms: i64,
    pub response_body_type: &'a str,
    pub response_body: &'a str,
    pub response_size_bytes: i64,
    pub extractions: &'a str,
    pub assertions: &'a str,
    pub success: bool,
    pub error_message: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTaskRunApiRequestsAllBorrowed<'a>> for GetTaskRunApiRequestsAll {
    fn from(
        GetTaskRunApiRequestsAllBorrowed {
            id,
            task_run_id,
            step_id,
            step_name,
            method,
            url,
            resolved_url,
            request_headers,
            request_body,
            status_code,
            status_text,
            response_headers,
            response_time_ms,
            response_body_type,
            response_body,
            response_size_bytes,
            extractions,
            assertions,
            success,
            error_message,
            created_at,
        }: GetTaskRunApiRequestsAllBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            step_id: step_id.into(),
            step_name: step_name.into(),
            method: method.into(),
            url: url.into(),
            resolved_url: resolved_url.into(),
            request_headers: request_headers.into(),
            request_body: request_body.into(),
            status_code,
            status_text: status_text.into(),
            response_headers: response_headers.into(),
            response_time_ms,
            response_body_type: response_body_type.into(),
            response_body: response_body.into(),
            response_size_bytes,
            extractions: extractions.into(),
            assertions: assertions.into(),
            success,
            error_message: error_message.into(),
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunAwasSteps {
    pub id: String,
    pub task_run_id: String,
    pub step_id: String,
    pub step_name: String,
    pub step_type: String,
    pub url: String,
    pub action_id: String,
    pub parameters: String,
    pub response_data: String,
    pub success: bool,
    pub error_message: String,
    pub duration_ms: i64,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTaskRunAwasStepsBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub step_id: &'a str,
    pub step_name: &'a str,
    pub step_type: &'a str,
    pub url: &'a str,
    pub action_id: &'a str,
    pub parameters: &'a str,
    pub response_data: &'a str,
    pub success: bool,
    pub error_message: &'a str,
    pub duration_ms: i64,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTaskRunAwasStepsBorrowed<'a>> for GetTaskRunAwasSteps {
    fn from(
        GetTaskRunAwasStepsBorrowed {
            id,
            task_run_id,
            step_id,
            step_name,
            step_type,
            url,
            action_id,
            parameters,
            response_data,
            success,
            error_message,
            duration_ms,
            created_at,
        }: GetTaskRunAwasStepsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            step_id: step_id.into(),
            step_name: step_name.into(),
            step_type: step_type.into(),
            url: url.into(),
            action_id: action_id.into(),
            parameters: parameters.into(),
            response_data: response_data.into(),
            success,
            error_message: error_message.into(),
            duration_ms,
            created_at,
        }
    }
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
pub struct GetTaskRunEventsAllQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTaskRunEventsAllBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunEventsAllBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunEventsAllQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunEventsAllBorrowed) -> R,
    ) -> GetTaskRunEventsAllQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunEventsAllQuery {
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
pub struct GetTaskRunEventsByTypeQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTaskRunEventsByTypeBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunEventsByTypeBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunEventsByTypeQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunEventsByTypeBorrowed) -> R,
    ) -> GetTaskRunEventsByTypeQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunEventsByTypeQuery {
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
pub struct GetTaskRunEventsLimitedQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTaskRunEventsLimitedBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunEventsLimitedBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunEventsLimitedQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunEventsLimitedBorrowed) -> R,
    ) -> GetTaskRunEventsLimitedQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunEventsLimitedQuery {
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
pub struct GetTaskRunScreenshotsAllQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTaskRunScreenshotsAllBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunScreenshotsAllBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunScreenshotsAllQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunScreenshotsAllBorrowed) -> R,
    ) -> GetTaskRunScreenshotsAllQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunScreenshotsAllQuery {
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
pub struct GetTaskRunScreenshotsByTypeQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetTaskRunScreenshotsByTypeBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunScreenshotsByTypeBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunScreenshotsByTypeQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunScreenshotsByTypeBorrowed) -> R,
    ) -> GetTaskRunScreenshotsByTypeQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunScreenshotsByTypeQuery {
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
pub struct GetTaskRunPlaywrightResultsAllQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetTaskRunPlaywrightResultsAllBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunPlaywrightResultsAllBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunPlaywrightResultsAllQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunPlaywrightResultsAllBorrowed) -> R,
    ) -> GetTaskRunPlaywrightResultsAllQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunPlaywrightResultsAllQuery {
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
pub struct GetTaskRunApiRequestsAllQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTaskRunApiRequestsAllBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunApiRequestsAllBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunApiRequestsAllQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunApiRequestsAllBorrowed) -> R,
    ) -> GetTaskRunApiRequestsAllQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunApiRequestsAllQuery {
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
pub struct GetTaskRunAwasStepsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTaskRunAwasStepsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunAwasStepsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunAwasStepsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunAwasStepsBorrowed) -> R,
    ) -> GetTaskRunAwasStepsQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunAwasStepsQuery {
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
pub struct CreateTaskRunEventStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_task_run_event() -> CreateTaskRunEventStmt {
    CreateTaskRunEventStmt(
        "INSERT INTO task_run_events (task_run_id, event_type, event_subtype, message, data, workflow_name, state_name, action_id, timestamp, duration_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        None,
    )
}
impl CreateTaskRunEventStmt {
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
    >(
        &'s self,
        client: &'c C,
        task_run_id: &'a T1,
        event_type: &'a T2,
        event_subtype: &'a Option<T3>,
        message: &'a T4,
        data: &'a Option<T5>,
        workflow_name: &'a Option<T6>,
        state_name: &'a Option<T7>,
        action_id: &'a Option<T8>,
        timestamp: &'a T9,
        duration_ms: &'a Option<i64>,
    ) -> I64Query<'c, 'a, 's, C, i64, 10> {
        I64Query {
            client,
            params: [
                task_run_id,
                event_type,
                event_subtype,
                message,
                data,
                workflow_name,
                state_name,
                action_id,
                timestamp,
                duration_ms,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        CreateTaskRunEventParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
        I64Query<'c, 'a, 's, C, i64, 10>,
        C,
    > for CreateTaskRunEventStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateTaskRunEventParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    ) -> I64Query<'c, 'a, 's, C, i64, 10> {
        self.bind(
            client,
            &params.task_run_id,
            &params.event_type,
            &params.event_subtype,
            &params.message,
            &params.data,
            &params.workflow_name,
            &params.state_name,
            &params.action_id,
            &params.timestamp,
            &params.duration_ms,
        )
    }
}
pub struct GetTaskRunEventsAllStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_events_all() -> GetTaskRunEventsAllStmt {
    GetTaskRunEventsAllStmt(
        "SELECT id, task_run_id, COALESCE(event_type, '') as event_type, COALESCE(event_subtype, '') as event_subtype, COALESCE(message, '') as message, COALESCE(data, '') as data, COALESCE(workflow_name, '') as workflow_name, COALESCE(state_name, '') as state_name, COALESCE(action_id, '') as action_id, COALESCE(timestamp, '') as timestamp, COALESCE(duration_ms, 0) as duration_ms FROM task_run_events WHERE task_run_id = $1 ORDER BY timestamp ASC",
        None,
    )
}
impl GetTaskRunEventsAllStmt {
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
    ) -> GetTaskRunEventsAllQuery<'c, 'a, 's, C, GetTaskRunEventsAll, 1> {
        GetTaskRunEventsAllQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskRunEventsAllBorrowed, tokio_postgres::Error> {
                Ok(GetTaskRunEventsAllBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    event_type: row.try_get(2)?,
                    event_subtype: row.try_get(3)?,
                    message: row.try_get(4)?,
                    data: row.try_get(5)?,
                    workflow_name: row.try_get(6)?,
                    state_name: row.try_get(7)?,
                    action_id: row.try_get(8)?,
                    timestamp: row.try_get(9)?,
                    duration_ms: row.try_get(10)?,
                })
            },
            mapper: |it| GetTaskRunEventsAll::from(it),
        }
    }
}
pub struct GetTaskRunEventsByTypeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_events_by_type() -> GetTaskRunEventsByTypeStmt {
    GetTaskRunEventsByTypeStmt(
        "SELECT id, task_run_id, COALESCE(event_type, '') as event_type, COALESCE(event_subtype, '') as event_subtype, COALESCE(message, '') as message, COALESCE(data, '') as data, COALESCE(workflow_name, '') as workflow_name, COALESCE(state_name, '') as state_name, COALESCE(action_id, '') as action_id, COALESCE(timestamp, '') as timestamp, COALESCE(duration_ms, 0) as duration_ms FROM task_run_events WHERE task_run_id = $1 AND event_type = $2 ORDER BY timestamp ASC",
        None,
    )
}
impl GetTaskRunEventsByTypeStmt {
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
        event_type: &'a T2,
    ) -> GetTaskRunEventsByTypeQuery<'c, 'a, 's, C, GetTaskRunEventsByType, 2> {
        GetTaskRunEventsByTypeQuery {
            client,
            params: [task_run_id, event_type],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskRunEventsByTypeBorrowed, tokio_postgres::Error> {
                Ok(GetTaskRunEventsByTypeBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    event_type: row.try_get(2)?,
                    event_subtype: row.try_get(3)?,
                    message: row.try_get(4)?,
                    data: row.try_get(5)?,
                    workflow_name: row.try_get(6)?,
                    state_name: row.try_get(7)?,
                    action_id: row.try_get(8)?,
                    timestamp: row.try_get(9)?,
                    duration_ms: row.try_get(10)?,
                })
            },
            mapper: |it| GetTaskRunEventsByType::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetTaskRunEventsByTypeParams<T1, T2>,
        GetTaskRunEventsByTypeQuery<'c, 'a, 's, C, GetTaskRunEventsByType, 2>,
        C,
    > for GetTaskRunEventsByTypeStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetTaskRunEventsByTypeParams<T1, T2>,
    ) -> GetTaskRunEventsByTypeQuery<'c, 'a, 's, C, GetTaskRunEventsByType, 2> {
        self.bind(client, &params.task_run_id, &params.event_type)
    }
}
pub struct GetTaskRunEventsLimitedStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_events_limited() -> GetTaskRunEventsLimitedStmt {
    GetTaskRunEventsLimitedStmt(
        "SELECT id, task_run_id, COALESCE(event_type, '') as event_type, COALESCE(event_subtype, '') as event_subtype, COALESCE(message, '') as message, COALESCE(data, '') as data, COALESCE(workflow_name, '') as workflow_name, COALESCE(state_name, '') as state_name, COALESCE(action_id, '') as action_id, COALESCE(timestamp, '') as timestamp, COALESCE(duration_ms, 0) as duration_ms FROM task_run_events WHERE task_run_id = $1 ORDER BY timestamp ASC LIMIT $2",
        None,
    )
}
impl GetTaskRunEventsLimitedStmt {
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
    ) -> GetTaskRunEventsLimitedQuery<'c, 'a, 's, C, GetTaskRunEventsLimited, 2> {
        GetTaskRunEventsLimitedQuery {
            client,
            params: [task_run_id, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskRunEventsLimitedBorrowed, tokio_postgres::Error> {
                Ok(GetTaskRunEventsLimitedBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    event_type: row.try_get(2)?,
                    event_subtype: row.try_get(3)?,
                    message: row.try_get(4)?,
                    data: row.try_get(5)?,
                    workflow_name: row.try_get(6)?,
                    state_name: row.try_get(7)?,
                    action_id: row.try_get(8)?,
                    timestamp: row.try_get(9)?,
                    duration_ms: row.try_get(10)?,
                })
            },
            mapper: |it| GetTaskRunEventsLimited::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetTaskRunEventsLimitedParams<T1>,
        GetTaskRunEventsLimitedQuery<'c, 'a, 's, C, GetTaskRunEventsLimited, 2>,
        C,
    > for GetTaskRunEventsLimitedStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetTaskRunEventsLimitedParams<T1>,
    ) -> GetTaskRunEventsLimitedQuery<'c, 'a, 's, C, GetTaskRunEventsLimited, 2> {
        self.bind(client, &params.task_run_id, &params.max_results)
    }
}
pub struct DeleteTaskRunEventsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_task_run_events() -> DeleteTaskRunEventsStmt {
    DeleteTaskRunEventsStmt("DELETE FROM task_run_events WHERE task_run_id = $1", None)
}
impl DeleteTaskRunEventsStmt {
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
        task_run_id: &'a T1,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[task_run_id]).await
    }
}
pub struct GetEventCountByTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_event_count_by_task_run() -> GetEventCountByTaskRunStmt {
    GetEventCountByTaskRunStmt(
        "SELECT COUNT(*)::bigint as count FROM task_run_events WHERE task_run_id = $1",
        None,
    )
}
impl GetEventCountByTaskRunStmt {
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
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct CreateTaskRunScreenshotStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_task_run_screenshot() -> CreateTaskRunScreenshotStmt {
    CreateTaskRunScreenshotStmt(
        "INSERT INTO task_run_screenshots (id, task_run_id, event_id, file_path, screenshot_type, template_name, confidence, match_location, width, height, file_size_bytes, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())",
        None,
    )
}
impl CreateTaskRunScreenshotStmt {
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
        event_id: &'a Option<i64>,
        file_path: &'a T3,
        screenshot_type: &'a T4,
        template_name: &'a Option<T5>,
        confidence: &'a Option<f64>,
        match_location: &'a Option<T6>,
        width: &'a Option<i32>,
        height: &'a Option<i32>,
        file_size_bytes: &'a Option<i64>,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    task_run_id,
                    event_id,
                    file_path,
                    screenshot_type,
                    template_name,
                    confidence,
                    match_location,
                    width,
                    height,
                    file_size_bytes,
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
        CreateTaskRunScreenshotParams<T1, T2, T3, T4, T5, T6>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for CreateTaskRunScreenshotStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a CreateTaskRunScreenshotParams<T1, T2, T3, T4, T5, T6>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.event_id,
            &params.file_path,
            &params.screenshot_type,
            &params.template_name,
            &params.confidence,
            &params.match_location,
            &params.width,
            &params.height,
            &params.file_size_bytes,
        ))
    }
}
pub struct GetTaskRunScreenshotsAllStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_screenshots_all() -> GetTaskRunScreenshotsAllStmt {
    GetTaskRunScreenshotsAllStmt(
        "SELECT id, task_run_id, event_id, file_path, screenshot_type, template_name, confidence, match_location, width, height, file_size_bytes, created_at FROM task_run_screenshots WHERE task_run_id = $1 ORDER BY created_at ASC",
        None,
    )
}
impl GetTaskRunScreenshotsAllStmt {
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
    ) -> GetTaskRunScreenshotsAllQuery<'c, 'a, 's, C, GetTaskRunScreenshotsAll, 1> {
        GetTaskRunScreenshotsAllQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskRunScreenshotsAllBorrowed, tokio_postgres::Error> {
                Ok(GetTaskRunScreenshotsAllBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    event_id: row.try_get(2)?,
                    file_path: row.try_get(3)?,
                    screenshot_type: row.try_get(4)?,
                    template_name: row.try_get(5)?,
                    confidence: row.try_get(6)?,
                    match_location: row.try_get(7)?,
                    width: row.try_get(8)?,
                    height: row.try_get(9)?,
                    file_size_bytes: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                })
            },
            mapper: |it| GetTaskRunScreenshotsAll::from(it),
        }
    }
}
pub struct GetTaskRunScreenshotsByTypeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_screenshots_by_type() -> GetTaskRunScreenshotsByTypeStmt {
    GetTaskRunScreenshotsByTypeStmt(
        "SELECT id, task_run_id, event_id, file_path, screenshot_type, template_name, confidence, match_location, width, height, file_size_bytes, created_at FROM task_run_screenshots WHERE task_run_id = $1 AND screenshot_type = $2 ORDER BY created_at ASC",
        None,
    )
}
impl GetTaskRunScreenshotsByTypeStmt {
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
        screenshot_type: &'a T2,
    ) -> GetTaskRunScreenshotsByTypeQuery<'c, 'a, 's, C, GetTaskRunScreenshotsByType, 2> {
        GetTaskRunScreenshotsByTypeQuery {
            client,
            params: [task_run_id, screenshot_type],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskRunScreenshotsByTypeBorrowed, tokio_postgres::Error> {
                Ok(GetTaskRunScreenshotsByTypeBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    event_id: row.try_get(2)?,
                    file_path: row.try_get(3)?,
                    screenshot_type: row.try_get(4)?,
                    template_name: row.try_get(5)?,
                    confidence: row.try_get(6)?,
                    match_location: row.try_get(7)?,
                    width: row.try_get(8)?,
                    height: row.try_get(9)?,
                    file_size_bytes: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                })
            },
            mapper: |it| GetTaskRunScreenshotsByType::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetTaskRunScreenshotsByTypeParams<T1, T2>,
        GetTaskRunScreenshotsByTypeQuery<'c, 'a, 's, C, GetTaskRunScreenshotsByType, 2>,
        C,
    > for GetTaskRunScreenshotsByTypeStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetTaskRunScreenshotsByTypeParams<T1, T2>,
    ) -> GetTaskRunScreenshotsByTypeQuery<'c, 'a, 's, C, GetTaskRunScreenshotsByType, 2> {
        self.bind(client, &params.task_run_id, &params.screenshot_type)
    }
}
pub struct CreateTaskRunPlaywrightResultStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_task_run_playwright_result() -> CreateTaskRunPlaywrightResultStmt {
    CreateTaskRunPlaywrightResultStmt(
        "INSERT INTO task_run_playwright_results (id, task_run_id, test_name, spec_file, status, duration_ms, stdout, stderr, console_output, page_snapshot, error_message, failure_screenshot_path, assertions_passed, assertions_failed, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NOW())",
        None,
    )
}
impl CreateTaskRunPlaywrightResultStmt {
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
        T10: crate::StringSql,
        T11: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        task_run_id: &'a T2,
        test_name: &'a T3,
        spec_file: &'a Option<T4>,
        status: &'a T5,
        duration_ms: &'a Option<i64>,
        stdout: &'a Option<T6>,
        stderr: &'a Option<T7>,
        console_output: &'a Option<T8>,
        page_snapshot: &'a Option<T9>,
        error_message: &'a Option<T10>,
        failure_screenshot_path: &'a Option<T11>,
        assertions_passed: &'a i32,
        assertions_failed: &'a i32,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    task_run_id,
                    test_name,
                    spec_file,
                    status,
                    duration_ms,
                    stdout,
                    stderr,
                    console_output,
                    page_snapshot,
                    error_message,
                    failure_screenshot_path,
                    assertions_passed,
                    assertions_failed,
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
    T10: crate::StringSql,
    T11: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        CreateTaskRunPlaywrightResultParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for CreateTaskRunPlaywrightResultStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a CreateTaskRunPlaywrightResultParams<
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
        >,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.test_name,
            &params.spec_file,
            &params.status,
            &params.duration_ms,
            &params.stdout,
            &params.stderr,
            &params.console_output,
            &params.page_snapshot,
            &params.error_message,
            &params.failure_screenshot_path,
            &params.assertions_passed,
            &params.assertions_failed,
        ))
    }
}
pub struct GetTaskRunPlaywrightResultsAllStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_playwright_results_all() -> GetTaskRunPlaywrightResultsAllStmt {
    GetTaskRunPlaywrightResultsAllStmt(
        "SELECT id, task_run_id, test_name, spec_file, status, duration_ms, stdout, stderr, console_output, page_snapshot, error_message, failure_screenshot_path, assertions_passed, assertions_failed, created_at FROM task_run_playwright_results WHERE task_run_id = $1 ORDER BY created_at ASC",
        None,
    )
}
impl GetTaskRunPlaywrightResultsAllStmt {
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
    ) -> GetTaskRunPlaywrightResultsAllQuery<'c, 'a, 's, C, GetTaskRunPlaywrightResultsAll, 1> {
        GetTaskRunPlaywrightResultsAllQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row: &tokio_postgres::Row| -> Result<
                GetTaskRunPlaywrightResultsAllBorrowed,
                tokio_postgres::Error,
            > {
                Ok(GetTaskRunPlaywrightResultsAllBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    test_name: row.try_get(2)?,
                    spec_file: row.try_get(3)?,
                    status: row.try_get(4)?,
                    duration_ms: row.try_get(5)?,
                    stdout: row.try_get(6)?,
                    stderr: row.try_get(7)?,
                    console_output: row.try_get(8)?,
                    page_snapshot: row.try_get(9)?,
                    error_message: row.try_get(10)?,
                    failure_screenshot_path: row.try_get(11)?,
                    assertions_passed: row.try_get(12)?,
                    assertions_failed: row.try_get(13)?,
                    created_at: row.try_get(14)?,
                })
            },
            mapper: |it| GetTaskRunPlaywrightResultsAll::from(it),
        }
    }
}
pub struct CreateTaskRunApiRequestStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_task_run_api_request() -> CreateTaskRunApiRequestStmt {
    CreateTaskRunApiRequestStmt(
        "INSERT INTO task_run_api_requests (id, task_run_id, step_id, step_name, method, url, resolved_url, request_headers, request_body, status_code, status_text, response_headers, response_time_ms, response_body_type, response_body, response_size_bytes, extractions, assertions, success, error_message, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, NOW())",
        None,
    )
}
impl CreateTaskRunApiRequestStmt {
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
        T10: crate::StringSql,
        T11: crate::StringSql,
        T12: crate::StringSql,
        T13: crate::StringSql,
        T14: crate::StringSql,
        T15: crate::StringSql,
        T16: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        task_run_id: &'a T2,
        step_id: &'a T3,
        step_name: &'a Option<T4>,
        method: &'a T5,
        url: &'a T6,
        resolved_url: &'a T7,
        request_headers: &'a Option<T8>,
        request_body: &'a Option<T9>,
        status_code: &'a i32,
        status_text: &'a Option<T10>,
        response_headers: &'a Option<T11>,
        response_time_ms: &'a i64,
        response_body_type: &'a T12,
        response_body: &'a Option<T13>,
        response_size_bytes: &'a Option<i64>,
        extractions: &'a Option<T14>,
        assertions: &'a Option<T15>,
        success: &'a bool,
        error_message: &'a Option<T16>,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    task_run_id,
                    step_id,
                    step_name,
                    method,
                    url,
                    resolved_url,
                    request_headers,
                    request_body,
                    status_code,
                    status_text,
                    response_headers,
                    response_time_ms,
                    response_body_type,
                    response_body,
                    response_size_bytes,
                    extractions,
                    assertions,
                    success,
                    error_message,
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
    T10: crate::StringSql,
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        CreateTaskRunApiRequestParams<
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
        >,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for CreateTaskRunApiRequestStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a CreateTaskRunApiRequestParams<
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
        >,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.step_id,
            &params.step_name,
            &params.method,
            &params.url,
            &params.resolved_url,
            &params.request_headers,
            &params.request_body,
            &params.status_code,
            &params.status_text,
            &params.response_headers,
            &params.response_time_ms,
            &params.response_body_type,
            &params.response_body,
            &params.response_size_bytes,
            &params.extractions,
            &params.assertions,
            &params.success,
            &params.error_message,
        ))
    }
}
pub struct GetTaskRunApiRequestsAllStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_api_requests_all() -> GetTaskRunApiRequestsAllStmt {
    GetTaskRunApiRequestsAllStmt(
        "SELECT id, task_run_id, step_id, step_name, method, url, resolved_url, request_headers, request_body, status_code, status_text, response_headers, response_time_ms, response_body_type, response_body, response_size_bytes, extractions, assertions, success, error_message, created_at FROM task_run_api_requests WHERE task_run_id = $1 ORDER BY created_at ASC",
        None,
    )
}
impl GetTaskRunApiRequestsAllStmt {
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
    ) -> GetTaskRunApiRequestsAllQuery<'c, 'a, 's, C, GetTaskRunApiRequestsAll, 1> {
        GetTaskRunApiRequestsAllQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskRunApiRequestsAllBorrowed, tokio_postgres::Error> {
                Ok(GetTaskRunApiRequestsAllBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    step_id: row.try_get(2)?,
                    step_name: row.try_get(3)?,
                    method: row.try_get(4)?,
                    url: row.try_get(5)?,
                    resolved_url: row.try_get(6)?,
                    request_headers: row.try_get(7)?,
                    request_body: row.try_get(8)?,
                    status_code: row.try_get(9)?,
                    status_text: row.try_get(10)?,
                    response_headers: row.try_get(11)?,
                    response_time_ms: row.try_get(12)?,
                    response_body_type: row.try_get(13)?,
                    response_body: row.try_get(14)?,
                    response_size_bytes: row.try_get(15)?,
                    extractions: row.try_get(16)?,
                    assertions: row.try_get(17)?,
                    success: row.try_get(18)?,
                    error_message: row.try_get(19)?,
                    created_at: row.try_get(20)?,
                })
            },
            mapper: |it| GetTaskRunApiRequestsAll::from(it),
        }
    }
}
pub struct CreateTaskRunAwasStepStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_task_run_awas_step() -> CreateTaskRunAwasStepStmt {
    CreateTaskRunAwasStepStmt(
        "INSERT INTO task_run_awas_steps (id, task_run_id, step_id, step_name, step_type, url, action_id, parameters, response_data, success, error_message, duration_ms, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NOW())",
        None,
    )
}
impl CreateTaskRunAwasStepStmt {
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
        T10: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        task_run_id: &'a T2,
        step_id: &'a Option<T3>,
        step_name: &'a Option<T4>,
        step_type: &'a T5,
        url: &'a Option<T6>,
        action_id: &'a Option<T7>,
        parameters: &'a Option<T8>,
        response_data: &'a Option<T9>,
        success: &'a bool,
        error_message: &'a Option<T10>,
        duration_ms: &'a Option<i64>,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    id,
                    task_run_id,
                    step_id,
                    step_name,
                    step_type,
                    url,
                    action_id,
                    parameters,
                    response_data,
                    success,
                    error_message,
                    duration_ms,
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
    T10: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        CreateTaskRunAwasStepParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for CreateTaskRunAwasStepStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a CreateTaskRunAwasStepParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.step_id,
            &params.step_name,
            &params.step_type,
            &params.url,
            &params.action_id,
            &params.parameters,
            &params.response_data,
            &params.success,
            &params.error_message,
            &params.duration_ms,
        ))
    }
}
pub struct GetTaskRunAwasStepsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_awas_steps() -> GetTaskRunAwasStepsStmt {
    GetTaskRunAwasStepsStmt(
        "SELECT id, task_run_id, step_id, step_name, step_type, url, action_id, parameters, response_data, success, error_message, duration_ms, created_at FROM task_run_awas_steps WHERE task_run_id = $1 ORDER BY created_at ASC",
        None,
    )
}
impl GetTaskRunAwasStepsStmt {
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
    ) -> GetTaskRunAwasStepsQuery<'c, 'a, 's, C, GetTaskRunAwasSteps, 1> {
        GetTaskRunAwasStepsQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskRunAwasStepsBorrowed, tokio_postgres::Error> {
                Ok(GetTaskRunAwasStepsBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    step_id: row.try_get(2)?,
                    step_name: row.try_get(3)?,
                    step_type: row.try_get(4)?,
                    url: row.try_get(5)?,
                    action_id: row.try_get(6)?,
                    parameters: row.try_get(7)?,
                    response_data: row.try_get(8)?,
                    success: row.try_get(9)?,
                    error_message: row.try_get(10)?,
                    duration_ms: row.try_get(11)?,
                    created_at: row.try_get(12)?,
                })
            },
            mapper: |it| GetTaskRunAwasSteps::from(it),
        }
    }
}
