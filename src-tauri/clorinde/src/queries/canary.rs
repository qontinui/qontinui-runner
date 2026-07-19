// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct StartCanaryParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
> {
    pub id: T1,
    pub recommendation_id: T2,
    pub percentage: i64,
    pub baseline_metrics_json: T3,
    pub canary_metrics_json: T4,
}
#[derive(Debug)]
pub struct UpdateCanaryMetricsParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
> {
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
    pub baseline_metrics_json: T1,
    pub canary_metrics_json: T2,
    pub id: T3,
}
#[derive(Debug)]
pub struct RecordCanaryRunParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
> {
    pub id: T1,
    pub canary_id: T2,
    pub is_canary: bool,
    pub task_run_id: Option<T3>,
    pub success: bool,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<f64>,
}
#[derive(Debug)]
pub struct CreateTemplateCanaryParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
> {
    pub id: T1,
    pub template_id: T2,
    pub baseline_version: i32,
    pub candidate_version: i32,
    pub traffic_percentage: f64,
    pub baseline_metrics_json: T3,
    pub candidate_metrics_json: T4,
}
#[derive(Debug)]
pub struct UpdateTemplateCanaryMetricsParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
> {
    pub baseline_metrics_json: T1,
    pub candidate_metrics_json: T2,
    pub id: T3,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StartCanary {
    pub id: String,
    pub recommendation_id: String,
    pub percentage: i64,
    pub status: String,
    pub start_date: chrono::DateTime<chrono::FixedOffset>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct StartCanaryBorrowed<'a> {
    pub id: &'a str,
    pub recommendation_id: &'a str,
    pub percentage: i64,
    pub status: &'a str,
    pub start_date: chrono::DateTime<chrono::FixedOffset>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<StartCanaryBorrowed<'a>> for StartCanary {
    fn from(
        StartCanaryBorrowed {
            id,
            recommendation_id,
            percentage,
            status,
            start_date,
            created_at,
        }: StartCanaryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            recommendation_id: recommendation_id.into(),
            percentage,
            status: status.into(),
            start_date,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetActiveCanaries {
    pub id: String,
    pub recommendation_id: String,
    pub percentage: i64,
    pub status: String,
    pub start_date: chrono::DateTime<chrono::FixedOffset>,
    pub end_date: chrono::DateTime<chrono::FixedOffset>,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
    pub baseline_metrics_json: String,
    pub canary_metrics_json: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetActiveCanariesBorrowed<'a> {
    pub id: &'a str,
    pub recommendation_id: &'a str,
    pub percentage: i64,
    pub status: &'a str,
    pub start_date: chrono::DateTime<chrono::FixedOffset>,
    pub end_date: chrono::DateTime<chrono::FixedOffset>,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
    pub baseline_metrics_json: &'a str,
    pub canary_metrics_json: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetActiveCanariesBorrowed<'a>> for GetActiveCanaries {
    fn from(
        GetActiveCanariesBorrowed {
            id,
            recommendation_id,
            percentage,
            status,
            start_date,
            end_date,
            baseline_run_count,
            canary_run_count,
            baseline_metrics_json,
            canary_metrics_json,
            created_at,
        }: GetActiveCanariesBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            recommendation_id: recommendation_id.into(),
            percentage,
            status: status.into(),
            start_date,
            end_date,
            baseline_run_count,
            canary_run_count,
            baseline_metrics_json: baseline_metrics_json.into(),
            canary_metrics_json: canary_metrics_json.into(),
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetCanaryHistory {
    pub id: String,
    pub recommendation_id: String,
    pub percentage: i64,
    pub status: String,
    pub start_date: chrono::DateTime<chrono::FixedOffset>,
    pub end_date: chrono::DateTime<chrono::FixedOffset>,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
    pub baseline_metrics_json: String,
    pub canary_metrics_json: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub recommendation_title: String,
    pub optimizer_type: String,
    pub target_agent: String,
}
pub struct GetCanaryHistoryBorrowed<'a> {
    pub id: &'a str,
    pub recommendation_id: &'a str,
    pub percentage: i64,
    pub status: &'a str,
    pub start_date: chrono::DateTime<chrono::FixedOffset>,
    pub end_date: chrono::DateTime<chrono::FixedOffset>,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
    pub baseline_metrics_json: &'a str,
    pub canary_metrics_json: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub recommendation_title: &'a str,
    pub optimizer_type: &'a str,
    pub target_agent: &'a str,
}
impl<'a> From<GetCanaryHistoryBorrowed<'a>> for GetCanaryHistory {
    fn from(
        GetCanaryHistoryBorrowed {
            id,
            recommendation_id,
            percentage,
            status,
            start_date,
            end_date,
            baseline_run_count,
            canary_run_count,
            baseline_metrics_json,
            canary_metrics_json,
            created_at,
            recommendation_title,
            optimizer_type,
            target_agent,
        }: GetCanaryHistoryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            recommendation_id: recommendation_id.into(),
            percentage,
            status: status.into(),
            start_date,
            end_date,
            baseline_run_count,
            canary_run_count,
            baseline_metrics_json: baseline_metrics_json.into(),
            canary_metrics_json: canary_metrics_json.into(),
            created_at,
            recommendation_title: recommendation_title.into(),
            optimizer_type: optimizer_type.into(),
            target_agent: target_agent.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetCanaryMetrics {
    pub id: String,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
    pub baseline_metrics_json: String,
    pub canary_metrics_json: String,
}
pub struct GetCanaryMetricsBorrowed<'a> {
    pub id: &'a str,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
    pub baseline_metrics_json: &'a str,
    pub canary_metrics_json: &'a str,
}
impl<'a> From<GetCanaryMetricsBorrowed<'a>> for GetCanaryMetrics {
    fn from(
        GetCanaryMetricsBorrowed {
            id,
            baseline_run_count,
            canary_run_count,
            baseline_metrics_json,
            canary_metrics_json,
        }: GetCanaryMetricsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            baseline_run_count,
            canary_run_count,
            baseline_metrics_json: baseline_metrics_json.into(),
            canary_metrics_json: canary_metrics_json.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UpdateCanaryMetrics {
    pub id: String,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
}
pub struct UpdateCanaryMetricsBorrowed<'a> {
    pub id: &'a str,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
}
impl<'a> From<UpdateCanaryMetricsBorrowed<'a>> for UpdateCanaryMetrics {
    fn from(
        UpdateCanaryMetricsBorrowed {
            id,
            baseline_run_count,
            canary_run_count,
        }: UpdateCanaryMetricsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            baseline_run_count,
            canary_run_count,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecordCanaryRun {
    pub id: String,
    pub canary_id: String,
    pub is_canary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct RecordCanaryRunBorrowed<'a> {
    pub id: &'a str,
    pub canary_id: &'a str,
    pub is_canary: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<RecordCanaryRunBorrowed<'a>> for RecordCanaryRun {
    fn from(
        RecordCanaryRunBorrowed {
            id,
            canary_id,
            is_canary,
            created_at,
        }: RecordCanaryRunBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            canary_id: canary_id.into(),
            is_canary,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PromoteCanary {
    pub id: String,
    pub recommendation_id: String,
    pub status: String,
    pub end_date: chrono::DateTime<chrono::FixedOffset>,
}
pub struct PromoteCanaryBorrowed<'a> {
    pub id: &'a str,
    pub recommendation_id: &'a str,
    pub status: &'a str,
    pub end_date: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<PromoteCanaryBorrowed<'a>> for PromoteCanary {
    fn from(
        PromoteCanaryBorrowed {
            id,
            recommendation_id,
            status,
            end_date,
        }: PromoteCanaryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            recommendation_id: recommendation_id.into(),
            status: status.into(),
            end_date,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RollbackCanary {
    pub id: String,
    pub recommendation_id: String,
    pub status: String,
    pub end_date: chrono::DateTime<chrono::FixedOffset>,
}
pub struct RollbackCanaryBorrowed<'a> {
    pub id: &'a str,
    pub recommendation_id: &'a str,
    pub status: &'a str,
    pub end_date: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<RollbackCanaryBorrowed<'a>> for RollbackCanary {
    fn from(
        RollbackCanaryBorrowed {
            id,
            recommendation_id,
            status,
            end_date,
        }: RollbackCanaryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            recommendation_id: recommendation_id.into(),
            status: status.into(),
            end_date,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CreateTemplateCanary {
    pub id: String,
    pub template_id: String,
    pub baseline_version: i32,
    pub candidate_version: i32,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct CreateTemplateCanaryBorrowed<'a> {
    pub id: &'a str,
    pub template_id: &'a str,
    pub baseline_version: i32,
    pub candidate_version: i32,
    pub status: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<CreateTemplateCanaryBorrowed<'a>> for CreateTemplateCanary {
    fn from(
        CreateTemplateCanaryBorrowed {
            id,
            template_id,
            baseline_version,
            candidate_version,
            status,
            created_at,
        }: CreateTemplateCanaryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            template_id: template_id.into(),
            baseline_version,
            candidate_version,
            status: status.into(),
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTemplateCanary {
    pub id: String,
    pub template_id: String,
    pub baseline_version: i32,
    pub candidate_version: i32,
    pub traffic_percentage: f64,
    pub status: String,
    pub baseline_metrics_json: String,
    pub candidate_metrics_json: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub ended_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTemplateCanaryBorrowed<'a> {
    pub id: &'a str,
    pub template_id: &'a str,
    pub baseline_version: i32,
    pub candidate_version: i32,
    pub traffic_percentage: f64,
    pub status: &'a str,
    pub baseline_metrics_json: &'a str,
    pub candidate_metrics_json: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub ended_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTemplateCanaryBorrowed<'a>> for GetTemplateCanary {
    fn from(
        GetTemplateCanaryBorrowed {
            id,
            template_id,
            baseline_version,
            candidate_version,
            traffic_percentage,
            status,
            baseline_metrics_json,
            candidate_metrics_json,
            created_at,
            ended_at,
        }: GetTemplateCanaryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            template_id: template_id.into(),
            baseline_version,
            candidate_version,
            traffic_percentage,
            status: status.into(),
            baseline_metrics_json: baseline_metrics_json.into(),
            candidate_metrics_json: candidate_metrics_json.into(),
            created_at,
            ended_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UpdateTemplateCanaryMetrics {
    pub id: String,
    pub template_id: String,
    pub status: String,
}
pub struct UpdateTemplateCanaryMetricsBorrowed<'a> {
    pub id: &'a str,
    pub template_id: &'a str,
    pub status: &'a str,
}
impl<'a> From<UpdateTemplateCanaryMetricsBorrowed<'a>> for UpdateTemplateCanaryMetrics {
    fn from(
        UpdateTemplateCanaryMetricsBorrowed {
            id,
            template_id,
            status,
        }: UpdateTemplateCanaryMetricsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            template_id: template_id.into(),
            status: status.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetActiveTemplateCanary {
    pub id: String,
    pub template_id: String,
    pub baseline_version: i32,
    pub candidate_version: i32,
    pub traffic_percentage: f64,
    pub status: String,
    pub baseline_metrics_json: String,
    pub candidate_metrics_json: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub ended_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetActiveTemplateCanaryBorrowed<'a> {
    pub id: &'a str,
    pub template_id: &'a str,
    pub baseline_version: i32,
    pub candidate_version: i32,
    pub traffic_percentage: f64,
    pub status: &'a str,
    pub baseline_metrics_json: &'a str,
    pub candidate_metrics_json: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub ended_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetActiveTemplateCanaryBorrowed<'a>> for GetActiveTemplateCanary {
    fn from(
        GetActiveTemplateCanaryBorrowed {
            id,
            template_id,
            baseline_version,
            candidate_version,
            traffic_percentage,
            status,
            baseline_metrics_json,
            candidate_metrics_json,
            created_at,
            ended_at,
        }: GetActiveTemplateCanaryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            template_id: template_id.into(),
            baseline_version,
            candidate_version,
            traffic_percentage,
            status: status.into(),
            baseline_metrics_json: baseline_metrics_json.into(),
            candidate_metrics_json: candidate_metrics_json.into(),
            created_at,
            ended_at,
        }
    }
}
use futures::{self, StreamExt, TryStreamExt};
use crate::client::async_::GenericClient;
pub struct StartCanaryQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<StartCanaryBorrowed, tokio_postgres::Error>,
    mapper: fn(StartCanaryBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> StartCanaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(StartCanaryBorrowed) -> R,
    ) -> StartCanaryQuery<'c, 'a, 's, C, R, N> {
        StartCanaryQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetActiveCanariesQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetActiveCanariesBorrowed, tokio_postgres::Error>,
    mapper: fn(GetActiveCanariesBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetActiveCanariesQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetActiveCanariesBorrowed) -> R,
    ) -> GetActiveCanariesQuery<'c, 'a, 's, C, R, N> {
        GetActiveCanariesQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetCanaryHistoryQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetCanaryHistoryBorrowed, tokio_postgres::Error>,
    mapper: fn(GetCanaryHistoryBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCanaryHistoryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetCanaryHistoryBorrowed) -> R,
    ) -> GetCanaryHistoryQuery<'c, 'a, 's, C, R, N> {
        GetCanaryHistoryQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetCanaryMetricsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetCanaryMetricsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetCanaryMetricsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCanaryMetricsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetCanaryMetricsBorrowed) -> R,
    ) -> GetCanaryMetricsQuery<'c, 'a, 's, C, R, N> {
        GetCanaryMetricsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct UpdateCanaryMetricsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<UpdateCanaryMetricsBorrowed, tokio_postgres::Error>,
    mapper: fn(UpdateCanaryMetricsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> UpdateCanaryMetricsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(UpdateCanaryMetricsBorrowed) -> R,
    ) -> UpdateCanaryMetricsQuery<'c, 'a, 's, C, R, N> {
        UpdateCanaryMetricsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct RecordCanaryRunQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<RecordCanaryRunBorrowed, tokio_postgres::Error>,
    mapper: fn(RecordCanaryRunBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> RecordCanaryRunQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(RecordCanaryRunBorrowed) -> R,
    ) -> RecordCanaryRunQuery<'c, 'a, 's, C, R, N> {
        RecordCanaryRunQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct PromoteCanaryQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<PromoteCanaryBorrowed, tokio_postgres::Error>,
    mapper: fn(PromoteCanaryBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> PromoteCanaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(PromoteCanaryBorrowed) -> R,
    ) -> PromoteCanaryQuery<'c, 'a, 's, C, R, N> {
        PromoteCanaryQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct RollbackCanaryQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<RollbackCanaryBorrowed, tokio_postgres::Error>,
    mapper: fn(RollbackCanaryBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> RollbackCanaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(RollbackCanaryBorrowed) -> R,
    ) -> RollbackCanaryQuery<'c, 'a, 's, C, R, N> {
        RollbackCanaryQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct CreateTemplateCanaryQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<CreateTemplateCanaryBorrowed, tokio_postgres::Error>,
    mapper: fn(CreateTemplateCanaryBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> CreateTemplateCanaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(CreateTemplateCanaryBorrowed) -> R,
    ) -> CreateTemplateCanaryQuery<'c, 'a, 's, C, R, N> {
        CreateTemplateCanaryQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetTemplateCanaryQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetTemplateCanaryBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTemplateCanaryBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTemplateCanaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTemplateCanaryBorrowed) -> R,
    ) -> GetTemplateCanaryQuery<'c, 'a, 's, C, R, N> {
        GetTemplateCanaryQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct UpdateTemplateCanaryMetricsQuery<
    'c,
    'a,
    's,
    C: GenericClient,
    T,
    const N: usize,
> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<UpdateTemplateCanaryMetricsBorrowed, tokio_postgres::Error>,
    mapper: fn(UpdateTemplateCanaryMetricsBorrowed) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> UpdateTemplateCanaryMetricsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(UpdateTemplateCanaryMetricsBorrowed) -> R,
    ) -> UpdateTemplateCanaryMetricsQuery<'c, 'a, 's, C, R, N> {
        UpdateTemplateCanaryMetricsQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct GetActiveTemplateCanaryQuery<
    'c,
    'a,
    's,
    C: GenericClient,
    T,
    const N: usize,
> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetActiveTemplateCanaryBorrowed, tokio_postgres::Error>,
    mapper: fn(GetActiveTemplateCanaryBorrowed) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> GetActiveTemplateCanaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetActiveTemplateCanaryBorrowed) -> R,
    ) -> GetActiveTemplateCanaryQuery<'c, 'a, 's, C, R, N> {
        GetActiveTemplateCanaryQuery {
            client: self.client,
            params: self.params,
            query: self.query,
            cached: self.cached,
            extractor: self.extractor,
            mapper,
        }
    }
    pub async fn one(self) -> Result<T, tokio_postgres::Error> {
        let row = crate::client::async_::one(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok((self.mapper)((self.extractor)(&row)?))
    }
    pub async fn all(self) -> Result<Vec<T>, tokio_postgres::Error> {
        self.iter().await?.try_collect().await
    }
    pub async fn opt(self) -> Result<Option<T>, tokio_postgres::Error> {
        let opt_row = crate::client::async_::opt(
                self.client,
                self.query,
                &self.params,
                self.cached,
            )
            .await?;
        Ok(
            opt_row
                .map(|row| {
                    let extracted = (self.extractor)(&row)?;
                    Ok((self.mapper)(extracted))
                })
                .transpose()?,
        )
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
                res
                    .and_then(|row| {
                        let extracted = (self.extractor)(&row)?;
                        Ok((self.mapper)(extracted))
                    })
            })
            .into_stream();
        Ok(mapped)
    }
}
pub struct StartCanaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn start_canary() -> StartCanaryStmt {
    StartCanaryStmt(
        "INSERT INTO canary_rollouts ( id, recommendation_id, percentage, status, start_date, baseline_metrics_json, canary_metrics_json ) VALUES ( $1, $2, $3, 'active', NOW(), $4, $5 ) RETURNING id, recommendation_id, percentage, status, start_date, created_at",
        None,
    )
}
impl StartCanaryStmt {
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
        id: &'a T1,
        recommendation_id: &'a T2,
        percentage: &'a i64,
        baseline_metrics_json: &'a T3,
        canary_metrics_json: &'a T4,
    ) -> StartCanaryQuery<'c, 'a, 's, C, StartCanary, 5> {
        StartCanaryQuery {
            client,
            params: [
                id,
                recommendation_id,
                percentage,
                baseline_metrics_json,
                canary_metrics_json,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<StartCanaryBorrowed, tokio_postgres::Error> {
                Ok(StartCanaryBorrowed {
                    id: row.try_get(0)?,
                    recommendation_id: row.try_get(1)?,
                    percentage: row.try_get(2)?,
                    status: row.try_get(3)?,
                    start_date: row.try_get(4)?,
                    created_at: row.try_get(5)?,
                })
            },
            mapper: |it| StartCanary::from(it),
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    StartCanaryParams<T1, T2, T3, T4>,
    StartCanaryQuery<'c, 'a, 's, C, StartCanary, 5>,
    C,
> for StartCanaryStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a StartCanaryParams<T1, T2, T3, T4>,
    ) -> StartCanaryQuery<'c, 'a, 's, C, StartCanary, 5> {
        self.bind(
            client,
            &params.id,
            &params.recommendation_id,
            &params.percentage,
            &params.baseline_metrics_json,
            &params.canary_metrics_json,
        )
    }
}
pub struct GetActiveCanariesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_active_canaries() -> GetActiveCanariesStmt {
    GetActiveCanariesStmt(
        "SELECT id, recommendation_id, percentage, status, start_date, COALESCE(end_date, NOW()) as end_date, COALESCE(baseline_run_count, 0) as baseline_run_count, COALESCE(canary_run_count, 0) as canary_run_count, COALESCE(baseline_metrics_json, '{}') as baseline_metrics_json, COALESCE(canary_metrics_json, '{}') as canary_metrics_json, created_at FROM canary_rollouts WHERE status = 'active' ORDER BY created_at DESC",
        None,
    )
}
impl GetActiveCanariesStmt {
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
    ) -> GetActiveCanariesQuery<'c, 'a, 's, C, GetActiveCanaries, 0> {
        GetActiveCanariesQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetActiveCanariesBorrowed, tokio_postgres::Error> {
                Ok(GetActiveCanariesBorrowed {
                    id: row.try_get(0)?,
                    recommendation_id: row.try_get(1)?,
                    percentage: row.try_get(2)?,
                    status: row.try_get(3)?,
                    start_date: row.try_get(4)?,
                    end_date: row.try_get(5)?,
                    baseline_run_count: row.try_get(6)?,
                    canary_run_count: row.try_get(7)?,
                    baseline_metrics_json: row.try_get(8)?,
                    canary_metrics_json: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                })
            },
            mapper: |it| GetActiveCanaries::from(it),
        }
    }
}
pub struct GetCanaryHistoryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_canary_history() -> GetCanaryHistoryStmt {
    GetCanaryHistoryStmt(
        "SELECT cr.id, cr.recommendation_id, cr.percentage, cr.status, cr.start_date, COALESCE(cr.end_date, NOW()) as end_date, COALESCE(cr.baseline_run_count, 0) as baseline_run_count, COALESCE(cr.canary_run_count, 0) as canary_run_count, COALESCE(cr.baseline_metrics_json, '{}') as baseline_metrics_json, COALESCE(cr.canary_metrics_json, '{}') as canary_metrics_json, cr.created_at, COALESCE(mor.title, '') as recommendation_title, COALESCE(mor.optimizer_type, '') as optimizer_type, COALESCE(mor.target_agent, '') as target_agent FROM canary_rollouts cr LEFT JOIN meta_optimizer_recommendations mor ON mor.id = cr.recommendation_id ORDER BY cr.created_at DESC LIMIT $1",
        None,
    )
}
impl GetCanaryHistoryStmt {
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
    ) -> GetCanaryHistoryQuery<'c, 'a, 's, C, GetCanaryHistory, 1> {
        GetCanaryHistoryQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetCanaryHistoryBorrowed, tokio_postgres::Error> {
                Ok(GetCanaryHistoryBorrowed {
                    id: row.try_get(0)?,
                    recommendation_id: row.try_get(1)?,
                    percentage: row.try_get(2)?,
                    status: row.try_get(3)?,
                    start_date: row.try_get(4)?,
                    end_date: row.try_get(5)?,
                    baseline_run_count: row.try_get(6)?,
                    canary_run_count: row.try_get(7)?,
                    baseline_metrics_json: row.try_get(8)?,
                    canary_metrics_json: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    recommendation_title: row.try_get(11)?,
                    optimizer_type: row.try_get(12)?,
                    target_agent: row.try_get(13)?,
                })
            },
            mapper: |it| GetCanaryHistory::from(it),
        }
    }
}
pub struct GetCanaryMetricsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_canary_metrics() -> GetCanaryMetricsStmt {
    GetCanaryMetricsStmt(
        "SELECT id, COALESCE(baseline_run_count, 0) as baseline_run_count, COALESCE(canary_run_count, 0) as canary_run_count, COALESCE(baseline_metrics_json, '{}') as baseline_metrics_json, COALESCE(canary_metrics_json, '{}') as canary_metrics_json FROM canary_rollouts WHERE id = $1",
        None,
    )
}
impl GetCanaryMetricsStmt {
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
    ) -> GetCanaryMetricsQuery<'c, 'a, 's, C, GetCanaryMetrics, 1> {
        GetCanaryMetricsQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetCanaryMetricsBorrowed, tokio_postgres::Error> {
                Ok(GetCanaryMetricsBorrowed {
                    id: row.try_get(0)?,
                    baseline_run_count: row.try_get(1)?,
                    canary_run_count: row.try_get(2)?,
                    baseline_metrics_json: row.try_get(3)?,
                    canary_metrics_json: row.try_get(4)?,
                })
            },
            mapper: |it| GetCanaryMetrics::from(it),
        }
    }
}
pub struct UpdateCanaryMetricsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_canary_metrics() -> UpdateCanaryMetricsStmt {
    UpdateCanaryMetricsStmt(
        "UPDATE canary_rollouts SET baseline_run_count = $1, canary_run_count = $2, baseline_metrics_json = $3, canary_metrics_json = $4 WHERE id = $5 RETURNING id, baseline_run_count, canary_run_count",
        None,
    )
}
impl UpdateCanaryMetricsStmt {
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
        baseline_run_count: &'a i64,
        canary_run_count: &'a i64,
        baseline_metrics_json: &'a T1,
        canary_metrics_json: &'a T2,
        id: &'a T3,
    ) -> UpdateCanaryMetricsQuery<'c, 'a, 's, C, UpdateCanaryMetrics, 5> {
        UpdateCanaryMetricsQuery {
            client,
            params: [
                baseline_run_count,
                canary_run_count,
                baseline_metrics_json,
                canary_metrics_json,
                id,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<UpdateCanaryMetricsBorrowed, tokio_postgres::Error> {
                Ok(UpdateCanaryMetricsBorrowed {
                    id: row.try_get(0)?,
                    baseline_run_count: row.try_get(1)?,
                    canary_run_count: row.try_get(2)?,
                })
            },
            mapper: |it| UpdateCanaryMetrics::from(it),
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    UpdateCanaryMetricsParams<T1, T2, T3>,
    UpdateCanaryMetricsQuery<'c, 'a, 's, C, UpdateCanaryMetrics, 5>,
    C,
> for UpdateCanaryMetricsStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateCanaryMetricsParams<T1, T2, T3>,
    ) -> UpdateCanaryMetricsQuery<'c, 'a, 's, C, UpdateCanaryMetrics, 5> {
        self.bind(
            client,
            &params.baseline_run_count,
            &params.canary_run_count,
            &params.baseline_metrics_json,
            &params.canary_metrics_json,
            &params.id,
        )
    }
}
pub struct RecordCanaryRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn record_canary_run() -> RecordCanaryRunStmt {
    RecordCanaryRunStmt(
        "INSERT INTO canary_run_records ( id, canary_id, is_canary, task_run_id, success, cost_usd, duration_ms ) VALUES ( $1, $2, $3, $4, $5, $6, $7 ) RETURNING id, canary_id, is_canary, created_at",
        None,
    )
}
impl RecordCanaryRunStmt {
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
        id: &'a T1,
        canary_id: &'a T2,
        is_canary: &'a bool,
        task_run_id: &'a Option<T3>,
        success: &'a bool,
        cost_usd: &'a Option<f64>,
        duration_ms: &'a Option<f64>,
    ) -> RecordCanaryRunQuery<'c, 'a, 's, C, RecordCanaryRun, 7> {
        RecordCanaryRunQuery {
            client,
            params: [
                id,
                canary_id,
                is_canary,
                task_run_id,
                success,
                cost_usd,
                duration_ms,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<RecordCanaryRunBorrowed, tokio_postgres::Error> {
                Ok(RecordCanaryRunBorrowed {
                    id: row.try_get(0)?,
                    canary_id: row.try_get(1)?,
                    is_canary: row.try_get(2)?,
                    created_at: row.try_get(3)?,
                })
            },
            mapper: |it| RecordCanaryRun::from(it),
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    RecordCanaryRunParams<T1, T2, T3>,
    RecordCanaryRunQuery<'c, 'a, 's, C, RecordCanaryRun, 7>,
    C,
> for RecordCanaryRunStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a RecordCanaryRunParams<T1, T2, T3>,
    ) -> RecordCanaryRunQuery<'c, 'a, 's, C, RecordCanaryRun, 7> {
        self.bind(
            client,
            &params.id,
            &params.canary_id,
            &params.is_canary,
            &params.task_run_id,
            &params.success,
            &params.cost_usd,
            &params.duration_ms,
        )
    }
}
pub struct PromoteCanaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn promote_canary() -> PromoteCanaryStmt {
    PromoteCanaryStmt(
        "UPDATE canary_rollouts SET status = 'promoted', end_date = NOW() WHERE id = $1 RETURNING id, recommendation_id, status, end_date",
        None,
    )
}
impl PromoteCanaryStmt {
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
    ) -> PromoteCanaryQuery<'c, 'a, 's, C, PromoteCanary, 1> {
        PromoteCanaryQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<PromoteCanaryBorrowed, tokio_postgres::Error> {
                Ok(PromoteCanaryBorrowed {
                    id: row.try_get(0)?,
                    recommendation_id: row.try_get(1)?,
                    status: row.try_get(2)?,
                    end_date: row.try_get(3)?,
                })
            },
            mapper: |it| PromoteCanary::from(it),
        }
    }
}
pub struct RollbackCanaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn rollback_canary() -> RollbackCanaryStmt {
    RollbackCanaryStmt(
        "UPDATE canary_rollouts SET status = 'rolled_back', end_date = NOW() WHERE id = $1 RETURNING id, recommendation_id, status, end_date",
        None,
    )
}
impl RollbackCanaryStmt {
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
    ) -> RollbackCanaryQuery<'c, 'a, 's, C, RollbackCanary, 1> {
        RollbackCanaryQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<RollbackCanaryBorrowed, tokio_postgres::Error> {
                Ok(RollbackCanaryBorrowed {
                    id: row.try_get(0)?,
                    recommendation_id: row.try_get(1)?,
                    status: row.try_get(2)?,
                    end_date: row.try_get(3)?,
                })
            },
            mapper: |it| RollbackCanary::from(it),
        }
    }
}
pub struct CreateTemplateCanaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_template_canary() -> CreateTemplateCanaryStmt {
    CreateTemplateCanaryStmt(
        "INSERT INTO prompt_template_canaries ( id, template_id, baseline_version, candidate_version, traffic_percentage, status, baseline_metrics_json, candidate_metrics_json ) VALUES ( $1, $2, $3, $4, $5, 'active', $6, $7 ) RETURNING id, template_id, baseline_version, candidate_version, status, created_at",
        None,
    )
}
impl CreateTemplateCanaryStmt {
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
        id: &'a T1,
        template_id: &'a T2,
        baseline_version: &'a i32,
        candidate_version: &'a i32,
        traffic_percentage: &'a f64,
        baseline_metrics_json: &'a T3,
        candidate_metrics_json: &'a T4,
    ) -> CreateTemplateCanaryQuery<'c, 'a, 's, C, CreateTemplateCanary, 7> {
        CreateTemplateCanaryQuery {
            client,
            params: [
                id,
                template_id,
                baseline_version,
                candidate_version,
                traffic_percentage,
                baseline_metrics_json,
                candidate_metrics_json,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<CreateTemplateCanaryBorrowed, tokio_postgres::Error> {
                Ok(CreateTemplateCanaryBorrowed {
                    id: row.try_get(0)?,
                    template_id: row.try_get(1)?,
                    baseline_version: row.try_get(2)?,
                    candidate_version: row.try_get(3)?,
                    status: row.try_get(4)?,
                    created_at: row.try_get(5)?,
                })
            },
            mapper: |it| CreateTemplateCanary::from(it),
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    CreateTemplateCanaryParams<T1, T2, T3, T4>,
    CreateTemplateCanaryQuery<'c, 'a, 's, C, CreateTemplateCanary, 7>,
    C,
> for CreateTemplateCanaryStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateTemplateCanaryParams<T1, T2, T3, T4>,
    ) -> CreateTemplateCanaryQuery<'c, 'a, 's, C, CreateTemplateCanary, 7> {
        self.bind(
            client,
            &params.id,
            &params.template_id,
            &params.baseline_version,
            &params.candidate_version,
            &params.traffic_percentage,
            &params.baseline_metrics_json,
            &params.candidate_metrics_json,
        )
    }
}
pub struct GetTemplateCanaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_template_canary() -> GetTemplateCanaryStmt {
    GetTemplateCanaryStmt(
        "SELECT id, template_id, baseline_version, candidate_version, traffic_percentage, status, COALESCE(baseline_metrics_json, '{}') as baseline_metrics_json, COALESCE(candidate_metrics_json, '{}') as candidate_metrics_json, created_at, COALESCE(ended_at, NOW()) as ended_at FROM prompt_template_canaries WHERE id = $1",
        None,
    )
}
impl GetTemplateCanaryStmt {
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
    ) -> GetTemplateCanaryQuery<'c, 'a, 's, C, GetTemplateCanary, 1> {
        GetTemplateCanaryQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTemplateCanaryBorrowed, tokio_postgres::Error> {
                Ok(GetTemplateCanaryBorrowed {
                    id: row.try_get(0)?,
                    template_id: row.try_get(1)?,
                    baseline_version: row.try_get(2)?,
                    candidate_version: row.try_get(3)?,
                    traffic_percentage: row.try_get(4)?,
                    status: row.try_get(5)?,
                    baseline_metrics_json: row.try_get(6)?,
                    candidate_metrics_json: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                    ended_at: row.try_get(9)?,
                })
            },
            mapper: |it| GetTemplateCanary::from(it),
        }
    }
}
pub struct UpdateTemplateCanaryMetricsStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn update_template_canary_metrics() -> UpdateTemplateCanaryMetricsStmt {
    UpdateTemplateCanaryMetricsStmt(
        "UPDATE prompt_template_canaries SET baseline_metrics_json = $1, candidate_metrics_json = $2 WHERE id = $3 RETURNING id, template_id, status",
        None,
    )
}
impl UpdateTemplateCanaryMetricsStmt {
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
        baseline_metrics_json: &'a T1,
        candidate_metrics_json: &'a T2,
        id: &'a T3,
    ) -> UpdateTemplateCanaryMetricsQuery<
        'c,
        'a,
        's,
        C,
        UpdateTemplateCanaryMetrics,
        3,
    > {
        UpdateTemplateCanaryMetricsQuery {
            client,
            params: [baseline_metrics_json, candidate_metrics_json, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<UpdateTemplateCanaryMetricsBorrowed, tokio_postgres::Error> {
                Ok(UpdateTemplateCanaryMetricsBorrowed {
                    id: row.try_get(0)?,
                    template_id: row.try_get(1)?,
                    status: row.try_get(2)?,
                })
            },
            mapper: |it| UpdateTemplateCanaryMetrics::from(it),
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    UpdateTemplateCanaryMetricsParams<T1, T2, T3>,
    UpdateTemplateCanaryMetricsQuery<'c, 'a, 's, C, UpdateTemplateCanaryMetrics, 3>,
    C,
> for UpdateTemplateCanaryMetricsStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateTemplateCanaryMetricsParams<T1, T2, T3>,
    ) -> UpdateTemplateCanaryMetricsQuery<
        'c,
        'a,
        's,
        C,
        UpdateTemplateCanaryMetrics,
        3,
    > {
        self.bind(
            client,
            &params.baseline_metrics_json,
            &params.candidate_metrics_json,
            &params.id,
        )
    }
}
pub struct GetActiveTemplateCanaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_active_template_canary() -> GetActiveTemplateCanaryStmt {
    GetActiveTemplateCanaryStmt(
        "SELECT id, template_id, baseline_version, candidate_version, traffic_percentage, status, COALESCE(baseline_metrics_json, '{}') as baseline_metrics_json, COALESCE(candidate_metrics_json, '{}') as candidate_metrics_json, created_at, COALESCE(ended_at, NOW()) as ended_at FROM prompt_template_canaries WHERE template_id = $1 AND status = 'active' LIMIT 1",
        None,
    )
}
impl GetActiveTemplateCanaryStmt {
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
        template_id: &'a T1,
    ) -> GetActiveTemplateCanaryQuery<'c, 'a, 's, C, GetActiveTemplateCanary, 1> {
        GetActiveTemplateCanaryQuery {
            client,
            params: [template_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetActiveTemplateCanaryBorrowed, tokio_postgres::Error> {
                Ok(GetActiveTemplateCanaryBorrowed {
                    id: row.try_get(0)?,
                    template_id: row.try_get(1)?,
                    baseline_version: row.try_get(2)?,
                    candidate_version: row.try_get(3)?,
                    traffic_percentage: row.try_get(4)?,
                    status: row.try_get(5)?,
                    baseline_metrics_json: row.try_get(6)?,
                    candidate_metrics_json: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                    ended_at: row.try_get(9)?,
                })
            },
            mapper: |it| GetActiveTemplateCanary::from(it),
        }
    }
}
