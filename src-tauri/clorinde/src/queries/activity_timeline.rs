// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct InsertActivityEntryParams<
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
    pub text_content: T1,
    pub content_hash: T2,
    pub source_type: T3,
    pub capture_mode: T4,
    pub app_name: Option<T5>,
    pub window_title: Option<T6>,
    pub url: Option<T7>,
    pub task_run_id: Option<T8>,
    pub screenshot_path: Option<T9>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub metadata_json: Option<T10>,
}
#[derive(Debug)]
pub struct SearchTimelineParams<T1: crate::StringSql> {
    pub query: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct SearchTimelineFilteredParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
> {
    pub query: T1,
    pub app_name: Option<T2>,
    pub source_type: Option<T3>,
    pub task_run_id: Option<T4>,
    pub max_results: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct GetTimelineRangeParams {
    pub start_time: chrono::DateTime<chrono::FixedOffset>,
    pub end_time: chrono::DateTime<chrono::FixedOffset>,
    pub max_results: i64,
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct FindRecentDuplicate {
    pub id: i64,
    pub duplicate_count: i32,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchTimelineRow {
    pub id: i64,
    pub text_preview: String,
    pub source_type: String,
    pub capture_mode: String,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub url: Option<String>,
    pub screenshot_path: Option<String>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: f32,
}
pub struct SearchTimelineRowBorrowed<'a> {
    pub id: i64,
    pub text_preview: &'a str,
    pub source_type: &'a str,
    pub capture_mode: &'a str,
    pub app_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub url: Option<&'a str>,
    pub screenshot_path: Option<&'a str>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: f32,
}
impl<'a> From<SearchTimelineRowBorrowed<'a>> for SearchTimelineRow {
    fn from(
        SearchTimelineRowBorrowed {
            id,
            text_preview,
            source_type,
            capture_mode,
            app_name,
            window_title,
            url,
            screenshot_path,
            element_count,
            confidence,
            created_at,
            rank,
        }: SearchTimelineRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            text_preview: text_preview.into(),
            source_type: source_type.into(),
            capture_mode: capture_mode.into(),
            app_name: app_name.map(|v| v.into()),
            window_title: window_title.map(|v| v.into()),
            url: url.map(|v| v.into()),
            screenshot_path: screenshot_path.map(|v| v.into()),
            element_count,
            confidence,
            created_at,
            rank,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchTimelineFilteredRow {
    pub id: i64,
    pub text_preview: String,
    pub source_type: String,
    pub capture_mode: String,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub url: Option<String>,
    pub screenshot_path: Option<String>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: f32,
}
pub struct SearchTimelineFilteredRowBorrowed<'a> {
    pub id: i64,
    pub text_preview: &'a str,
    pub source_type: &'a str,
    pub capture_mode: &'a str,
    pub app_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub url: Option<&'a str>,
    pub screenshot_path: Option<&'a str>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: f32,
}
impl<'a> From<SearchTimelineFilteredRowBorrowed<'a>> for SearchTimelineFilteredRow {
    fn from(
        SearchTimelineFilteredRowBorrowed {
            id,
            text_preview,
            source_type,
            capture_mode,
            app_name,
            window_title,
            url,
            screenshot_path,
            element_count,
            confidence,
            created_at,
            rank,
        }: SearchTimelineFilteredRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            text_preview: text_preview.into(),
            source_type: source_type.into(),
            capture_mode: capture_mode.into(),
            app_name: app_name.map(|v| v.into()),
            window_title: window_title.map(|v| v.into()),
            url: url.map(|v| v.into()),
            screenshot_path: screenshot_path.map(|v| v.into()),
            element_count,
            confidence,
            created_at,
            rank,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTimelineRangeRow {
    pub id: i64,
    pub text_preview: String,
    pub source_type: String,
    pub capture_mode: String,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub url: Option<String>,
    pub screenshot_path: Option<String>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTimelineRangeRowBorrowed<'a> {
    pub id: i64,
    pub text_preview: &'a str,
    pub source_type: &'a str,
    pub capture_mode: &'a str,
    pub app_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub url: Option<&'a str>,
    pub screenshot_path: Option<&'a str>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTimelineRangeRowBorrowed<'a>> for GetTimelineRangeRow {
    fn from(
        GetTimelineRangeRowBorrowed {
            id,
            text_preview,
            source_type,
            capture_mode,
            app_name,
            window_title,
            url,
            screenshot_path,
            element_count,
            confidence,
            created_at,
        }: GetTimelineRangeRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            text_preview: text_preview.into(),
            source_type: source_type.into(),
            capture_mode: capture_mode.into(),
            app_name: app_name.map(|v| v.into()),
            window_title: window_title.map(|v| v.into()),
            url: url.map(|v| v.into()),
            screenshot_path: screenshot_path.map(|v| v.into()),
            element_count,
            confidence,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTimelineEntryRow {
    pub id: i64,
    pub text_content: String,
    pub content_hash: String,
    pub source_type: String,
    pub capture_mode: String,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub url: Option<String>,
    pub task_run_id: Option<String>,
    pub screenshot_path: Option<String>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub metadata_json: Option<String>,
    pub duplicate_count: i32,
    pub is_deleted: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTimelineEntryRowBorrowed<'a> {
    pub id: i64,
    pub text_content: &'a str,
    pub content_hash: &'a str,
    pub source_type: &'a str,
    pub capture_mode: &'a str,
    pub app_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub url: Option<&'a str>,
    pub task_run_id: Option<&'a str>,
    pub screenshot_path: Option<&'a str>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub metadata_json: Option<&'a str>,
    pub duplicate_count: i32,
    pub is_deleted: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTimelineEntryRowBorrowed<'a>> for GetTimelineEntryRow {
    fn from(
        GetTimelineEntryRowBorrowed {
            id,
            text_content,
            content_hash,
            source_type,
            capture_mode,
            app_name,
            window_title,
            url,
            task_run_id,
            screenshot_path,
            element_count,
            confidence,
            metadata_json,
            duplicate_count,
            is_deleted,
            created_at,
        }: GetTimelineEntryRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            text_content: text_content.into(),
            content_hash: content_hash.into(),
            source_type: source_type.into(),
            capture_mode: capture_mode.into(),
            app_name: app_name.map(|v| v.into()),
            window_title: window_title.map(|v| v.into()),
            url: url.map(|v| v.into()),
            task_run_id: task_run_id.map(|v| v.into()),
            screenshot_path: screenshot_path.map(|v| v.into()),
            element_count,
            confidence,
            metadata_json: metadata_json.map(|v| v.into()),
            duplicate_count,
            is_deleted,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTimelineByTaskRunRow {
    pub id: i64,
    pub text_preview: String,
    pub source_type: String,
    pub capture_mode: String,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub url: Option<String>,
    pub screenshot_path: Option<String>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTimelineByTaskRunRowBorrowed<'a> {
    pub id: i64,
    pub text_preview: &'a str,
    pub source_type: &'a str,
    pub capture_mode: &'a str,
    pub app_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub url: Option<&'a str>,
    pub screenshot_path: Option<&'a str>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTimelineByTaskRunRowBorrowed<'a>> for GetTimelineByTaskRunRow {
    fn from(
        GetTimelineByTaskRunRowBorrowed {
            id,
            text_preview,
            source_type,
            capture_mode,
            app_name,
            window_title,
            url,
            screenshot_path,
            element_count,
            confidence,
            created_at,
        }: GetTimelineByTaskRunRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            text_preview: text_preview.into(),
            source_type: source_type.into(),
            capture_mode: capture_mode.into(),
            app_name: app_name.map(|v| v.into()),
            window_title: window_title.map(|v| v.into()),
            url: url.map(|v| v.into()),
            screenshot_path: screenshot_path.map(|v| v.into()),
            element_count,
            confidence,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTimelineStatsRow {
    pub source_type: String,
    pub capture_mode: String,
    pub count: i64,
    pub latest_capture: Option<chrono::DateTime<chrono::FixedOffset>>,
}
pub struct GetTimelineStatsRowBorrowed<'a> {
    pub source_type: &'a str,
    pub capture_mode: &'a str,
    pub count: i64,
    pub latest_capture: Option<chrono::DateTime<chrono::FixedOffset>>,
}
impl<'a> From<GetTimelineStatsRowBorrowed<'a>> for GetTimelineStatsRow {
    fn from(
        GetTimelineStatsRowBorrowed {
            source_type,
            capture_mode,
            count,
            latest_capture,
        }: GetTimelineStatsRowBorrowed<'a>,
    ) -> Self {
        Self {
            source_type: source_type.into(),
            capture_mode: capture_mode.into(),
            count,
            latest_capture,
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
pub struct FindRecentDuplicateQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<FindRecentDuplicate, tokio_postgres::Error>,
    mapper: fn(FindRecentDuplicate) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> FindRecentDuplicateQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(FindRecentDuplicate) -> R,
    ) -> FindRecentDuplicateQuery<'c, 'a, 's, C, R, N> {
        FindRecentDuplicateQuery {
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
pub struct SearchTimelineRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<SearchTimelineRowBorrowed, tokio_postgres::Error>,
    mapper: fn(SearchTimelineRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> SearchTimelineRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(SearchTimelineRowBorrowed) -> R,
    ) -> SearchTimelineRowQuery<'c, 'a, 's, C, R, N> {
        SearchTimelineRowQuery {
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
pub struct SearchTimelineFilteredRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<SearchTimelineFilteredRowBorrowed, tokio_postgres::Error>,
    mapper: fn(SearchTimelineFilteredRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> SearchTimelineFilteredRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(SearchTimelineFilteredRowBorrowed) -> R,
    ) -> SearchTimelineFilteredRowQuery<'c, 'a, 's, C, R, N> {
        SearchTimelineFilteredRowQuery {
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
pub struct GetTimelineRangeRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTimelineRangeRowBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTimelineRangeRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTimelineRangeRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTimelineRangeRowBorrowed) -> R,
    ) -> GetTimelineRangeRowQuery<'c, 'a, 's, C, R, N> {
        GetTimelineRangeRowQuery {
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
pub struct GetTimelineEntryRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTimelineEntryRowBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTimelineEntryRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTimelineEntryRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTimelineEntryRowBorrowed) -> R,
    ) -> GetTimelineEntryRowQuery<'c, 'a, 's, C, R, N> {
        GetTimelineEntryRowQuery {
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
pub struct GetTimelineByTaskRunRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTimelineByTaskRunRowBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTimelineByTaskRunRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTimelineByTaskRunRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTimelineByTaskRunRowBorrowed) -> R,
    ) -> GetTimelineByTaskRunRowQuery<'c, 'a, 's, C, R, N> {
        GetTimelineByTaskRunRowQuery {
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
pub struct GetTimelineStatsRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetTimelineStatsRowBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTimelineStatsRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTimelineStatsRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTimelineStatsRowBorrowed) -> R,
    ) -> GetTimelineStatsRowQuery<'c, 'a, 's, C, R, N> {
        GetTimelineStatsRowQuery {
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
pub struct InsertActivityEntryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_activity_entry() -> InsertActivityEntryStmt {
    InsertActivityEntryStmt(
        "INSERT INTO activity_timeline (text_content, content_hash, source_type, capture_mode, app_name, window_title, url, task_run_id, screenshot_path, element_count, confidence, metadata_json) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
        None,
    )
}
impl InsertActivityEntryStmt {
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
        text_content: &'a T1,
        content_hash: &'a T2,
        source_type: &'a T3,
        capture_mode: &'a T4,
        app_name: &'a Option<T5>,
        window_title: &'a Option<T6>,
        url: &'a Option<T7>,
        task_run_id: &'a Option<T8>,
        screenshot_path: &'a Option<T9>,
        element_count: &'a Option<i32>,
        confidence: &'a Option<f64>,
        metadata_json: &'a Option<T10>,
    ) -> I64Query<'c, 'a, 's, C, i64, 12> {
        I64Query {
            client,
            params: [
                text_content,
                content_hash,
                source_type,
                capture_mode,
                app_name,
                window_title,
                url,
                task_run_id,
                screenshot_path,
                element_count,
                confidence,
                metadata_json,
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
        InsertActivityEntryParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
        I64Query<'c, 'a, 's, C, i64, 12>,
        C,
    > for InsertActivityEntryStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a InsertActivityEntryParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
    ) -> I64Query<'c, 'a, 's, C, i64, 12> {
        self.bind(
            client,
            &params.text_content,
            &params.content_hash,
            &params.source_type,
            &params.capture_mode,
            &params.app_name,
            &params.window_title,
            &params.url,
            &params.task_run_id,
            &params.screenshot_path,
            &params.element_count,
            &params.confidence,
            &params.metadata_json,
        )
    }
}
pub struct FindRecentDuplicateStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn find_recent_duplicate() -> FindRecentDuplicateStmt {
    FindRecentDuplicateStmt(
        "SELECT id, duplicate_count FROM activity_timeline WHERE content_hash = $1 AND NOT is_deleted AND created_at > NOW() - INTERVAL '30 seconds' LIMIT 1",
        None,
    )
}
impl FindRecentDuplicateStmt {
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
    ) -> FindRecentDuplicateQuery<'c, 'a, 's, C, FindRecentDuplicate, 1> {
        FindRecentDuplicateQuery {
            client,
            params: [content_hash],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<FindRecentDuplicate, tokio_postgres::Error> {
                    Ok(FindRecentDuplicate {
                        id: row.try_get(0)?,
                        duplicate_count: row.try_get(1)?,
                    })
                },
            mapper: |it| FindRecentDuplicate::from(it),
        }
    }
}
pub struct IncrementDuplicateCountStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn increment_duplicate_count() -> IncrementDuplicateCountStmt {
    IncrementDuplicateCountStmt(
        "UPDATE activity_timeline SET duplicate_count = duplicate_count + 1 WHERE id = $1",
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
pub struct SearchTimelineStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_timeline() -> SearchTimelineStmt {
    SearchTimelineStmt(
        "SELECT id, LEFT(text_content, 500) as text_preview, source_type, capture_mode, app_name, window_title, url, screenshot_path, element_count, confidence, created_at, ts_rank(to_tsvector('english', text_content), plainto_tsquery('english', $1)) as rank FROM activity_timeline WHERE NOT is_deleted AND to_tsvector('english', text_content) @@ plainto_tsquery('english', $1) ORDER BY rank DESC LIMIT $2",
        None,
    )
}
impl SearchTimelineStmt {
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
    ) -> SearchTimelineRowQuery<'c, 'a, 's, C, SearchTimelineRow, 2> {
        SearchTimelineRowQuery {
            client,
            params: [query, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<SearchTimelineRowBorrowed, tokio_postgres::Error> {
                Ok(SearchTimelineRowBorrowed {
                    id: row.try_get(0)?,
                    text_preview: row.try_get(1)?,
                    source_type: row.try_get(2)?,
                    capture_mode: row.try_get(3)?,
                    app_name: row.try_get(4)?,
                    window_title: row.try_get(5)?,
                    url: row.try_get(6)?,
                    screenshot_path: row.try_get(7)?,
                    element_count: row.try_get(8)?,
                    confidence: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    rank: row.try_get(11)?,
                })
            },
            mapper: |it| SearchTimelineRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SearchTimelineParams<T1>,
        SearchTimelineRowQuery<'c, 'a, 's, C, SearchTimelineRow, 2>,
        C,
    > for SearchTimelineStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SearchTimelineParams<T1>,
    ) -> SearchTimelineRowQuery<'c, 'a, 's, C, SearchTimelineRow, 2> {
        self.bind(client, &params.query, &params.max_results)
    }
}
pub struct SearchTimelineFilteredStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_timeline_filtered() -> SearchTimelineFilteredStmt {
    SearchTimelineFilteredStmt(
        "SELECT id, LEFT(text_content, 500) as text_preview, source_type, capture_mode, app_name, window_title, url, screenshot_path, element_count, confidence, created_at, ts_rank(to_tsvector('english', text_content), plainto_tsquery('english', $1)) as rank FROM activity_timeline WHERE NOT is_deleted AND to_tsvector('english', text_content) @@ plainto_tsquery('english', $1) AND ($2::text IS NULL OR app_name = $2) AND ($3::text IS NULL OR source_type = $3) AND ($4::text IS NULL OR task_run_id = $4) ORDER BY rank DESC LIMIT $5",
        None,
    )
}
impl SearchTimelineFilteredStmt {
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
        query: &'a T1,
        app_name: &'a Option<T2>,
        source_type: &'a Option<T3>,
        task_run_id: &'a Option<T4>,
        max_results: &'a i64,
    ) -> SearchTimelineFilteredRowQuery<'c, 'a, 's, C, SearchTimelineFilteredRow, 5> {
        SearchTimelineFilteredRowQuery {
            client,
            params: [query, app_name, source_type, task_run_id, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<SearchTimelineFilteredRowBorrowed, tokio_postgres::Error> {
                Ok(SearchTimelineFilteredRowBorrowed {
                    id: row.try_get(0)?,
                    text_preview: row.try_get(1)?,
                    source_type: row.try_get(2)?,
                    capture_mode: row.try_get(3)?,
                    app_name: row.try_get(4)?,
                    window_title: row.try_get(5)?,
                    url: row.try_get(6)?,
                    screenshot_path: row.try_get(7)?,
                    element_count: row.try_get(8)?,
                    confidence: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    rank: row.try_get(11)?,
                })
            },
            mapper: |it| SearchTimelineFilteredRow::from(it),
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
        SearchTimelineFilteredParams<T1, T2, T3, T4>,
        SearchTimelineFilteredRowQuery<'c, 'a, 's, C, SearchTimelineFilteredRow, 5>,
        C,
    > for SearchTimelineFilteredStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SearchTimelineFilteredParams<T1, T2, T3, T4>,
    ) -> SearchTimelineFilteredRowQuery<'c, 'a, 's, C, SearchTimelineFilteredRow, 5> {
        self.bind(
            client,
            &params.query,
            &params.app_name,
            &params.source_type,
            &params.task_run_id,
            &params.max_results,
        )
    }
}
pub struct GetTimelineRangeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_timeline_range() -> GetTimelineRangeStmt {
    GetTimelineRangeStmt(
        "SELECT id, LEFT(text_content, 500) as text_preview, source_type, capture_mode, app_name, window_title, url, screenshot_path, element_count, confidence, created_at FROM activity_timeline WHERE NOT is_deleted AND created_at >= $1 AND created_at <= $2 ORDER BY created_at DESC LIMIT $3",
        None,
    )
}
impl GetTimelineRangeStmt {
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
        start_time: &'a chrono::DateTime<chrono::FixedOffset>,
        end_time: &'a chrono::DateTime<chrono::FixedOffset>,
        max_results: &'a i64,
    ) -> GetTimelineRangeRowQuery<'c, 'a, 's, C, GetTimelineRangeRow, 3> {
        GetTimelineRangeRowQuery {
            client,
            params: [start_time, end_time, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTimelineRangeRowBorrowed, tokio_postgres::Error> {
                Ok(GetTimelineRangeRowBorrowed {
                    id: row.try_get(0)?,
                    text_preview: row.try_get(1)?,
                    source_type: row.try_get(2)?,
                    capture_mode: row.try_get(3)?,
                    app_name: row.try_get(4)?,
                    window_title: row.try_get(5)?,
                    url: row.try_get(6)?,
                    screenshot_path: row.try_get(7)?,
                    element_count: row.try_get(8)?,
                    confidence: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                })
            },
            mapper: |it| GetTimelineRangeRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetTimelineRangeParams,
        GetTimelineRangeRowQuery<'c, 'a, 's, C, GetTimelineRangeRow, 3>,
        C,
    > for GetTimelineRangeStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetTimelineRangeParams,
    ) -> GetTimelineRangeRowQuery<'c, 'a, 's, C, GetTimelineRangeRow, 3> {
        self.bind(
            client,
            &params.start_time,
            &params.end_time,
            &params.max_results,
        )
    }
}
pub struct GetTimelineEntryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_timeline_entry() -> GetTimelineEntryStmt {
    GetTimelineEntryStmt(
        "SELECT id, text_content, content_hash, source_type, capture_mode, app_name, window_title, url, task_run_id, screenshot_path, element_count, confidence, metadata_json, duplicate_count, is_deleted, created_at FROM activity_timeline WHERE id = $1 AND NOT is_deleted",
        None,
    )
}
impl GetTimelineEntryStmt {
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
    ) -> GetTimelineEntryRowQuery<'c, 'a, 's, C, GetTimelineEntryRow, 1> {
        GetTimelineEntryRowQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTimelineEntryRowBorrowed, tokio_postgres::Error> {
                Ok(GetTimelineEntryRowBorrowed {
                    id: row.try_get(0)?,
                    text_content: row.try_get(1)?,
                    content_hash: row.try_get(2)?,
                    source_type: row.try_get(3)?,
                    capture_mode: row.try_get(4)?,
                    app_name: row.try_get(5)?,
                    window_title: row.try_get(6)?,
                    url: row.try_get(7)?,
                    task_run_id: row.try_get(8)?,
                    screenshot_path: row.try_get(9)?,
                    element_count: row.try_get(10)?,
                    confidence: row.try_get(11)?,
                    metadata_json: row.try_get(12)?,
                    duplicate_count: row.try_get(13)?,
                    is_deleted: row.try_get(14)?,
                    created_at: row.try_get(15)?,
                })
            },
            mapper: |it| GetTimelineEntryRow::from(it),
        }
    }
}
pub struct GetTimelineByTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_timeline_by_task_run() -> GetTimelineByTaskRunStmt {
    GetTimelineByTaskRunStmt(
        "SELECT id, LEFT(text_content, 500) as text_preview, source_type, capture_mode, app_name, window_title, url, screenshot_path, element_count, confidence, created_at FROM activity_timeline WHERE task_run_id = $1 AND NOT is_deleted ORDER BY created_at ASC",
        None,
    )
}
impl GetTimelineByTaskRunStmt {
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
    ) -> GetTimelineByTaskRunRowQuery<'c, 'a, 's, C, GetTimelineByTaskRunRow, 1> {
        GetTimelineByTaskRunRowQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTimelineByTaskRunRowBorrowed, tokio_postgres::Error> {
                Ok(GetTimelineByTaskRunRowBorrowed {
                    id: row.try_get(0)?,
                    text_preview: row.try_get(1)?,
                    source_type: row.try_get(2)?,
                    capture_mode: row.try_get(3)?,
                    app_name: row.try_get(4)?,
                    window_title: row.try_get(5)?,
                    url: row.try_get(6)?,
                    screenshot_path: row.try_get(7)?,
                    element_count: row.try_get(8)?,
                    confidence: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                })
            },
            mapper: |it| GetTimelineByTaskRunRow::from(it),
        }
    }
}
pub struct SoftDeleteTimelineEntryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn soft_delete_timeline_entry() -> SoftDeleteTimelineEntryStmt {
    SoftDeleteTimelineEntryStmt(
        "UPDATE activity_timeline SET is_deleted = true WHERE id = $1 RETURNING id",
        None,
    )
}
impl SoftDeleteTimelineEntryStmt {
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
pub struct GetTimelineStatsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_timeline_stats() -> GetTimelineStatsStmt {
    GetTimelineStatsStmt(
        "SELECT source_type, capture_mode, COUNT(*)::bigint as count, MAX(created_at) as latest_capture FROM activity_timeline WHERE NOT is_deleted GROUP BY source_type, capture_mode ORDER BY count DESC",
        None,
    )
}
impl GetTimelineStatsStmt {
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
    ) -> GetTimelineStatsRowQuery<'c, 'a, 's, C, GetTimelineStatsRow, 0> {
        GetTimelineStatsRowQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTimelineStatsRowBorrowed, tokio_postgres::Error> {
                Ok(GetTimelineStatsRowBorrowed {
                    source_type: row.try_get(0)?,
                    capture_mode: row.try_get(1)?,
                    count: row.try_get(2)?,
                    latest_capture: row.try_get(3)?,
                })
            },
            mapper: |it| GetTimelineStatsRow::from(it),
        }
    }
}
pub struct CleanupOldEntriesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn cleanup_old_entries() -> CleanupOldEntriesStmt {
    CleanupOldEntriesStmt(
        "DELETE FROM activity_timeline WHERE created_at < NOW() - make_interval(days => $1) AND is_deleted",
        None,
    )
}
impl CleanupOldEntriesStmt {
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
        retention_days: &'a i32,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[retention_days]).await
    }
}
