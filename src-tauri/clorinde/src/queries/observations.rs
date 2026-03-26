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
pub struct SaveObservationStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_observation() -> SaveObservationStmt {
    SaveObservationStmt(
        "INSERT INTO observations (title, content, observation_type, scope, topic_key, content_hash, project_id, workflow_id, task_run_id, session_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
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
pub struct UpsertObservationByTopicKeyStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn upsert_observation_by_topic_key() -> UpsertObservationByTopicKeyStmt {
    UpsertObservationByTopicKeyStmt(
        "INSERT INTO observations (title, content, observation_type, scope, topic_key, content_hash, project_id, workflow_id, task_run_id, session_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (topic_key) WHERE topic_key IS NOT NULL AND NOT is_deleted DO UPDATE SET title = EXCLUDED.title, content = EXCLUDED.content, content_hash = EXCLUDED.content_hash, observation_type = EXCLUDED.observation_type, revision_count = observations.revision_count + 1, updated_at = NOW() RETURNING id",
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
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, project_id, workflow_id, task_run_id, session_id, is_deleted, created_at, updated_at FROM observations WHERE id = $1 AND NOT is_deleted",
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
                    created_at: row.try_get(14)?,
                    updated_at: row.try_get(15)?,
                })
            },
            mapper: |it| GetObservation::from(it),
        }
    }
}
pub struct SearchObservationsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_observations() -> SearchObservationsStmt {
    SearchObservationsStmt(
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, created_at, updated_at, ts_rank(to_tsvector('english', title || ' ' || content), plainto_tsquery('english', $1)) as rank FROM observations WHERE NOT is_deleted AND to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', $1) ORDER BY rank DESC LIMIT $2",
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
                    created_at: row.try_get(8)?,
                    updated_at: row.try_get(9)?,
                    rank: row.try_get(10)?,
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
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, created_at, updated_at, ts_rank(to_tsvector('english', title || ' ' || content), plainto_tsquery('english', $1)) as rank FROM observations WHERE NOT is_deleted AND project_id = $2 AND to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', $1) ORDER BY rank DESC LIMIT $3",
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
                    created_at: row.try_get(8)?,
                    updated_at: row.try_get(9)?,
                    rank: row.try_get(10)?,
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
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, created_at, updated_at FROM observations WHERE NOT is_deleted AND (project_id = $1 OR scope = 'global') AND ($2::text IS NULL OR observation_type = $2) ORDER BY updated_at DESC LIMIT $3",
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
                    created_at: row.try_get(8)?,
                    updated_at: row.try_get(9)?,
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
        "SELECT id, title, LEFT(content, 300) as content_preview, observation_type, scope, topic_key, revision_count, project_id, created_at, updated_at FROM observations WHERE task_run_id = $1 AND NOT is_deleted ORDER BY created_at ASC",
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
                    created_at: row.try_get(8)?,
                    updated_at: row.try_get(9)?,
                })
            },
            mapper: |it| ObservationPreview::from(it),
        }
    }
}
pub struct GetAllObservationsFullStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_all_observations_full() -> GetAllObservationsFullStmt {
    GetAllObservationsFullStmt(
        "SELECT id, title, content, observation_type, scope, topic_key, content_hash, revision_count, duplicate_count, project_id, workflow_id, task_run_id, session_id, is_deleted, created_at, updated_at FROM observations WHERE NOT is_deleted ORDER BY updated_at DESC LIMIT $1",
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
                    created_at: row.try_get(14)?,
                    updated_at: row.try_get(15)?,
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
