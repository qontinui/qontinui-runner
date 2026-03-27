// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct UpsertQEntryParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub state_key: T1,
    pub action: T2,
    pub q_value: f64,
    pub visit_count: i32,
}
#[derive(Debug)]
pub struct DeleteQEntryParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub state_key: T1,
    pub action: T2,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAllQEntries {
    pub state_key: String,
    pub action: String,
    pub q_value: f64,
    pub visit_count: i32,
    pub last_updated: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetAllQEntriesBorrowed<'a> {
    pub state_key: &'a str,
    pub action: &'a str,
    pub q_value: f64,
    pub visit_count: i32,
    pub last_updated: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetAllQEntriesBorrowed<'a>> for GetAllQEntries {
    fn from(
        GetAllQEntriesBorrowed {
            state_key,
            action,
            q_value,
            visit_count,
            last_updated,
        }: GetAllQEntriesBorrowed<'a>,
    ) -> Self {
        Self {
            state_key: state_key.into(),
            action: action.into(),
            q_value,
            visit_count,
            last_updated,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetQEntriesForState {
    pub state_key: String,
    pub action: String,
    pub q_value: f64,
    pub visit_count: i32,
    pub last_updated: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetQEntriesForStateBorrowed<'a> {
    pub state_key: &'a str,
    pub action: &'a str,
    pub q_value: f64,
    pub visit_count: i32,
    pub last_updated: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetQEntriesForStateBorrowed<'a>> for GetQEntriesForState {
    fn from(
        GetQEntriesForStateBorrowed {
            state_key,
            action,
            q_value,
            visit_count,
            last_updated,
        }: GetQEntriesForStateBorrowed<'a>,
    ) -> Self {
        Self {
            state_key: state_key.into(),
            action: action.into(),
            q_value,
            visit_count,
            last_updated,
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
pub struct GetAllQEntriesQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetAllQEntriesBorrowed, tokio_postgres::Error>,
    mapper: fn(GetAllQEntriesBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetAllQEntriesQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetAllQEntriesBorrowed) -> R,
    ) -> GetAllQEntriesQuery<'c, 'a, 's, C, R, N> {
        GetAllQEntriesQuery {
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
pub struct GetQEntriesForStateQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetQEntriesForStateBorrowed, tokio_postgres::Error>,
    mapper: fn(GetQEntriesForStateBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetQEntriesForStateQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetQEntriesForStateBorrowed) -> R,
    ) -> GetQEntriesForStateQuery<'c, 'a, 's, C, R, N> {
        GetQEntriesForStateQuery {
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
pub struct UpsertQEntryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn upsert_q_entry() -> UpsertQEntryStmt {
    UpsertQEntryStmt(
        "INSERT INTO q_routing_table (state_key, action, q_value, visit_count, last_updated) VALUES ($1, $2, $3, $4, NOW()) ON CONFLICT(state_key, action) DO UPDATE SET q_value = EXCLUDED.q_value, visit_count = EXCLUDED.visit_count, last_updated = NOW() RETURNING state_key",
        None,
    )
}
impl UpsertQEntryStmt {
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
        state_key: &'a T1,
        action: &'a T2,
        q_value: &'a f64,
        visit_count: &'a i32,
    ) -> StringQuery<'c, 'a, 's, C, String, 4> {
        StringQuery {
            client,
            params: [state_key, action, q_value, visit_count],
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
        UpsertQEntryParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 4>,
        C,
    > for UpsertQEntryStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpsertQEntryParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 4> {
        self.bind(
            client,
            &params.state_key,
            &params.action,
            &params.q_value,
            &params.visit_count,
        )
    }
}
pub struct GetAllQEntriesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_all_q_entries() -> GetAllQEntriesStmt {
    GetAllQEntriesStmt(
        "SELECT state_key, action, q_value, visit_count, last_updated FROM q_routing_table ORDER BY state_key, action",
        None,
    )
}
impl GetAllQEntriesStmt {
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
    ) -> GetAllQEntriesQuery<'c, 'a, 's, C, GetAllQEntries, 0> {
        GetAllQEntriesQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetAllQEntriesBorrowed, tokio_postgres::Error> {
                Ok(GetAllQEntriesBorrowed {
                    state_key: row.try_get(0)?,
                    action: row.try_get(1)?,
                    q_value: row.try_get(2)?,
                    visit_count: row.try_get(3)?,
                    last_updated: row.try_get(4)?,
                })
            },
            mapper: |it| GetAllQEntries::from(it),
        }
    }
}
pub struct GetQEntriesForStateStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_q_entries_for_state() -> GetQEntriesForStateStmt {
    GetQEntriesForStateStmt(
        "SELECT state_key, action, q_value, visit_count, last_updated FROM q_routing_table WHERE state_key = $1 ORDER BY action",
        None,
    )
}
impl GetQEntriesForStateStmt {
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
        state_key: &'a T1,
    ) -> GetQEntriesForStateQuery<'c, 'a, 's, C, GetQEntriesForState, 1> {
        GetQEntriesForStateQuery {
            client,
            params: [state_key],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetQEntriesForStateBorrowed, tokio_postgres::Error> {
                Ok(GetQEntriesForStateBorrowed {
                    state_key: row.try_get(0)?,
                    action: row.try_get(1)?,
                    q_value: row.try_get(2)?,
                    visit_count: row.try_get(3)?,
                    last_updated: row.try_get(4)?,
                })
            },
            mapper: |it| GetQEntriesForState::from(it),
        }
    }
}
pub struct DeleteQEntryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_q_entry() -> DeleteQEntryStmt {
    DeleteQEntryStmt(
        "DELETE FROM q_routing_table WHERE state_key = $1 AND action = $2",
        None,
    )
}
impl DeleteQEntryStmt {
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
        state_key: &'a T1,
        action: &'a T2,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[state_key, action]).await
    }
}
impl<'a, C: GenericClient + Send + Sync, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        DeleteQEntryParams<T1, T2>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for DeleteQEntryStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a DeleteQEntryParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(client, &params.state_key, &params.action))
    }
}
pub struct ClearQTableStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn clear_q_table() -> ClearQTableStmt {
    ClearQTableStmt("DELETE FROM q_routing_table", None)
}
impl ClearQTableStmt {
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
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[]).await
    }
}
