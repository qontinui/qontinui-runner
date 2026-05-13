// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreateWatcherParams<
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
    pub name: T2,
    pub schedule_json: T3,
    pub timeline_query: T4,
    pub app_name_filter: Option<T5>,
    pub source_type_filter: Option<T6>,
    pub lookback_window: T7,
    pub reasoning_prompt: T8,
    pub action_json: T9,
}
#[derive(Debug)]
pub struct UpdateWatcherParams<
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
    pub name: Option<T1>,
    pub schedule_json: Option<T2>,
    pub timeline_query: Option<T3>,
    pub app_name_filter: Option<T4>,
    pub source_type_filter: Option<T5>,
    pub lookback_window: Option<T6>,
    pub reasoning_prompt: Option<T7>,
    pub action_json: Option<T8>,
    pub enabled: Option<bool>,
    pub id: T9,
}
#[derive(Debug)]
pub struct RecordWatcherRunParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub last_result_json: Option<T1>,
    pub id: T2,
}
#[derive(Debug)]
pub struct SetWatcherEnabledParams<T1: crate::StringSql> {
    pub enabled: bool,
    pub id: T1,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WatcherRow {
    pub id: String,
    pub name: String,
    pub schedule_json: String,
    pub timeline_query: String,
    pub app_name_filter: Option<String>,
    pub source_type_filter: Option<String>,
    pub lookback_window: String,
    pub reasoning_prompt: String,
    pub action_json: String,
    pub enabled: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub last_result_json: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct WatcherRowBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub schedule_json: &'a str,
    pub timeline_query: &'a str,
    pub app_name_filter: Option<&'a str>,
    pub source_type_filter: Option<&'a str>,
    pub lookback_window: &'a str,
    pub reasoning_prompt: &'a str,
    pub action_json: &'a str,
    pub enabled: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub last_result_json: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<WatcherRowBorrowed<'a>> for WatcherRow {
    fn from(
        WatcherRowBorrowed {
            id,
            name,
            schedule_json,
            timeline_query,
            app_name_filter,
            source_type_filter,
            lookback_window,
            reasoning_prompt,
            action_json,
            enabled,
            last_run_at,
            last_result_json,
            created_at,
            updated_at,
        }: WatcherRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            schedule_json: schedule_json.into(),
            timeline_query: timeline_query.into(),
            app_name_filter: app_name_filter.map(|v| v.into()),
            source_type_filter: source_type_filter.map(|v| v.into()),
            lookback_window: lookback_window.into(),
            reasoning_prompt: reasoning_prompt.into(),
            action_json: action_json.into(),
            enabled,
            last_run_at,
            last_result_json: last_result_json.map(|v| v.into()),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListWatcherRow {
    pub id: String,
    pub name: String,
    pub schedule_json: String,
    pub timeline_query: String,
    pub app_name_filter: Option<String>,
    pub source_type_filter: Option<String>,
    pub lookback_window: String,
    pub reasoning_prompt: String,
    pub action_json: String,
    pub enabled: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub last_result_json: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListWatcherRowBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub schedule_json: &'a str,
    pub timeline_query: &'a str,
    pub app_name_filter: Option<&'a str>,
    pub source_type_filter: Option<&'a str>,
    pub lookback_window: &'a str,
    pub reasoning_prompt: &'a str,
    pub action_json: &'a str,
    pub enabled: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub last_result_json: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListWatcherRowBorrowed<'a>> for ListWatcherRow {
    fn from(
        ListWatcherRowBorrowed {
            id,
            name,
            schedule_json,
            timeline_query,
            app_name_filter,
            source_type_filter,
            lookback_window,
            reasoning_prompt,
            action_json,
            enabled,
            last_run_at,
            last_result_json,
            created_at,
            updated_at,
        }: ListWatcherRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            schedule_json: schedule_json.into(),
            timeline_query: timeline_query.into(),
            app_name_filter: app_name_filter.map(|v| v.into()),
            source_type_filter: source_type_filter.map(|v| v.into()),
            lookback_window: lookback_window.into(),
            reasoning_prompt: reasoning_prompt.into(),
            action_json: action_json.into(),
            enabled,
            last_run_at,
            last_result_json: last_result_json.map(|v| v.into()),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListEnabledWatcherRow {
    pub id: String,
    pub name: String,
    pub schedule_json: String,
    pub timeline_query: String,
    pub app_name_filter: Option<String>,
    pub source_type_filter: Option<String>,
    pub lookback_window: String,
    pub reasoning_prompt: String,
    pub action_json: String,
    pub enabled: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub last_result_json: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListEnabledWatcherRowBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub schedule_json: &'a str,
    pub timeline_query: &'a str,
    pub app_name_filter: Option<&'a str>,
    pub source_type_filter: Option<&'a str>,
    pub lookback_window: &'a str,
    pub reasoning_prompt: &'a str,
    pub action_json: &'a str,
    pub enabled: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub last_result_json: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListEnabledWatcherRowBorrowed<'a>> for ListEnabledWatcherRow {
    fn from(
        ListEnabledWatcherRowBorrowed {
            id,
            name,
            schedule_json,
            timeline_query,
            app_name_filter,
            source_type_filter,
            lookback_window,
            reasoning_prompt,
            action_json,
            enabled,
            last_run_at,
            last_result_json,
            created_at,
            updated_at,
        }: ListEnabledWatcherRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            schedule_json: schedule_json.into(),
            timeline_query: timeline_query.into(),
            app_name_filter: app_name_filter.map(|v| v.into()),
            source_type_filter: source_type_filter.map(|v| v.into()),
            lookback_window: lookback_window.into(),
            reasoning_prompt: reasoning_prompt.into(),
            action_json: action_json.into(),
            enabled,
            last_run_at,
            last_result_json: last_result_json.map(|v| v.into()),
            created_at,
            updated_at,
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
pub struct WatcherRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<WatcherRowBorrowed, tokio_postgres::Error>,
    mapper: fn(WatcherRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> WatcherRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(WatcherRowBorrowed) -> R,
    ) -> WatcherRowQuery<'c, 'a, 's, C, R, N> {
        WatcherRowQuery {
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
pub struct ListWatcherRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<ListWatcherRowBorrowed, tokio_postgres::Error>,
    mapper: fn(ListWatcherRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListWatcherRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListWatcherRowBorrowed) -> R,
    ) -> ListWatcherRowQuery<'c, 'a, 's, C, R, N> {
        ListWatcherRowQuery {
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
pub struct ListEnabledWatcherRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ListEnabledWatcherRowBorrowed, tokio_postgres::Error>,
    mapper: fn(ListEnabledWatcherRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListEnabledWatcherRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListEnabledWatcherRowBorrowed) -> R,
    ) -> ListEnabledWatcherRowQuery<'c, 'a, 's, C, R, N> {
        ListEnabledWatcherRowQuery {
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
pub struct CreateWatcherStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_watcher() -> CreateWatcherStmt {
    CreateWatcherStmt(
        "INSERT INTO watchers (id, name, schedule_json, timeline_query, app_name_filter, source_type_filter, lookback_window, reasoning_prompt, action_json) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        None,
    )
}
impl CreateWatcherStmt {
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
        id: &'a T1,
        name: &'a T2,
        schedule_json: &'a T3,
        timeline_query: &'a T4,
        app_name_filter: &'a Option<T5>,
        source_type_filter: &'a Option<T6>,
        lookback_window: &'a T7,
        reasoning_prompt: &'a T8,
        action_json: &'a T9,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        StringQuery {
            client,
            params: [
                id,
                name,
                schedule_json,
                timeline_query,
                app_name_filter,
                source_type_filter,
                lookback_window,
                reasoning_prompt,
                action_json,
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
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        CreateWatcherParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
        StringQuery<'c, 'a, 's, C, String, 9>,
        C,
    > for CreateWatcherStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateWatcherParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.schedule_json,
            &params.timeline_query,
            &params.app_name_filter,
            &params.source_type_filter,
            &params.lookback_window,
            &params.reasoning_prompt,
            &params.action_json,
        )
    }
}
pub struct GetWatcherStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_watcher() -> GetWatcherStmt {
    GetWatcherStmt(
        "SELECT id, name, schedule_json, timeline_query, app_name_filter, source_type_filter, lookback_window, reasoning_prompt, action_json, enabled, last_run_at, last_result_json, created_at, updated_at FROM watchers WHERE id = $1",
        None,
    )
}
impl GetWatcherStmt {
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
    ) -> WatcherRowQuery<'c, 'a, 's, C, WatcherRow, 1> {
        WatcherRowQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<WatcherRowBorrowed, tokio_postgres::Error> {
                    Ok(WatcherRowBorrowed {
                        id: row.try_get(0)?,
                        name: row.try_get(1)?,
                        schedule_json: row.try_get(2)?,
                        timeline_query: row.try_get(3)?,
                        app_name_filter: row.try_get(4)?,
                        source_type_filter: row.try_get(5)?,
                        lookback_window: row.try_get(6)?,
                        reasoning_prompt: row.try_get(7)?,
                        action_json: row.try_get(8)?,
                        enabled: row.try_get(9)?,
                        last_run_at: row.try_get(10)?,
                        last_result_json: row.try_get(11)?,
                        created_at: row.try_get(12)?,
                        updated_at: row.try_get(13)?,
                    })
                },
            mapper: |it| WatcherRow::from(it),
        }
    }
}
pub struct ListWatchersStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_watchers() -> ListWatchersStmt {
    ListWatchersStmt(
        "SELECT id, name, schedule_json, timeline_query, app_name_filter, source_type_filter, lookback_window, reasoning_prompt, action_json, enabled, last_run_at, last_result_json, created_at, updated_at FROM watchers ORDER BY created_at DESC",
        None,
    )
}
impl ListWatchersStmt {
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
    ) -> ListWatcherRowQuery<'c, 'a, 's, C, ListWatcherRow, 0> {
        ListWatcherRowQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListWatcherRowBorrowed, tokio_postgres::Error> {
                Ok(ListWatcherRowBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    schedule_json: row.try_get(2)?,
                    timeline_query: row.try_get(3)?,
                    app_name_filter: row.try_get(4)?,
                    source_type_filter: row.try_get(5)?,
                    lookback_window: row.try_get(6)?,
                    reasoning_prompt: row.try_get(7)?,
                    action_json: row.try_get(8)?,
                    enabled: row.try_get(9)?,
                    last_run_at: row.try_get(10)?,
                    last_result_json: row.try_get(11)?,
                    created_at: row.try_get(12)?,
                    updated_at: row.try_get(13)?,
                })
            },
            mapper: |it| ListWatcherRow::from(it),
        }
    }
}
pub struct ListEnabledWatchersStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_enabled_watchers() -> ListEnabledWatchersStmt {
    ListEnabledWatchersStmt(
        "SELECT id, name, schedule_json, timeline_query, app_name_filter, source_type_filter, lookback_window, reasoning_prompt, action_json, enabled, last_run_at, last_result_json, created_at, updated_at FROM watchers WHERE enabled = true ORDER BY created_at ASC",
        None,
    )
}
impl ListEnabledWatchersStmt {
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
    ) -> ListEnabledWatcherRowQuery<'c, 'a, 's, C, ListEnabledWatcherRow, 0> {
        ListEnabledWatcherRowQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListEnabledWatcherRowBorrowed, tokio_postgres::Error> {
                Ok(ListEnabledWatcherRowBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    schedule_json: row.try_get(2)?,
                    timeline_query: row.try_get(3)?,
                    app_name_filter: row.try_get(4)?,
                    source_type_filter: row.try_get(5)?,
                    lookback_window: row.try_get(6)?,
                    reasoning_prompt: row.try_get(7)?,
                    action_json: row.try_get(8)?,
                    enabled: row.try_get(9)?,
                    last_run_at: row.try_get(10)?,
                    last_result_json: row.try_get(11)?,
                    created_at: row.try_get(12)?,
                    updated_at: row.try_get(13)?,
                })
            },
            mapper: |it| ListEnabledWatcherRow::from(it),
        }
    }
}
pub struct UpdateWatcherStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_watcher() -> UpdateWatcherStmt {
    UpdateWatcherStmt(
        "UPDATE watchers SET name = COALESCE($1, name), schedule_json = COALESCE($2, schedule_json), timeline_query = COALESCE($3, timeline_query), app_name_filter = COALESCE($4, app_name_filter), source_type_filter = COALESCE($5, source_type_filter), lookback_window = COALESCE($6, lookback_window), reasoning_prompt = COALESCE($7, reasoning_prompt), action_json = COALESCE($8, action_json), enabled = COALESCE($9, enabled), updated_at = NOW() WHERE id = $10 RETURNING id",
        None,
    )
}
impl UpdateWatcherStmt {
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
        name: &'a Option<T1>,
        schedule_json: &'a Option<T2>,
        timeline_query: &'a Option<T3>,
        app_name_filter: &'a Option<T4>,
        source_type_filter: &'a Option<T5>,
        lookback_window: &'a Option<T6>,
        reasoning_prompt: &'a Option<T7>,
        action_json: &'a Option<T8>,
        enabled: &'a Option<bool>,
        id: &'a T9,
    ) -> StringQuery<'c, 'a, 's, C, String, 10> {
        StringQuery {
            client,
            params: [
                name,
                schedule_json,
                timeline_query,
                app_name_filter,
                source_type_filter,
                lookback_window,
                reasoning_prompt,
                action_json,
                enabled,
                id,
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
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
    T9: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateWatcherParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
        StringQuery<'c, 'a, 's, C, String, 10>,
        C,
    > for UpdateWatcherStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateWatcherParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    ) -> StringQuery<'c, 'a, 's, C, String, 10> {
        self.bind(
            client,
            &params.name,
            &params.schedule_json,
            &params.timeline_query,
            &params.app_name_filter,
            &params.source_type_filter,
            &params.lookback_window,
            &params.reasoning_prompt,
            &params.action_json,
            &params.enabled,
            &params.id,
        )
    }
}
pub struct RecordWatcherRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn record_watcher_run() -> RecordWatcherRunStmt {
    RecordWatcherRunStmt(
        "UPDATE watchers SET last_run_at = NOW(), last_result_json = $1, updated_at = NOW() WHERE id = $2",
        None,
    )
}
impl RecordWatcherRunStmt {
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
        last_result_json: &'a Option<T1>,
        id: &'a T2,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[last_result_json, id]).await
    }
}
impl<'a, C: GenericClient + Send + Sync, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        RecordWatcherRunParams<T1, T2>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for RecordWatcherRunStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a RecordWatcherRunParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(client, &params.last_result_json, &params.id))
    }
}
pub struct DeleteWatcherStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_watcher() -> DeleteWatcherStmt {
    DeleteWatcherStmt("DELETE FROM watchers WHERE id = $1 RETURNING id", None)
}
impl DeleteWatcherStmt {
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
pub struct SetWatcherEnabledStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn set_watcher_enabled() -> SetWatcherEnabledStmt {
    SetWatcherEnabledStmt(
        "UPDATE watchers SET enabled = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
        None,
    )
}
impl SetWatcherEnabledStmt {
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
        enabled: &'a bool,
        id: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [enabled, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SetWatcherEnabledParams<T1>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for SetWatcherEnabledStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SetWatcherEnabledParams<T1>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.enabled, &params.id)
    }
}
