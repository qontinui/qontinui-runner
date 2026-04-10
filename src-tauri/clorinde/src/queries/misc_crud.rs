// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreateShellCommandParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
> {
    pub id: T1,
    pub name: T2,
    pub description: Option<T3>,
    pub command: T4,
    pub working_directory: Option<T5>,
    pub timeout_seconds: Option<i32>,
    pub fail_on_error: bool,
    pub category: T6,
    pub tags: T7,
    pub enabled: bool,
}
#[derive(Debug)]
pub struct UpdateMcpServerParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
> {
    pub name: Option<T1>,
    pub description: Option<T2>,
    pub transport: Option<T3>,
    pub stdio_config: Option<T4>,
    pub http_config: Option<T5>,
    pub enabled: Option<bool>,
    pub auto_start: Option<bool>,
    pub timeout_seconds: Option<i32>,
    pub id: T6,
}
#[derive(Debug)]
pub struct UpdateMcpServerCachedToolsParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub cached_tools: Option<T1>,
    pub tools_cached_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub id: T2,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetShellCommand {
    pub id: String,
    pub name: String,
    pub description: String,
    pub command: String,
    pub working_directory: String,
    pub timeout_seconds: i32,
    pub fail_on_error: bool,
    pub category: String,
    pub tags: String,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetShellCommandBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub command: &'a str,
    pub working_directory: &'a str,
    pub timeout_seconds: i32,
    pub fail_on_error: bool,
    pub category: &'a str,
    pub tags: &'a str,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetShellCommandBorrowed<'a>> for GetShellCommand {
    fn from(
        GetShellCommandBorrowed {
            id,
            name,
            description,
            command,
            working_directory,
            timeout_seconds,
            fail_on_error,
            category,
            tags,
            enabled,
            created_at,
            updated_at,
        }: GetShellCommandBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            command: command.into(),
            working_directory: working_directory.into(),
            timeout_seconds,
            fail_on_error,
            category: category.into(),
            tags: tags.into(),
            enabled,
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListSavedApiRequests {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub method: String,
    pub url: String,
    pub headers: String,
    pub body: String,
    pub body_content_type: String,
    pub timeout_ms: i32,
    pub follow_redirects: bool,
    pub variable_extractions: String,
    pub assertions: String,
    pub credential_id: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListSavedApiRequestsBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a str,
    pub body: &'a str,
    pub body_content_type: &'a str,
    pub timeout_ms: i32,
    pub follow_redirects: bool,
    pub variable_extractions: &'a str,
    pub assertions: &'a str,
    pub credential_id: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListSavedApiRequestsBorrowed<'a>> for ListSavedApiRequests {
    fn from(
        ListSavedApiRequestsBorrowed {
            id,
            name,
            description,
            category,
            tags,
            method,
            url,
            headers,
            body,
            body_content_type,
            timeout_ms,
            follow_redirects,
            variable_extractions,
            assertions,
            credential_id,
            created_at,
            updated_at,
        }: ListSavedApiRequestsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            method: method.into(),
            url: url.into(),
            headers: headers.into(),
            body: body.into(),
            body_content_type: body_content_type.into(),
            timeout_ms,
            follow_redirects,
            variable_extractions: variable_extractions.into(),
            assertions: assertions.into(),
            credential_id: credential_id.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetSavedApiRequest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub method: String,
    pub url: String,
    pub headers: String,
    pub body: String,
    pub body_content_type: String,
    pub timeout_ms: i32,
    pub follow_redirects: bool,
    pub variable_extractions: String,
    pub assertions: String,
    pub credential_id: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetSavedApiRequestBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a str,
    pub body: &'a str,
    pub body_content_type: &'a str,
    pub timeout_ms: i32,
    pub follow_redirects: bool,
    pub variable_extractions: &'a str,
    pub assertions: &'a str,
    pub credential_id: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetSavedApiRequestBorrowed<'a>> for GetSavedApiRequest {
    fn from(
        GetSavedApiRequestBorrowed {
            id,
            name,
            description,
            category,
            tags,
            method,
            url,
            headers,
            body,
            body_content_type,
            timeout_ms,
            follow_redirects,
            variable_extractions,
            assertions,
            credential_id,
            created_at,
            updated_at,
        }: GetSavedApiRequestBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            method: method.into(),
            url: url.into(),
            headers: headers.into(),
            body: body.into(),
            body_content_type: body_content_type.into(),
            timeout_ms,
            follow_redirects,
            variable_extractions: variable_extractions.into(),
            assertions: assertions.into(),
            credential_id: credential_id.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListMcpServers {
    pub id: String,
    pub name: String,
    pub description: String,
    pub transport: String,
    pub stdio_config: String,
    pub http_config: String,
    pub enabled: bool,
    pub auto_start: bool,
    pub timeout_seconds: i32,
    pub cached_tools: String,
    pub tools_cached_at: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListMcpServersBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub transport: &'a str,
    pub stdio_config: &'a str,
    pub http_config: &'a str,
    pub enabled: bool,
    pub auto_start: bool,
    pub timeout_seconds: i32,
    pub cached_tools: &'a str,
    pub tools_cached_at: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListMcpServersBorrowed<'a>> for ListMcpServers {
    fn from(
        ListMcpServersBorrowed {
            id,
            name,
            description,
            transport,
            stdio_config,
            http_config,
            enabled,
            auto_start,
            timeout_seconds,
            cached_tools,
            tools_cached_at,
            created_at,
            updated_at,
        }: ListMcpServersBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            transport: transport.into(),
            stdio_config: stdio_config.into(),
            http_config: http_config.into(),
            enabled,
            auto_start,
            timeout_seconds,
            cached_tools: cached_tools.into(),
            tools_cached_at: tools_cached_at.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetMcpServer {
    pub id: String,
    pub name: String,
    pub description: String,
    pub transport: String,
    pub stdio_config: String,
    pub http_config: String,
    pub enabled: bool,
    pub auto_start: bool,
    pub timeout_seconds: i32,
    pub cached_tools: String,
    pub tools_cached_at: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetMcpServerBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub transport: &'a str,
    pub stdio_config: &'a str,
    pub http_config: &'a str,
    pub enabled: bool,
    pub auto_start: bool,
    pub timeout_seconds: i32,
    pub cached_tools: &'a str,
    pub tools_cached_at: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetMcpServerBorrowed<'a>> for GetMcpServer {
    fn from(
        GetMcpServerBorrowed {
            id,
            name,
            description,
            transport,
            stdio_config,
            http_config,
            enabled,
            auto_start,
            timeout_seconds,
            cached_tools,
            tools_cached_at,
            created_at,
            updated_at,
        }: GetMcpServerBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            transport: transport.into(),
            stdio_config: stdio_config.into(),
            http_config: http_config.into(),
            enabled,
            auto_start,
            timeout_seconds,
            cached_tools: cached_tools.into(),
            tools_cached_at: tools_cached_at.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListAutoStartMcpServers {
    pub id: String,
    pub name: String,
    pub description: String,
    pub transport: String,
    pub stdio_config: String,
    pub http_config: String,
    pub enabled: bool,
    pub auto_start: bool,
    pub timeout_seconds: i32,
    pub cached_tools: String,
    pub tools_cached_at: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListAutoStartMcpServersBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub transport: &'a str,
    pub stdio_config: &'a str,
    pub http_config: &'a str,
    pub enabled: bool,
    pub auto_start: bool,
    pub timeout_seconds: i32,
    pub cached_tools: &'a str,
    pub tools_cached_at: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListAutoStartMcpServersBorrowed<'a>> for ListAutoStartMcpServers {
    fn from(
        ListAutoStartMcpServersBorrowed {
            id,
            name,
            description,
            transport,
            stdio_config,
            http_config,
            enabled,
            auto_start,
            timeout_seconds,
            cached_tools,
            tools_cached_at,
            created_at,
            updated_at,
        }: ListAutoStartMcpServersBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            transport: transport.into(),
            stdio_config: stdio_config.into(),
            http_config: http_config.into(),
            enabled,
            auto_start,
            timeout_seconds,
            cached_tools: cached_tools.into(),
            tools_cached_at: tools_cached_at.into(),
            created_at,
            updated_at,
        }
    }
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct GetShellCommandQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetShellCommandBorrowed, tokio_postgres::Error>,
    mapper: fn(GetShellCommandBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetShellCommandQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetShellCommandBorrowed) -> R,
    ) -> GetShellCommandQuery<'c, 'a, 's, C, R, N> {
        GetShellCommandQuery {
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
pub struct ListSavedApiRequestsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ListSavedApiRequestsBorrowed, tokio_postgres::Error>,
    mapper: fn(ListSavedApiRequestsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListSavedApiRequestsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListSavedApiRequestsBorrowed) -> R,
    ) -> ListSavedApiRequestsQuery<'c, 'a, 's, C, R, N> {
        ListSavedApiRequestsQuery {
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
pub struct GetSavedApiRequestQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetSavedApiRequestBorrowed, tokio_postgres::Error>,
    mapper: fn(GetSavedApiRequestBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetSavedApiRequestQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetSavedApiRequestBorrowed) -> R,
    ) -> GetSavedApiRequestQuery<'c, 'a, 's, C, R, N> {
        GetSavedApiRequestQuery {
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
pub struct ListMcpServersQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<ListMcpServersBorrowed, tokio_postgres::Error>,
    mapper: fn(ListMcpServersBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListMcpServersQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListMcpServersBorrowed) -> R,
    ) -> ListMcpServersQuery<'c, 'a, 's, C, R, N> {
        ListMcpServersQuery {
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
pub struct GetMcpServerQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetMcpServerBorrowed, tokio_postgres::Error>,
    mapper: fn(GetMcpServerBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetMcpServerQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetMcpServerBorrowed) -> R,
    ) -> GetMcpServerQuery<'c, 'a, 's, C, R, N> {
        GetMcpServerQuery {
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
pub struct ListAutoStartMcpServersQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ListAutoStartMcpServersBorrowed, tokio_postgres::Error>,
    mapper: fn(ListAutoStartMcpServersBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListAutoStartMcpServersQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListAutoStartMcpServersBorrowed) -> R,
    ) -> ListAutoStartMcpServersQuery<'c, 'a, 's, C, R, N> {
        ListAutoStartMcpServersQuery {
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
pub struct GetShellCommandStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_shell_command() -> GetShellCommandStmt {
    GetShellCommandStmt(
        "SELECT id, name, COALESCE(description, '') as description, command, COALESCE(working_directory, '') as working_directory, COALESCE(timeout_seconds, 30) as timeout_seconds, fail_on_error, category, tags, enabled, created_at, updated_at FROM shell_commands WHERE id = $1",
        None,
    )
}
impl GetShellCommandStmt {
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
    ) -> GetShellCommandQuery<'c, 'a, 's, C, GetShellCommand, 1> {
        GetShellCommandQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetShellCommandBorrowed, tokio_postgres::Error> {
                Ok(GetShellCommandBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    command: row.try_get(3)?,
                    working_directory: row.try_get(4)?,
                    timeout_seconds: row.try_get(5)?,
                    fail_on_error: row.try_get(6)?,
                    category: row.try_get(7)?,
                    tags: row.try_get(8)?,
                    enabled: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    updated_at: row.try_get(11)?,
                })
            },
            mapper: |it| GetShellCommand::from(it),
        }
    }
}
pub struct CreateShellCommandStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_shell_command() -> CreateShellCommandStmt {
    CreateShellCommandStmt(
        "INSERT INTO shell_commands (id, name, description, command, working_directory, timeout_seconds, fail_on_error, category, tags, enabled) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        None,
    )
}
impl CreateShellCommandStmt {
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
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        name: &'a T2,
        description: &'a Option<T3>,
        command: &'a T4,
        working_directory: &'a Option<T5>,
        timeout_seconds: &'a Option<i32>,
        fail_on_error: &'a bool,
        category: &'a T6,
        tags: &'a T7,
        enabled: &'a bool,
    ) -> StringQuery<'c, 'a, 's, C, String, 10> {
        StringQuery {
            client,
            params: [
                id,
                name,
                description,
                command,
                working_directory,
                timeout_seconds,
                fail_on_error,
                category,
                tags,
                enabled,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        CreateShellCommandParams<T1, T2, T3, T4, T5, T6, T7>,
        StringQuery<'c, 'a, 's, C, String, 10>,
        C,
    > for CreateShellCommandStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateShellCommandParams<T1, T2, T3, T4, T5, T6, T7>,
    ) -> StringQuery<'c, 'a, 's, C, String, 10> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.description,
            &params.command,
            &params.working_directory,
            &params.timeout_seconds,
            &params.fail_on_error,
            &params.category,
            &params.tags,
            &params.enabled,
        )
    }
}
pub struct DeleteShellCommandStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_shell_command() -> DeleteShellCommandStmt {
    DeleteShellCommandStmt(
        "DELETE FROM shell_commands WHERE id = $1 RETURNING id",
        None,
    )
}
impl DeleteShellCommandStmt {
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
pub struct ListSavedApiRequestsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_saved_api_requests() -> ListSavedApiRequestsStmt {
    ListSavedApiRequestsStmt(
        "SELECT id, name, COALESCE(description, '') as description, COALESCE(category, '') as category, COALESCE(tags, '[]') as tags, method, url, COALESCE(headers, '{}') as headers, COALESCE(body, '') as body, COALESCE(body_content_type, '') as body_content_type, COALESCE(timeout_ms, 30000) as timeout_ms, COALESCE(follow_redirects, true) as follow_redirects, COALESCE(variable_extractions, '[]') as variable_extractions, COALESCE(assertions, '[]') as assertions, COALESCE(credential_id, '') as credential_id, created_at, updated_at FROM saved_api_requests ORDER BY updated_at DESC",
        None,
    )
}
impl ListSavedApiRequestsStmt {
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
    ) -> ListSavedApiRequestsQuery<'c, 'a, 's, C, ListSavedApiRequests, 0> {
        ListSavedApiRequestsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListSavedApiRequestsBorrowed, tokio_postgres::Error> {
                Ok(ListSavedApiRequestsBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    category: row.try_get(3)?,
                    tags: row.try_get(4)?,
                    method: row.try_get(5)?,
                    url: row.try_get(6)?,
                    headers: row.try_get(7)?,
                    body: row.try_get(8)?,
                    body_content_type: row.try_get(9)?,
                    timeout_ms: row.try_get(10)?,
                    follow_redirects: row.try_get(11)?,
                    variable_extractions: row.try_get(12)?,
                    assertions: row.try_get(13)?,
                    credential_id: row.try_get(14)?,
                    created_at: row.try_get(15)?,
                    updated_at: row.try_get(16)?,
                })
            },
            mapper: |it| ListSavedApiRequests::from(it),
        }
    }
}
pub struct GetSavedApiRequestStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_saved_api_request() -> GetSavedApiRequestStmt {
    GetSavedApiRequestStmt(
        "SELECT id, name, COALESCE(description, '') as description, COALESCE(category, '') as category, COALESCE(tags, '[]') as tags, method, url, COALESCE(headers, '{}') as headers, COALESCE(body, '') as body, COALESCE(body_content_type, '') as body_content_type, COALESCE(timeout_ms, 30000) as timeout_ms, COALESCE(follow_redirects, true) as follow_redirects, COALESCE(variable_extractions, '[]') as variable_extractions, COALESCE(assertions, '[]') as assertions, COALESCE(credential_id, '') as credential_id, created_at, updated_at FROM saved_api_requests WHERE id = $1",
        None,
    )
}
impl GetSavedApiRequestStmt {
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
    ) -> GetSavedApiRequestQuery<'c, 'a, 's, C, GetSavedApiRequest, 1> {
        GetSavedApiRequestQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetSavedApiRequestBorrowed, tokio_postgres::Error> {
                Ok(GetSavedApiRequestBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    category: row.try_get(3)?,
                    tags: row.try_get(4)?,
                    method: row.try_get(5)?,
                    url: row.try_get(6)?,
                    headers: row.try_get(7)?,
                    body: row.try_get(8)?,
                    body_content_type: row.try_get(9)?,
                    timeout_ms: row.try_get(10)?,
                    follow_redirects: row.try_get(11)?,
                    variable_extractions: row.try_get(12)?,
                    assertions: row.try_get(13)?,
                    credential_id: row.try_get(14)?,
                    created_at: row.try_get(15)?,
                    updated_at: row.try_get(16)?,
                })
            },
            mapper: |it| GetSavedApiRequest::from(it),
        }
    }
}
pub struct DeleteSavedApiRequestStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_saved_api_request() -> DeleteSavedApiRequestStmt {
    DeleteSavedApiRequestStmt(
        "DELETE FROM saved_api_requests WHERE id = $1 RETURNING id",
        None,
    )
}
impl DeleteSavedApiRequestStmt {
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
pub struct GetSavedApiRequestTagsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_saved_api_request_tags() -> GetSavedApiRequestTagsStmt {
    GetSavedApiRequestTagsStmt(
        "SELECT DISTINCT tags FROM saved_api_requests WHERE tags != '[]'",
        None,
    )
}
impl GetSavedApiRequestTagsStmt {
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
pub struct ListMcpServersStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_mcp_servers() -> ListMcpServersStmt {
    ListMcpServersStmt(
        "SELECT id, name, COALESCE(description, '') as description, transport, COALESCE(stdio_config, '{}') as stdio_config, COALESCE(http_config, '{}') as http_config, enabled, auto_start, COALESCE(timeout_seconds, 30) as timeout_seconds, COALESCE(cached_tools, '[]') as cached_tools, COALESCE(tools_cached_at::TEXT, '') as tools_cached_at, created_at, updated_at FROM mcp_servers ORDER BY name ASC",
        None,
    )
}
impl ListMcpServersStmt {
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
    ) -> ListMcpServersQuery<'c, 'a, 's, C, ListMcpServers, 0> {
        ListMcpServersQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListMcpServersBorrowed, tokio_postgres::Error> {
                Ok(ListMcpServersBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    transport: row.try_get(3)?,
                    stdio_config: row.try_get(4)?,
                    http_config: row.try_get(5)?,
                    enabled: row.try_get(6)?,
                    auto_start: row.try_get(7)?,
                    timeout_seconds: row.try_get(8)?,
                    cached_tools: row.try_get(9)?,
                    tools_cached_at: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                    updated_at: row.try_get(12)?,
                })
            },
            mapper: |it| ListMcpServers::from(it),
        }
    }
}
pub struct GetMcpServerStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_mcp_server() -> GetMcpServerStmt {
    GetMcpServerStmt(
        "SELECT id, name, COALESCE(description, '') as description, transport, COALESCE(stdio_config, '{}') as stdio_config, COALESCE(http_config, '{}') as http_config, enabled, auto_start, COALESCE(timeout_seconds, 30) as timeout_seconds, COALESCE(cached_tools, '[]') as cached_tools, COALESCE(tools_cached_at::TEXT, '') as tools_cached_at, created_at, updated_at FROM mcp_servers WHERE id = $1",
        None,
    )
}
impl GetMcpServerStmt {
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
    ) -> GetMcpServerQuery<'c, 'a, 's, C, GetMcpServer, 1> {
        GetMcpServerQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<GetMcpServerBorrowed, tokio_postgres::Error> {
                    Ok(GetMcpServerBorrowed {
                        id: row.try_get(0)?,
                        name: row.try_get(1)?,
                        description: row.try_get(2)?,
                        transport: row.try_get(3)?,
                        stdio_config: row.try_get(4)?,
                        http_config: row.try_get(5)?,
                        enabled: row.try_get(6)?,
                        auto_start: row.try_get(7)?,
                        timeout_seconds: row.try_get(8)?,
                        cached_tools: row.try_get(9)?,
                        tools_cached_at: row.try_get(10)?,
                        created_at: row.try_get(11)?,
                        updated_at: row.try_get(12)?,
                    })
                },
            mapper: |it| GetMcpServer::from(it),
        }
    }
}
pub struct UpdateMcpServerStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_mcp_server() -> UpdateMcpServerStmt {
    UpdateMcpServerStmt(
        "UPDATE mcp_servers SET name = COALESCE($1, name), description = COALESCE($2, description), transport = COALESCE($3, transport), stdio_config = COALESCE($4, stdio_config), http_config = COALESCE($5, http_config), enabled = COALESCE($6, enabled), auto_start = COALESCE($7, auto_start), timeout_seconds = COALESCE($8, timeout_seconds), updated_at = NOW() WHERE id = $9 RETURNING id",
        None,
    )
}
impl UpdateMcpServerStmt {
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
    >(
        &'s self,
        client: &'c C,
        name: &'a Option<T1>,
        description: &'a Option<T2>,
        transport: &'a Option<T3>,
        stdio_config: &'a Option<T4>,
        http_config: &'a Option<T5>,
        enabled: &'a Option<bool>,
        auto_start: &'a Option<bool>,
        timeout_seconds: &'a Option<i32>,
        id: &'a T6,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        StringQuery {
            client,
            params: [
                name,
                description,
                transport,
                stdio_config,
                http_config,
                enabled,
                auto_start,
                timeout_seconds,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdateMcpServerParams<T1, T2, T3, T4, T5, T6>,
        StringQuery<'c, 'a, 's, C, String, 9>,
        C,
    > for UpdateMcpServerStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateMcpServerParams<T1, T2, T3, T4, T5, T6>,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        self.bind(
            client,
            &params.name,
            &params.description,
            &params.transport,
            &params.stdio_config,
            &params.http_config,
            &params.enabled,
            &params.auto_start,
            &params.timeout_seconds,
            &params.id,
        )
    }
}
pub struct DeleteMcpServerStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_mcp_server() -> DeleteMcpServerStmt {
    DeleteMcpServerStmt("DELETE FROM mcp_servers WHERE id = $1 RETURNING id", None)
}
impl DeleteMcpServerStmt {
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
pub struct ListAutoStartMcpServersStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_auto_start_mcp_servers() -> ListAutoStartMcpServersStmt {
    ListAutoStartMcpServersStmt(
        "SELECT id, name, COALESCE(description, '') as description, transport, COALESCE(stdio_config, '{}') as stdio_config, COALESCE(http_config, '{}') as http_config, enabled, auto_start, COALESCE(timeout_seconds, 30) as timeout_seconds, COALESCE(cached_tools, '[]') as cached_tools, COALESCE(tools_cached_at::TEXT, '') as tools_cached_at, created_at, updated_at FROM mcp_servers WHERE auto_start = true AND enabled = true ORDER BY name ASC",
        None,
    )
}
impl ListAutoStartMcpServersStmt {
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
    ) -> ListAutoStartMcpServersQuery<'c, 'a, 's, C, ListAutoStartMcpServers, 0> {
        ListAutoStartMcpServersQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListAutoStartMcpServersBorrowed, tokio_postgres::Error> {
                Ok(ListAutoStartMcpServersBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    description: row.try_get(2)?,
                    transport: row.try_get(3)?,
                    stdio_config: row.try_get(4)?,
                    http_config: row.try_get(5)?,
                    enabled: row.try_get(6)?,
                    auto_start: row.try_get(7)?,
                    timeout_seconds: row.try_get(8)?,
                    cached_tools: row.try_get(9)?,
                    tools_cached_at: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                    updated_at: row.try_get(12)?,
                })
            },
            mapper: |it| ListAutoStartMcpServers::from(it),
        }
    }
}
pub struct UpdateMcpServerCachedToolsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_mcp_server_cached_tools() -> UpdateMcpServerCachedToolsStmt {
    UpdateMcpServerCachedToolsStmt(
        "UPDATE mcp_servers SET cached_tools = $1, tools_cached_at = $2::TIMESTAMPTZ, updated_at = NOW() WHERE id = $3 RETURNING id",
        None,
    )
}
impl UpdateMcpServerCachedToolsStmt {
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
        cached_tools: &'a Option<T1>,
        tools_cached_at: &'a Option<chrono::DateTime<chrono::FixedOffset>>,
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 3> {
        StringQuery {
            client,
            params: [cached_tools, tools_cached_at, id],
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
        UpdateMcpServerCachedToolsParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 3>,
        C,
    > for UpdateMcpServerCachedToolsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateMcpServerCachedToolsParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 3> {
        self.bind(
            client,
            &params.cached_tools,
            &params.tools_cached_at,
            &params.id,
        )
    }
}
