// This file was generated with `clorinde`. Do not modify.

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetScoresForRun {
    pub id: String,
    pub task_run_id: String,
    pub metric_type: String,
    pub score: f64,
    pub confidence: f64,
    pub rationale: String,
    pub is_llm_judged: bool,
    pub model_used: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetScoresForRunBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub metric_type: &'a str,
    pub score: f64,
    pub confidence: f64,
    pub rationale: &'a str,
    pub is_llm_judged: bool,
    pub model_used: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetScoresForRunBorrowed<'a>> for GetScoresForRun {
    fn from(
        GetScoresForRunBorrowed {
            id,
            task_run_id,
            metric_type,
            score,
            confidence,
            rationale,
            is_llm_judged,
            model_used,
            created_at,
        }: GetScoresForRunBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            metric_type: metric_type.into(),
            score,
            confidence,
            rationale: rationale.into(),
            is_llm_judged,
            model_used: model_used.into(),
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetMetricAggregates {
    pub metric_type: String,
    pub avg_score: f64,
    pub min_score: f64,
    pub max_score: f64,
    pub sample_count: i64,
}
pub struct GetMetricAggregatesBorrowed<'a> {
    pub metric_type: &'a str,
    pub avg_score: f64,
    pub min_score: f64,
    pub max_score: f64,
    pub sample_count: i64,
}
impl<'a> From<GetMetricAggregatesBorrowed<'a>> for GetMetricAggregates {
    fn from(
        GetMetricAggregatesBorrowed {
            metric_type,
            avg_score,
            min_score,
            max_score,
            sample_count,
        }: GetMetricAggregatesBorrowed<'a>,
    ) -> Self {
        Self {
            metric_type: metric_type.into(),
            avg_score,
            min_score,
            max_score,
            sample_count,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct GetCompositeScoreTrend {
    pub day: chrono::DateTime<chrono::FixedOffset>,
    pub avg_score: f64,
    pub run_count: i64,
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct GetScoresForRunQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetScoresForRunBorrowed, tokio_postgres::Error>,
    mapper: fn(GetScoresForRunBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetScoresForRunQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetScoresForRunBorrowed) -> R,
    ) -> GetScoresForRunQuery<'c, 'a, 's, C, R, N> {
        GetScoresForRunQuery {
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
pub struct GetMetricAggregatesQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetMetricAggregatesBorrowed, tokio_postgres::Error>,
    mapper: fn(GetMetricAggregatesBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetMetricAggregatesQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetMetricAggregatesBorrowed) -> R,
    ) -> GetMetricAggregatesQuery<'c, 'a, 's, C, R, N> {
        GetMetricAggregatesQuery {
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
pub struct GetCompositeScoreTrendQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetCompositeScoreTrend, tokio_postgres::Error>,
    mapper: fn(GetCompositeScoreTrend) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCompositeScoreTrendQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetCompositeScoreTrend) -> R,
    ) -> GetCompositeScoreTrendQuery<'c, 'a, 's, C, R, N> {
        GetCompositeScoreTrendQuery {
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
pub struct GetScoresForRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_scores_for_run() -> GetScoresForRunStmt {
    GetScoresForRunStmt(
        "SELECT id, task_run_id, metric_type, score, confidence, rationale, is_llm_judged, model_used, created_at FROM agentic_metric_scores WHERE task_run_id = $1 ORDER BY metric_type",
        None,
    )
}
impl GetScoresForRunStmt {
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
    ) -> GetScoresForRunQuery<'c, 'a, 's, C, GetScoresForRun, 1> {
        GetScoresForRunQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetScoresForRunBorrowed, tokio_postgres::Error> {
                Ok(GetScoresForRunBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    metric_type: row.try_get(2)?,
                    score: row.try_get(3)?,
                    confidence: row.try_get(4)?,
                    rationale: row.try_get(5)?,
                    is_llm_judged: row.try_get(6)?,
                    model_used: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                })
            },
            mapper: |it| GetScoresForRun::from(it),
        }
    }
}
pub struct GetMetricAggregatesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_metric_aggregates() -> GetMetricAggregatesStmt {
    GetMetricAggregatesStmt(
        "SELECT metric_type, AVG(score) as avg_score, MIN(score) as min_score, MAX(score) as max_score, COUNT(*)::bigint as sample_count FROM agentic_metric_scores WHERE created_at >= $1 GROUP BY metric_type ORDER BY metric_type",
        None,
    )
}
impl GetMetricAggregatesStmt {
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
        since: &'a chrono::DateTime<chrono::FixedOffset>,
    ) -> GetMetricAggregatesQuery<'c, 'a, 's, C, GetMetricAggregates, 1> {
        GetMetricAggregatesQuery {
            client,
            params: [since],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetMetricAggregatesBorrowed, tokio_postgres::Error> {
                Ok(GetMetricAggregatesBorrowed {
                    metric_type: row.try_get(0)?,
                    avg_score: row.try_get(1)?,
                    min_score: row.try_get(2)?,
                    max_score: row.try_get(3)?,
                    sample_count: row.try_get(4)?,
                })
            },
            mapper: |it| GetMetricAggregates::from(it),
        }
    }
}
pub struct GetLatestTaskRunIdStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_latest_task_run_id() -> GetLatestTaskRunIdStmt {
    GetLatestTaskRunIdStmt(
        "SELECT task_run_id FROM agentic_metric_scores ORDER BY created_at DESC LIMIT 1",
        None,
    )
}
impl GetLatestTaskRunIdStmt {
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
    ) -> StringQuery<'c, 'a, 's, C, String, 0> {
        StringQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct HasScoresStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn has_scores() -> HasScoresStmt {
    HasScoresStmt(
        "SELECT COUNT(*)::bigint as count FROM agentic_metric_scores WHERE task_run_id = $1",
        None,
    )
}
impl HasScoresStmt {
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
pub struct GetCompositeScoreTrendStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_composite_score_trend() -> GetCompositeScoreTrendStmt {
    GetCompositeScoreTrendStmt(
        "SELECT DATE_TRUNC('day', created_at) as day, AVG(score) as avg_score, COUNT(DISTINCT task_run_id)::bigint as run_count FROM agentic_metric_scores WHERE created_at >= $1 GROUP BY DATE_TRUNC('day', created_at) ORDER BY day",
        None,
    )
}
impl GetCompositeScoreTrendStmt {
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
        since: &'a chrono::DateTime<chrono::FixedOffset>,
    ) -> GetCompositeScoreTrendQuery<'c, 'a, 's, C, GetCompositeScoreTrend, 1> {
        GetCompositeScoreTrendQuery {
            client,
            params: [since],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetCompositeScoreTrend, tokio_postgres::Error> {
                Ok(GetCompositeScoreTrend {
                    day: row.try_get(0)?,
                    avg_score: row.try_get(1)?,
                    run_count: row.try_get(2)?,
                })
            },
            mapper: |it| GetCompositeScoreTrend::from(it),
        }
    }
}
