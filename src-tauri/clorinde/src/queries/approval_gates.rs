// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct InsertApprovalGateParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
> {
    pub id: T1,
    pub task_run_id: T2,
    pub iteration: i32,
    pub prompt: T3,
    pub context_json: T4,
}
#[derive(Debug)]
pub struct ResolveApprovalGateParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
> {
    pub action: T1,
    pub comment: Option<T2>,
    pub status: T3,
    pub id: T4,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetApprovalGatesForTaskRun {
    pub id: String,
    pub task_run_id: String,
    pub iteration: i32,
    pub prompt: String,
    pub context_json: String,
    pub action: String,
    pub comment: String,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub resolved_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetApprovalGatesForTaskRunBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub iteration: i32,
    pub prompt: &'a str,
    pub context_json: &'a str,
    pub action: &'a str,
    pub comment: &'a str,
    pub status: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub resolved_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetApprovalGatesForTaskRunBorrowed<'a>> for GetApprovalGatesForTaskRun {
    fn from(
        GetApprovalGatesForTaskRunBorrowed {
            id,
            task_run_id,
            iteration,
            prompt,
            context_json,
            action,
            comment,
            status,
            created_at,
            resolved_at,
        }: GetApprovalGatesForTaskRunBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            iteration,
            prompt: prompt.into(),
            context_json: context_json.into(),
            action: action.into(),
            comment: comment.into(),
            status: status.into(),
            created_at,
            resolved_at,
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
pub struct GetApprovalGatesForTaskRunQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetApprovalGatesForTaskRunBorrowed, tokio_postgres::Error>,
    mapper: fn(GetApprovalGatesForTaskRunBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetApprovalGatesForTaskRunQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetApprovalGatesForTaskRunBorrowed) -> R,
    ) -> GetApprovalGatesForTaskRunQuery<'c, 'a, 's, C, R, N> {
        GetApprovalGatesForTaskRunQuery {
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
pub struct InsertApprovalGateStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_approval_gate() -> InsertApprovalGateStmt {
    InsertApprovalGateStmt(
        "INSERT INTO approval_gates (id, task_run_id, iteration, prompt, context_json, status) VALUES ($1, $2, $3, $4, $5, 'pending') RETURNING id",
        None,
    )
}
impl InsertApprovalGateStmt {
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
        task_run_id: &'a T2,
        iteration: &'a i32,
        prompt: &'a T3,
        context_json: &'a T4,
    ) -> StringQuery<'c, 'a, 's, C, String, 5> {
        StringQuery {
            client,
            params: [id, task_run_id, iteration, prompt, context_json],
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        InsertApprovalGateParams<T1, T2, T3, T4>,
        StringQuery<'c, 'a, 's, C, String, 5>,
        C,
    > for InsertApprovalGateStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a InsertApprovalGateParams<T1, T2, T3, T4>,
    ) -> StringQuery<'c, 'a, 's, C, String, 5> {
        self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.iteration,
            &params.prompt,
            &params.context_json,
        )
    }
}
pub struct ResolveApprovalGateStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn resolve_approval_gate() -> ResolveApprovalGateStmt {
    ResolveApprovalGateStmt(
        "UPDATE approval_gates SET action = $1, comment = $2, status = $3, resolved_at = NOW() WHERE id = $4 RETURNING id",
        None,
    )
}
impl ResolveApprovalGateStmt {
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
        action: &'a T1,
        comment: &'a Option<T2>,
        status: &'a T3,
        id: &'a T4,
    ) -> StringQuery<'c, 'a, 's, C, String, 4> {
        StringQuery {
            client,
            params: [action, comment, status, id],
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        ResolveApprovalGateParams<T1, T2, T3, T4>,
        StringQuery<'c, 'a, 's, C, String, 4>,
        C,
    > for ResolveApprovalGateStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a ResolveApprovalGateParams<T1, T2, T3, T4>,
    ) -> StringQuery<'c, 'a, 's, C, String, 4> {
        self.bind(
            client,
            &params.action,
            &params.comment,
            &params.status,
            &params.id,
        )
    }
}
pub struct GetApprovalGatesForTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_approval_gates_for_task_run() -> GetApprovalGatesForTaskRunStmt {
    GetApprovalGatesForTaskRunStmt(
        "SELECT id, task_run_id, iteration, prompt, context_json, COALESCE(action, '') as action, COALESCE(comment, '') as comment, status, created_at, COALESCE(resolved_at, NOW()) as resolved_at FROM approval_gates WHERE task_run_id = $1 ORDER BY created_at ASC",
        None,
    )
}
impl GetApprovalGatesForTaskRunStmt {
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
    ) -> GetApprovalGatesForTaskRunQuery<'c, 'a, 's, C, GetApprovalGatesForTaskRun, 1> {
        GetApprovalGatesForTaskRunQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetApprovalGatesForTaskRunBorrowed, tokio_postgres::Error> {
                Ok(GetApprovalGatesForTaskRunBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    iteration: row.try_get(2)?,
                    prompt: row.try_get(3)?,
                    context_json: row.try_get(4)?,
                    action: row.try_get(5)?,
                    comment: row.try_get(6)?,
                    status: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                    resolved_at: row.try_get(9)?,
                })
            },
            mapper: |it| GetApprovalGatesForTaskRun::from(it),
        }
    }
}
