// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreateCheckParams<
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
    pub name: T2,
    pub description: Option<T3>,
    pub check_type: T4,
    pub tool: T5,
    pub command: Option<T6>,
    pub working_directory: Option<T7>,
    pub config_path: Option<T8>,
    pub auto_fix: bool,
    pub fail_on_warning: bool,
    pub timeout_seconds: Option<i32>,
    pub is_critical: bool,
    pub enabled: bool,
    pub ai_generated: bool,
    pub ai_generation_prompt: Option<T9>,
    pub tags: T10,
}
#[derive(Debug)]
pub struct UpdateCheckParams<
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
    pub name: Option<T1>,
    pub description: Option<T2>,
    pub check_type: Option<T3>,
    pub tool: Option<T4>,
    pub command: Option<T5>,
    pub working_directory: Option<T6>,
    pub config_path: Option<T7>,
    pub auto_fix: Option<bool>,
    pub fail_on_warning: Option<bool>,
    pub timeout_seconds: Option<i32>,
    pub is_critical: Option<bool>,
    pub enabled: Option<bool>,
    pub ai_generation_prompt: Option<T8>,
    pub tags: Option<T9>,
    pub id: T10,
}
#[derive(Debug)]
pub struct CreateCheckGroupParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
> {
    pub id: T1,
    pub name: T2,
    pub description: Option<T3>,
    pub color: Option<T4>,
    pub enabled: bool,
    pub run_in_parallel: bool,
    pub stop_on_failure: bool,
    pub tags: T5,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListChecks {
    pub id: String,
    pub name: String,
    pub description: String,
    pub check_type: String,
    pub tool: String,
    pub command: String,
    pub working_directory: String,
    pub config_path: String,
    pub auto_fix: bool,
    pub fail_on_warning: bool,
    pub timeout_seconds: i32,
    pub is_critical: bool,
    pub enabled: bool,
    pub ai_generated: bool,
    pub ai_generation_prompt: String,
    pub tags: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListChecksBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub check_type: &'a str,
    pub tool: &'a str,
    pub command: &'a str,
    pub working_directory: &'a str,
    pub config_path: &'a str,
    pub auto_fix: bool,
    pub fail_on_warning: bool,
    pub timeout_seconds: i32,
    pub is_critical: bool,
    pub enabled: bool,
    pub ai_generated: bool,
    pub ai_generation_prompt: &'a str,
    pub tags: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListChecksBorrowed<'a>> for ListChecks {
    fn from(
        ListChecksBorrowed {
            id,
            name,
            description,
            check_type,
            tool,
            command,
            working_directory,
            config_path,
            auto_fix,
            fail_on_warning,
            timeout_seconds,
            is_critical,
            enabled,
            ai_generated,
            ai_generation_prompt,
            tags,
            created_at,
            updated_at,
        }: ListChecksBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            check_type: check_type.into(),
            tool: tool.into(),
            command: command.into(),
            working_directory: working_directory.into(),
            config_path: config_path.into(),
            auto_fix,
            fail_on_warning,
            timeout_seconds,
            is_critical,
            enabled,
            ai_generated,
            ai_generation_prompt: ai_generation_prompt.into(),
            tags: tags.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetCheck {
    pub id: String,
    pub name: String,
    pub description: String,
    pub check_type: String,
    pub tool: String,
    pub command: String,
    pub working_directory: String,
    pub config_path: String,
    pub auto_fix: bool,
    pub fail_on_warning: bool,
    pub timeout_seconds: i32,
    pub is_critical: bool,
    pub enabled: bool,
    pub ai_generated: bool,
    pub ai_generation_prompt: String,
    pub tags: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetCheckBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub check_type: &'a str,
    pub tool: &'a str,
    pub command: &'a str,
    pub working_directory: &'a str,
    pub config_path: &'a str,
    pub auto_fix: bool,
    pub fail_on_warning: bool,
    pub timeout_seconds: i32,
    pub is_critical: bool,
    pub enabled: bool,
    pub ai_generated: bool,
    pub ai_generation_prompt: &'a str,
    pub tags: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetCheckBorrowed<'a>> for GetCheck {
    fn from(
        GetCheckBorrowed {
            id,
            name,
            description,
            check_type,
            tool,
            command,
            working_directory,
            config_path,
            auto_fix,
            fail_on_warning,
            timeout_seconds,
            is_critical,
            enabled,
            ai_generated,
            ai_generation_prompt,
            tags,
            created_at,
            updated_at,
        }: GetCheckBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            check_type: check_type.into(),
            tool: tool.into(),
            command: command.into(),
            working_directory: working_directory.into(),
            config_path: config_path.into(),
            auto_fix,
            fail_on_warning,
            timeout_seconds,
            is_critical,
            enabled,
            ai_generated,
            ai_generation_prompt: ai_generation_prompt.into(),
            tags: tags.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetCheckGroup {
    pub id: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub enabled: bool,
    pub run_in_parallel: bool,
    pub stop_on_failure: bool,
    pub tags: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetCheckGroupBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub color: &'a str,
    pub enabled: bool,
    pub run_in_parallel: bool,
    pub stop_on_failure: bool,
    pub tags: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetCheckGroupBorrowed<'a>> for GetCheckGroup {
    fn from(
        GetCheckGroupBorrowed {
            id,
            name,
            description,
            color,
            enabled,
            run_in_parallel,
            stop_on_failure,
            tags,
            created_at,
            updated_at,
        }: GetCheckGroupBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            color: color.into(),
            enabled,
            run_in_parallel,
            stop_on_failure,
            tags: tags.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetChecksInGroup {
    pub id: String,
    pub name: String,
    pub description: String,
    pub check_type: String,
    pub tool: String,
    pub command: String,
    pub working_directory: String,
    pub config_path: String,
    pub auto_fix: bool,
    pub fail_on_warning: bool,
    pub timeout_seconds: i32,
    pub is_critical: bool,
    pub enabled: bool,
    pub ai_generated: bool,
    pub ai_generation_prompt: String,
    pub tags: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetChecksInGroupBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub check_type: &'a str,
    pub tool: &'a str,
    pub command: &'a str,
    pub working_directory: &'a str,
    pub config_path: &'a str,
    pub auto_fix: bool,
    pub fail_on_warning: bool,
    pub timeout_seconds: i32,
    pub is_critical: bool,
    pub enabled: bool,
    pub ai_generated: bool,
    pub ai_generation_prompt: &'a str,
    pub tags: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetChecksInGroupBorrowed<'a>> for GetChecksInGroup {
    fn from(
        GetChecksInGroupBorrowed {
            id,
            name,
            description,
            check_type,
            tool,
            command,
            working_directory,
            config_path,
            auto_fix,
            fail_on_warning,
            timeout_seconds,
            is_critical,
            enabled,
            ai_generated,
            ai_generation_prompt,
            tags,
            created_at,
            updated_at,
        }: GetChecksInGroupBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            check_type: check_type.into(),
            tool: tool.into(),
            command: command.into(),
            working_directory: working_directory.into(),
            config_path: config_path.into(),
            auto_fix,
            fail_on_warning,
            timeout_seconds,
            is_critical,
            enabled,
            ai_generated,
            ai_generation_prompt: ai_generation_prompt.into(),
            tags: tags.into(),
            created_at,
            updated_at,
        }
    }
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct ListChecksQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<ListChecksBorrowed, tokio_postgres::Error>,
    mapper: fn(ListChecksBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListChecksQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListChecksBorrowed) -> R,
    ) -> ListChecksQuery<'c, 'a, 's, C, R, N> {
        ListChecksQuery {
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
pub struct GetCheckQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetCheckBorrowed, tokio_postgres::Error>,
    mapper: fn(GetCheckBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCheckQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(self, mapper: fn(GetCheckBorrowed) -> R) -> GetCheckQuery<'c, 'a, 's, C, R, N> {
        GetCheckQuery {
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
pub struct GetCheckGroupQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetCheckGroupBorrowed, tokio_postgres::Error>,
    mapper: fn(GetCheckGroupBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCheckGroupQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetCheckGroupBorrowed) -> R,
    ) -> GetCheckGroupQuery<'c, 'a, 's, C, R, N> {
        GetCheckGroupQuery {
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
pub struct GetChecksInGroupQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetChecksInGroupBorrowed, tokio_postgres::Error>,
    mapper: fn(GetChecksInGroupBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetChecksInGroupQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetChecksInGroupBorrowed) -> R,
    ) -> GetChecksInGroupQuery<'c, 'a, 's, C, R, N> {
        GetChecksInGroupQuery {
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
pub struct ListChecksStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_checks() -> ListChecksStmt {
    ListChecksStmt(
        "SELECT id, name, description, check_type, tool, command, working_directory, config_path, auto_fix, fail_on_warning, timeout_seconds, is_critical, enabled, ai_generated, ai_generation_prompt, tags, created_at, updated_at FROM checks ORDER BY updated_at DESC",
        None,
    )
}
impl ListChecksStmt {
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
    ) -> ListChecksQuery<'c, 'a, 's, C, ListChecks, 0> {
        ListChecksQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<ListChecksBorrowed, tokio_postgres::Error> {
                    Ok(ListChecksBorrowed {
                        id: row.try_get(0)?,
                        name: row.try_get(1)?,
                        description: row.try_get(2)?,
                        check_type: row.try_get(3)?,
                        tool: row.try_get(4)?,
                        command: row.try_get(5)?,
                        working_directory: row.try_get(6)?,
                        config_path: row.try_get(7)?,
                        auto_fix: row.try_get(8)?,
                        fail_on_warning: row.try_get(9)?,
                        timeout_seconds: row.try_get(10)?,
                        is_critical: row.try_get(11)?,
                        enabled: row.try_get(12)?,
                        ai_generated: row.try_get(13)?,
                        ai_generation_prompt: row.try_get(14)?,
                        tags: row.try_get(15)?,
                        created_at: row.try_get(16)?,
                        updated_at: row.try_get(17)?,
                    })
                },
            mapper: |it| ListChecks::from(it),
        }
    }
}
pub struct GetCheckStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_check() -> GetCheckStmt {
    GetCheckStmt(
        "SELECT id, name, description, check_type, tool, command, working_directory, config_path, auto_fix, fail_on_warning, timeout_seconds, is_critical, enabled, ai_generated, ai_generation_prompt, tags, created_at, updated_at FROM checks WHERE id = $1",
        None,
    )
}
impl GetCheckStmt {
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
    ) -> GetCheckQuery<'c, 'a, 's, C, GetCheck, 1> {
        GetCheckQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<GetCheckBorrowed, tokio_postgres::Error> {
                    Ok(GetCheckBorrowed {
                        id: row.try_get(0)?,
                        name: row.try_get(1)?,
                        description: row.try_get(2)?,
                        check_type: row.try_get(3)?,
                        tool: row.try_get(4)?,
                        command: row.try_get(5)?,
                        working_directory: row.try_get(6)?,
                        config_path: row.try_get(7)?,
                        auto_fix: row.try_get(8)?,
                        fail_on_warning: row.try_get(9)?,
                        timeout_seconds: row.try_get(10)?,
                        is_critical: row.try_get(11)?,
                        enabled: row.try_get(12)?,
                        ai_generated: row.try_get(13)?,
                        ai_generation_prompt: row.try_get(14)?,
                        tags: row.try_get(15)?,
                        created_at: row.try_get(16)?,
                        updated_at: row.try_get(17)?,
                    })
                },
            mapper: |it| GetCheck::from(it),
        }
    }
}
pub struct CreateCheckStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_check() -> CreateCheckStmt {
    CreateCheckStmt(
        "INSERT INTO checks (id, name, description, check_type, tool, command, working_directory, config_path, auto_fix, fail_on_warning, timeout_seconds, is_critical, enabled, ai_generated, ai_generation_prompt, tags) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) RETURNING id",
        None,
    )
}
impl CreateCheckStmt {
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
        id: &'a T1,
        name: &'a T2,
        description: &'a Option<T3>,
        check_type: &'a T4,
        tool: &'a T5,
        command: &'a Option<T6>,
        working_directory: &'a Option<T7>,
        config_path: &'a Option<T8>,
        auto_fix: &'a bool,
        fail_on_warning: &'a bool,
        timeout_seconds: &'a Option<i32>,
        is_critical: &'a bool,
        enabled: &'a bool,
        ai_generated: &'a bool,
        ai_generation_prompt: &'a Option<T9>,
        tags: &'a T10,
    ) -> StringQuery<'c, 'a, 's, C, String, 16> {
        StringQuery {
            client,
            params: [
                id,
                name,
                description,
                check_type,
                tool,
                command,
                working_directory,
                config_path,
                auto_fix,
                fail_on_warning,
                timeout_seconds,
                is_critical,
                enabled,
                ai_generated,
                ai_generation_prompt,
                tags,
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
    T10: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        CreateCheckParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
        StringQuery<'c, 'a, 's, C, String, 16>,
        C,
    > for CreateCheckStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateCheckParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
    ) -> StringQuery<'c, 'a, 's, C, String, 16> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.description,
            &params.check_type,
            &params.tool,
            &params.command,
            &params.working_directory,
            &params.config_path,
            &params.auto_fix,
            &params.fail_on_warning,
            &params.timeout_seconds,
            &params.is_critical,
            &params.enabled,
            &params.ai_generated,
            &params.ai_generation_prompt,
            &params.tags,
        )
    }
}
pub struct UpdateCheckStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_check() -> UpdateCheckStmt {
    UpdateCheckStmt(
        "UPDATE checks SET name = COALESCE($1, name), description = COALESCE($2, description), check_type = COALESCE($3, check_type), tool = COALESCE($4, tool), command = COALESCE($5, command), working_directory = COALESCE($6, working_directory), config_path = COALESCE($7, config_path), auto_fix = COALESCE($8, auto_fix), fail_on_warning = COALESCE($9, fail_on_warning), timeout_seconds = COALESCE($10, timeout_seconds), is_critical = COALESCE($11, is_critical), enabled = COALESCE($12, enabled), ai_generation_prompt = COALESCE($13, ai_generation_prompt), tags = COALESCE($14, tags), updated_at = NOW() WHERE id = $15 RETURNING id",
        None,
    )
}
impl UpdateCheckStmt {
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
        name: &'a Option<T1>,
        description: &'a Option<T2>,
        check_type: &'a Option<T3>,
        tool: &'a Option<T4>,
        command: &'a Option<T5>,
        working_directory: &'a Option<T6>,
        config_path: &'a Option<T7>,
        auto_fix: &'a Option<bool>,
        fail_on_warning: &'a Option<bool>,
        timeout_seconds: &'a Option<i32>,
        is_critical: &'a Option<bool>,
        enabled: &'a Option<bool>,
        ai_generation_prompt: &'a Option<T8>,
        tags: &'a Option<T9>,
        id: &'a T10,
    ) -> StringQuery<'c, 'a, 's, C, String, 15> {
        StringQuery {
            client,
            params: [
                name,
                description,
                check_type,
                tool,
                command,
                working_directory,
                config_path,
                auto_fix,
                fail_on_warning,
                timeout_seconds,
                is_critical,
                enabled,
                ai_generation_prompt,
                tags,
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
    T10: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateCheckParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
        StringQuery<'c, 'a, 's, C, String, 15>,
        C,
    > for UpdateCheckStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateCheckParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10>,
    ) -> StringQuery<'c, 'a, 's, C, String, 15> {
        self.bind(
            client,
            &params.name,
            &params.description,
            &params.check_type,
            &params.tool,
            &params.command,
            &params.working_directory,
            &params.config_path,
            &params.auto_fix,
            &params.fail_on_warning,
            &params.timeout_seconds,
            &params.is_critical,
            &params.enabled,
            &params.ai_generation_prompt,
            &params.tags,
            &params.id,
        )
    }
}
pub struct DeleteCheckStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_check() -> DeleteCheckStmt {
    DeleteCheckStmt("DELETE FROM checks WHERE id = $1 RETURNING id", None)
}
impl DeleteCheckStmt {
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
pub struct GetCheckGroupStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_check_group() -> GetCheckGroupStmt {
    GetCheckGroupStmt(
        "SELECT id, name, description, color, enabled, run_in_parallel, stop_on_failure, tags, created_at, updated_at FROM check_groups WHERE id = $1",
        None,
    )
}
impl GetCheckGroupStmt {
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
    ) -> GetCheckGroupQuery<'c, 'a, 's, C, GetCheckGroup, 1> {
        GetCheckGroupQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<GetCheckGroupBorrowed, tokio_postgres::Error> {
                    Ok(GetCheckGroupBorrowed {
                        id: row.try_get(0)?,
                        name: row.try_get(1)?,
                        description: row.try_get(2)?,
                        color: row.try_get(3)?,
                        enabled: row.try_get(4)?,
                        run_in_parallel: row.try_get(5)?,
                        stop_on_failure: row.try_get(6)?,
                        tags: row.try_get(7)?,
                        created_at: row.try_get(8)?,
                        updated_at: row.try_get(9)?,
                    })
                },
            mapper: |it| GetCheckGroup::from(it),
        }
    }
}
pub struct CreateCheckGroupStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_check_group() -> CreateCheckGroupStmt {
    CreateCheckGroupStmt(
        "INSERT INTO check_groups (id, name, description, color, enabled, run_in_parallel, stop_on_failure, tags) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        None,
    )
}
impl CreateCheckGroupStmt {
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
        name: &'a T2,
        description: &'a Option<T3>,
        color: &'a Option<T4>,
        enabled: &'a bool,
        run_in_parallel: &'a bool,
        stop_on_failure: &'a bool,
        tags: &'a T5,
    ) -> StringQuery<'c, 'a, 's, C, String, 8> {
        StringQuery {
            client,
            params: [
                id,
                name,
                description,
                color,
                enabled,
                run_in_parallel,
                stop_on_failure,
                tags,
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
        CreateCheckGroupParams<T1, T2, T3, T4, T5>,
        StringQuery<'c, 'a, 's, C, String, 8>,
        C,
    > for CreateCheckGroupStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateCheckGroupParams<T1, T2, T3, T4, T5>,
    ) -> StringQuery<'c, 'a, 's, C, String, 8> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.description,
            &params.color,
            &params.enabled,
            &params.run_in_parallel,
            &params.stop_on_failure,
            &params.tags,
        )
    }
}
pub struct DeleteCheckGroupStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_check_group() -> DeleteCheckGroupStmt {
    DeleteCheckGroupStmt("DELETE FROM check_groups WHERE id = $1 RETURNING id", None)
}
impl DeleteCheckGroupStmt {
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
pub struct GetChecksInGroupStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_checks_in_group() -> GetChecksInGroupStmt {
    GetChecksInGroupStmt(
        "SELECT c.id, c.name, c.description, c.check_type, c.tool, c.command, c.working_directory, c.config_path, c.auto_fix, c.fail_on_warning, c.timeout_seconds, c.is_critical, c.enabled, c.ai_generated, c.ai_generation_prompt, c.tags, c.created_at, c.updated_at FROM checks c JOIN check_group_members cgm ON c.id = cgm.check_id WHERE cgm.group_id = $1 ORDER BY cgm.sort_order ASC",
        None,
    )
}
impl GetChecksInGroupStmt {
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
        group_id: &'a T1,
    ) -> GetChecksInGroupQuery<'c, 'a, 's, C, GetChecksInGroup, 1> {
        GetChecksInGroupQuery {
            client,
            params: [group_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetChecksInGroupBorrowed, tokio_postgres::Error> {
                Ok(GetChecksInGroupBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    check_type: row.try_get(3)?,
                    tool: row.try_get(4)?,
                    command: row.try_get(5)?,
                    working_directory: row.try_get(6)?,
                    config_path: row.try_get(7)?,
                    auto_fix: row.try_get(8)?,
                    fail_on_warning: row.try_get(9)?,
                    timeout_seconds: row.try_get(10)?,
                    is_critical: row.try_get(11)?,
                    enabled: row.try_get(12)?,
                    ai_generated: row.try_get(13)?,
                    ai_generation_prompt: row.try_get(14)?,
                    tags: row.try_get(15)?,
                    created_at: row.try_get(16)?,
                    updated_at: row.try_get(17)?,
                })
            },
            mapper: |it| GetChecksInGroup::from(it),
        }
    }
}
