// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct QueueInsertParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
> {
    pub id: T1,
    pub workflow_id: T2,
    pub workflow_name: T3,
    pub priority: i32,
    pub error_message: Option<T4>,
    pub task_run_id: Option<T5>,
    pub max_retries: i32,
}
#[derive(Debug)]
pub struct QueueUpdateStatusParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub status: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct QueueSetCompletedParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub status: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct QueueSetErrorParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub error_message: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct QueueSetTaskRunIdParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub task_run_id: T1,
    pub id: T2,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueGet {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub queued_at: chrono::DateTime<chrono::FixedOffset>,
    pub priority: i32,
    pub status: String,
    pub started_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub error_message: String,
    pub task_run_id: String,
    pub retry_count: i32,
    pub max_retries: i32,
}
pub struct QueueGetBorrowed<'a> {
    pub id: &'a str,
    pub workflow_id: &'a str,
    pub workflow_name: &'a str,
    pub queued_at: chrono::DateTime<chrono::FixedOffset>,
    pub priority: i32,
    pub status: &'a str,
    pub started_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub error_message: &'a str,
    pub task_run_id: &'a str,
    pub retry_count: i32,
    pub max_retries: i32,
}
impl<'a> From<QueueGetBorrowed<'a>> for QueueGet {
    fn from(
        QueueGetBorrowed {
            id,
            workflow_id,
            workflow_name,
            queued_at,
            priority,
            status,
            started_at,
            completed_at,
            updated_at,
            error_message,
            task_run_id,
            retry_count,
            max_retries,
        }: QueueGetBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            workflow_id: workflow_id.into(),
            workflow_name: workflow_name.into(),
            queued_at,
            priority,
            status: status.into(),
            started_at,
            completed_at,
            updated_at,
            error_message: error_message.into(),
            task_run_id: task_run_id.into(),
            retry_count,
            max_retries,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueLoadPending {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub queued_at: chrono::DateTime<chrono::FixedOffset>,
    pub priority: i32,
    pub status: String,
    pub started_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub error_message: String,
    pub task_run_id: String,
    pub retry_count: i32,
    pub max_retries: i32,
}
pub struct QueueLoadPendingBorrowed<'a> {
    pub id: &'a str,
    pub workflow_id: &'a str,
    pub workflow_name: &'a str,
    pub queued_at: chrono::DateTime<chrono::FixedOffset>,
    pub priority: i32,
    pub status: &'a str,
    pub started_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub error_message: &'a str,
    pub task_run_id: &'a str,
    pub retry_count: i32,
    pub max_retries: i32,
}
impl<'a> From<QueueLoadPendingBorrowed<'a>> for QueueLoadPending {
    fn from(
        QueueLoadPendingBorrowed {
            id,
            workflow_id,
            workflow_name,
            queued_at,
            priority,
            status,
            started_at,
            completed_at,
            updated_at,
            error_message,
            task_run_id,
            retry_count,
            max_retries,
        }: QueueLoadPendingBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            workflow_id: workflow_id.into(),
            workflow_name: workflow_name.into(),
            queued_at,
            priority,
            status: status.into(),
            started_at,
            completed_at,
            updated_at,
            error_message: error_message.into(),
            task_run_id: task_run_id.into(),
            retry_count,
            max_retries,
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
pub struct QueueGetQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<QueueGetBorrowed, tokio_postgres::Error>,
    mapper: fn(QueueGetBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> QueueGetQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(self, mapper: fn(QueueGetBorrowed) -> R) -> QueueGetQuery<'c, 'a, 's, C, R, N> {
        QueueGetQuery {
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
pub struct QueueLoadPendingQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<QueueLoadPendingBorrowed, tokio_postgres::Error>,
    mapper: fn(QueueLoadPendingBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> QueueLoadPendingQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(QueueLoadPendingBorrowed) -> R,
    ) -> QueueLoadPendingQuery<'c, 'a, 's, C, R, N> {
        QueueLoadPendingQuery {
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
pub struct I32Query<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<i32, tokio_postgres::Error>,
    mapper: fn(i32) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> I32Query<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(self, mapper: fn(i32) -> R) -> I32Query<'c, 'a, 's, C, R, N> {
        I32Query {
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
pub struct QueueInsertStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_insert() -> QueueInsertStmt {
    QueueInsertStmt(
        "INSERT INTO queued_workflows (id, workflow_id, workflow_name, queued_at, priority, status, error_message, task_run_id, retry_count, max_retries) VALUES ($1, $2, $3, NOW(), $4, 'pending', $5, $6, 0, $7) ON CONFLICT(id) DO UPDATE SET status = EXCLUDED.status, updated_at = NOW() RETURNING id",
        None,
    )
}
impl QueueInsertStmt {
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
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        workflow_id: &'a T2,
        workflow_name: &'a T3,
        priority: &'a i32,
        error_message: &'a Option<T4>,
        task_run_id: &'a Option<T5>,
        max_retries: &'a i32,
    ) -> StringQuery<'c, 'a, 's, C, String, 7> {
        StringQuery {
            client,
            params: [
                id,
                workflow_id,
                workflow_name,
                priority,
                error_message,
                task_run_id,
                max_retries,
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
    >
    crate::client::async_::Params<
        'c,
        'a,
        's,
        QueueInsertParams<T1, T2, T3, T4, T5>,
        StringQuery<'c, 'a, 's, C, String, 7>,
        C,
    > for QueueInsertStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a QueueInsertParams<T1, T2, T3, T4, T5>,
    ) -> StringQuery<'c, 'a, 's, C, String, 7> {
        self.bind(
            client,
            &params.id,
            &params.workflow_id,
            &params.workflow_name,
            &params.priority,
            &params.error_message,
            &params.task_run_id,
            &params.max_retries,
        )
    }
}
pub struct QueueGetStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_get() -> QueueGetStmt {
    QueueGetStmt(
        "SELECT id, workflow_id, workflow_name, queued_at, priority, status, COALESCE(started_at, NOW()) as started_at, COALESCE(completed_at, NOW()) as completed_at, updated_at, COALESCE(error_message, '') as error_message, COALESCE(task_run_id, '') as task_run_id, retry_count, max_retries FROM queued_workflows WHERE id = $1",
        None,
    )
}
impl QueueGetStmt {
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
    ) -> QueueGetQuery<'c, 'a, 's, C, QueueGet, 1> {
        QueueGetQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<QueueGetBorrowed, tokio_postgres::Error> {
                    Ok(QueueGetBorrowed {
                        id: row.try_get(0)?,
                        workflow_id: row.try_get(1)?,
                        workflow_name: row.try_get(2)?,
                        queued_at: row.try_get(3)?,
                        priority: row.try_get(4)?,
                        status: row.try_get(5)?,
                        started_at: row.try_get(6)?,
                        completed_at: row.try_get(7)?,
                        updated_at: row.try_get(8)?,
                        error_message: row.try_get(9)?,
                        task_run_id: row.try_get(10)?,
                        retry_count: row.try_get(11)?,
                        max_retries: row.try_get(12)?,
                    })
                },
            mapper: |it| QueueGet::from(it),
        }
    }
}
pub struct QueueLoadPendingStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_load_pending() -> QueueLoadPendingStmt {
    QueueLoadPendingStmt(
        "SELECT id, workflow_id, workflow_name, queued_at, priority, status, COALESCE(started_at, NOW()) as started_at, COALESCE(completed_at, NOW()) as completed_at, updated_at, COALESCE(error_message, '') as error_message, COALESCE(task_run_id, '') as task_run_id, retry_count, max_retries FROM queued_workflows WHERE status IN ('pending', 'running') ORDER BY priority DESC, queued_at ASC",
        None,
    )
}
impl QueueLoadPendingStmt {
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
    ) -> QueueLoadPendingQuery<'c, 'a, 's, C, QueueLoadPending, 0> {
        QueueLoadPendingQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<QueueLoadPendingBorrowed, tokio_postgres::Error> {
                Ok(QueueLoadPendingBorrowed {
                    id: row.try_get(0)?,
                    workflow_id: row.try_get(1)?,
                    workflow_name: row.try_get(2)?,
                    queued_at: row.try_get(3)?,
                    priority: row.try_get(4)?,
                    status: row.try_get(5)?,
                    started_at: row.try_get(6)?,
                    completed_at: row.try_get(7)?,
                    updated_at: row.try_get(8)?,
                    error_message: row.try_get(9)?,
                    task_run_id: row.try_get(10)?,
                    retry_count: row.try_get(11)?,
                    max_retries: row.try_get(12)?,
                })
            },
            mapper: |it| QueueLoadPending::from(it),
        }
    }
}
pub struct QueueUpdateStatusStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_update_status() -> QueueUpdateStatusStmt {
    QueueUpdateStatusStmt(
        "UPDATE queued_workflows SET status = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl QueueUpdateStatusStmt {
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
        QueueUpdateStatusParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for QueueUpdateStatusStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a QueueUpdateStatusParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.status, &params.id)
    }
}
pub struct QueueSetStartedStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_set_started() -> QueueSetStartedStmt {
    QueueSetStartedStmt(
        "UPDATE queued_workflows SET started_at = NOW(), status = 'running', updated_at = NOW() WHERE id = $1 AND started_at IS NULL RETURNING id",
        None,
    )
}
impl QueueSetStartedStmt {
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
pub struct QueueSetCompletedStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_set_completed() -> QueueSetCompletedStmt {
    QueueSetCompletedStmt(
        "UPDATE queued_workflows SET completed_at = NOW(), status = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl QueueSetCompletedStmt {
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
        QueueSetCompletedParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for QueueSetCompletedStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a QueueSetCompletedParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.status, &params.id)
    }
}
pub struct QueueSetErrorStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_set_error() -> QueueSetErrorStmt {
    QueueSetErrorStmt(
        "UPDATE queued_workflows SET error_message = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl QueueSetErrorStmt {
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
        QueueSetErrorParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for QueueSetErrorStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a QueueSetErrorParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.error_message, &params.id)
    }
}
pub struct QueueSetTaskRunIdStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_set_task_run_id() -> QueueSetTaskRunIdStmt {
    QueueSetTaskRunIdStmt(
        "UPDATE queued_workflows SET task_run_id = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl QueueSetTaskRunIdStmt {
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
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [task_run_id, id],
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
        QueueSetTaskRunIdParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for QueueSetTaskRunIdStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a QueueSetTaskRunIdParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.task_run_id, &params.id)
    }
}
pub struct QueueIncrementRetryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_increment_retry() -> QueueIncrementRetryStmt {
    QueueIncrementRetryStmt(
        "UPDATE queued_workflows SET retry_count = retry_count + 1, status = 'pending', error_message = NULL, updated_at = NOW() WHERE id = $1 RETURNING retry_count",
        None,
    )
}
impl QueueIncrementRetryStmt {
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
    ) -> I32Query<'c, 'a, 's, C, i32, 1> {
        I32Query {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct QueueRecoverCrashedStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_recover_crashed() -> QueueRecoverCrashedStmt {
    QueueRecoverCrashedStmt(
        "UPDATE queued_workflows SET status = 'pending', started_at = NULL, updated_at = NOW() WHERE status = 'running' RETURNING id",
        None,
    )
}
impl QueueRecoverCrashedStmt {
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
pub struct QueueCleanupStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn queue_cleanup() -> QueueCleanupStmt {
    QueueCleanupStmt(
        "DELETE FROM queued_workflows WHERE status IN ('completed', 'failed', 'cancelled') AND completed_at < $1 RETURNING id",
        None,
    )
}
impl QueueCleanupStmt {
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
        cutoff: &'a chrono::DateTime<chrono::FixedOffset>,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [cutoff],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
