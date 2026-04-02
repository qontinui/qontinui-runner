// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct GetPromptByVersionParams<T1: crate::StringSql> {
    pub agent_type: T1,
    pub version: i32,
}
#[derive(Debug)]
pub struct CreateVariantParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
> {
    pub id: T1,
    pub agent_type: T2,
    pub variant_name: T3,
    pub prompt_content: T4,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: Option<T5>,
    pub performance_metrics: Option<T6>,
}
#[derive(Debug)]
pub struct UpdatePerformanceMetricsParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub performance_metrics: T1,
    pub id: T2,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetActivePrompt {
    pub id: String,
    pub agent_type: String,
    pub variant_name: String,
    pub prompt_content: String,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: String,
    pub performance_metrics: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetActivePromptBorrowed<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub variant_name: &'a str,
    pub prompt_content: &'a str,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: &'a str,
    pub performance_metrics: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetActivePromptBorrowed<'a>> for GetActivePrompt {
    fn from(
        GetActivePromptBorrowed {
            id,
            agent_type,
            variant_name,
            prompt_content,
            version,
            is_active,
            source_recommendation_id,
            performance_metrics,
            created_at,
            updated_at,
        }: GetActivePromptBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            agent_type: agent_type.into(),
            variant_name: variant_name.into(),
            prompt_content: prompt_content.into(),
            version,
            is_active,
            source_recommendation_id: source_recommendation_id.into(),
            performance_metrics: performance_metrics.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetPromptByVersion {
    pub id: String,
    pub agent_type: String,
    pub variant_name: String,
    pub prompt_content: String,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: String,
    pub performance_metrics: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetPromptByVersionBorrowed<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub variant_name: &'a str,
    pub prompt_content: &'a str,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: &'a str,
    pub performance_metrics: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetPromptByVersionBorrowed<'a>> for GetPromptByVersion {
    fn from(
        GetPromptByVersionBorrowed {
            id,
            agent_type,
            variant_name,
            prompt_content,
            version,
            is_active,
            source_recommendation_id,
            performance_metrics,
            created_at,
            updated_at,
        }: GetPromptByVersionBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            agent_type: agent_type.into(),
            variant_name: variant_name.into(),
            prompt_content: prompt_content.into(),
            version,
            is_active,
            source_recommendation_id: source_recommendation_id.into(),
            performance_metrics: performance_metrics.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CreateVariant {
    pub id: String,
    pub agent_type: String,
    pub variant_name: String,
    pub version: i32,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct CreateVariantBorrowed<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub variant_name: &'a str,
    pub version: i32,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<CreateVariantBorrowed<'a>> for CreateVariant {
    fn from(
        CreateVariantBorrowed {
            id,
            agent_type,
            variant_name,
            version,
            is_active,
            created_at,
        }: CreateVariantBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            agent_type: agent_type.into(),
            variant_name: variant_name.into(),
            version,
            is_active,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActivateVariant {
    pub id: String,
    pub agent_type: String,
    pub variant_name: String,
    pub version: i32,
    pub is_active: bool,
}
pub struct ActivateVariantBorrowed<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub variant_name: &'a str,
    pub version: i32,
    pub is_active: bool,
}
impl<'a> From<ActivateVariantBorrowed<'a>> for ActivateVariant {
    fn from(
        ActivateVariantBorrowed {
            id,
            agent_type,
            variant_name,
            version,
            is_active,
        }: ActivateVariantBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            agent_type: agent_type.into(),
            variant_name: variant_name.into(),
            version,
            is_active,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListVariants {
    pub id: String,
    pub agent_type: String,
    pub variant_name: String,
    pub prompt_content: String,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: String,
    pub performance_metrics: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListVariantsBorrowed<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub variant_name: &'a str,
    pub prompt_content: &'a str,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: &'a str,
    pub performance_metrics: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListVariantsBorrowed<'a>> for ListVariants {
    fn from(
        ListVariantsBorrowed {
            id,
            agent_type,
            variant_name,
            prompt_content,
            version,
            is_active,
            source_recommendation_id,
            performance_metrics,
            created_at,
            updated_at,
        }: ListVariantsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            agent_type: agent_type.into(),
            variant_name: variant_name.into(),
            prompt_content: prompt_content.into(),
            version,
            is_active,
            source_recommendation_id: source_recommendation_id.into(),
            performance_metrics: performance_metrics.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListVariantsByAgent {
    pub id: String,
    pub agent_type: String,
    pub variant_name: String,
    pub prompt_content: String,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: String,
    pub performance_metrics: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListVariantsByAgentBorrowed<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub variant_name: &'a str,
    pub prompt_content: &'a str,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: &'a str,
    pub performance_metrics: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListVariantsByAgentBorrowed<'a>> for ListVariantsByAgent {
    fn from(
        ListVariantsByAgentBorrowed {
            id,
            agent_type,
            variant_name,
            prompt_content,
            version,
            is_active,
            source_recommendation_id,
            performance_metrics,
            created_at,
            updated_at,
        }: ListVariantsByAgentBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            agent_type: agent_type.into(),
            variant_name: variant_name.into(),
            prompt_content: prompt_content.into(),
            version,
            is_active,
            source_recommendation_id: source_recommendation_id.into(),
            performance_metrics: performance_metrics.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UpdatePerformanceMetrics {
    pub id: String,
    pub agent_type: String,
    pub variant_name: String,
    pub version: i32,
    pub performance_metrics: String,
}
pub struct UpdatePerformanceMetricsBorrowed<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    pub variant_name: &'a str,
    pub version: i32,
    pub performance_metrics: &'a str,
}
impl<'a> From<UpdatePerformanceMetricsBorrowed<'a>> for UpdatePerformanceMetrics {
    fn from(
        UpdatePerformanceMetricsBorrowed {
            id,
            agent_type,
            variant_name,
            version,
            performance_metrics,
        }: UpdatePerformanceMetricsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            agent_type: agent_type.into(),
            variant_name: variant_name.into(),
            version,
            performance_metrics: performance_metrics.into(),
        }
    }
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct GetActivePromptQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetActivePromptBorrowed, tokio_postgres::Error>,
    mapper: fn(GetActivePromptBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetActivePromptQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetActivePromptBorrowed) -> R,
    ) -> GetActivePromptQuery<'c, 'a, 's, C, R, N> {
        GetActivePromptQuery {
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
pub struct GetPromptByVersionQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetPromptByVersionBorrowed, tokio_postgres::Error>,
    mapper: fn(GetPromptByVersionBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetPromptByVersionQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetPromptByVersionBorrowed) -> R,
    ) -> GetPromptByVersionQuery<'c, 'a, 's, C, R, N> {
        GetPromptByVersionQuery {
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
pub struct CreateVariantQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<CreateVariantBorrowed, tokio_postgres::Error>,
    mapper: fn(CreateVariantBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> CreateVariantQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(CreateVariantBorrowed) -> R,
    ) -> CreateVariantQuery<'c, 'a, 's, C, R, N> {
        CreateVariantQuery {
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
pub struct ActivateVariantQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<ActivateVariantBorrowed, tokio_postgres::Error>,
    mapper: fn(ActivateVariantBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ActivateVariantQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ActivateVariantBorrowed) -> R,
    ) -> ActivateVariantQuery<'c, 'a, 's, C, R, N> {
        ActivateVariantQuery {
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
pub struct ListVariantsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<ListVariantsBorrowed, tokio_postgres::Error>,
    mapper: fn(ListVariantsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListVariantsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListVariantsBorrowed) -> R,
    ) -> ListVariantsQuery<'c, 'a, 's, C, R, N> {
        ListVariantsQuery {
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
pub struct ListVariantsByAgentQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<ListVariantsByAgentBorrowed, tokio_postgres::Error>,
    mapper: fn(ListVariantsByAgentBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListVariantsByAgentQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListVariantsByAgentBorrowed) -> R,
    ) -> ListVariantsByAgentQuery<'c, 'a, 's, C, R, N> {
        ListVariantsByAgentQuery {
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
pub struct UpdatePerformanceMetricsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<UpdatePerformanceMetricsBorrowed, tokio_postgres::Error>,
    mapper: fn(UpdatePerformanceMetricsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> UpdatePerformanceMetricsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(UpdatePerformanceMetricsBorrowed) -> R,
    ) -> UpdatePerformanceMetricsQuery<'c, 'a, 's, C, R, N> {
        UpdatePerformanceMetricsQuery {
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
pub struct GetActivePromptStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_active_prompt() -> GetActivePromptStmt {
    GetActivePromptStmt(
        "SELECT id, agent_type, variant_name, prompt_content, version, is_active, COALESCE(source_recommendation_id, '') as source_recommendation_id, COALESCE(performance_metrics, '{}') as performance_metrics, created_at, updated_at FROM prompt_registry WHERE agent_type = $1 AND is_active = true LIMIT 1",
        None,
    )
}
impl GetActivePromptStmt {
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
        agent_type: &'a T1,
    ) -> GetActivePromptQuery<'c, 'a, 's, C, GetActivePrompt, 1> {
        GetActivePromptQuery {
            client,
            params: [agent_type],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetActivePromptBorrowed, tokio_postgres::Error> {
                Ok(GetActivePromptBorrowed {
                    id: row.try_get(0)?,
                    agent_type: row.try_get(1)?,
                    variant_name: row.try_get(2)?,
                    prompt_content: row.try_get(3)?,
                    version: row.try_get(4)?,
                    is_active: row.try_get(5)?,
                    source_recommendation_id: row.try_get(6)?,
                    performance_metrics: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                    updated_at: row.try_get(9)?,
                })
            },
            mapper: |it| GetActivePrompt::from(it),
        }
    }
}
pub struct GetPromptByVersionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_prompt_by_version() -> GetPromptByVersionStmt {
    GetPromptByVersionStmt(
        "SELECT id, agent_type, variant_name, prompt_content, version, is_active, COALESCE(source_recommendation_id, '') as source_recommendation_id, COALESCE(performance_metrics, '{}') as performance_metrics, created_at, updated_at FROM prompt_registry WHERE agent_type = $1 AND version = $2 LIMIT 1",
        None,
    )
}
impl GetPromptByVersionStmt {
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
        agent_type: &'a T1,
        version: &'a i32,
    ) -> GetPromptByVersionQuery<'c, 'a, 's, C, GetPromptByVersion, 2> {
        GetPromptByVersionQuery {
            client,
            params: [agent_type, version],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetPromptByVersionBorrowed, tokio_postgres::Error> {
                Ok(GetPromptByVersionBorrowed {
                    id: row.try_get(0)?,
                    agent_type: row.try_get(1)?,
                    variant_name: row.try_get(2)?,
                    prompt_content: row.try_get(3)?,
                    version: row.try_get(4)?,
                    is_active: row.try_get(5)?,
                    source_recommendation_id: row.try_get(6)?,
                    performance_metrics: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                    updated_at: row.try_get(9)?,
                })
            },
            mapper: |it| GetPromptByVersion::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetPromptByVersionParams<T1>,
        GetPromptByVersionQuery<'c, 'a, 's, C, GetPromptByVersion, 2>,
        C,
    > for GetPromptByVersionStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetPromptByVersionParams<T1>,
    ) -> GetPromptByVersionQuery<'c, 'a, 's, C, GetPromptByVersion, 2> {
        self.bind(client, &params.agent_type, &params.version)
    }
}
pub struct CreateVariantStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_variant() -> CreateVariantStmt {
    CreateVariantStmt(
        "INSERT INTO prompt_registry ( id, agent_type, variant_name, prompt_content, version, is_active, source_recommendation_id, performance_metrics ) VALUES ( $1, $2, $3, $4, $5, $6, $7, $8 ) RETURNING id, agent_type, variant_name, version, is_active, created_at",
        None,
    )
}
impl CreateVariantStmt {
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
        id: &'a T1,
        agent_type: &'a T2,
        variant_name: &'a T3,
        prompt_content: &'a T4,
        version: &'a i32,
        is_active: &'a bool,
        source_recommendation_id: &'a Option<T5>,
        performance_metrics: &'a Option<T6>,
    ) -> CreateVariantQuery<'c, 'a, 's, C, CreateVariant, 8> {
        CreateVariantQuery {
            client,
            params: [
                id,
                agent_type,
                variant_name,
                prompt_content,
                version,
                is_active,
                source_recommendation_id,
                performance_metrics,
            ],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<CreateVariantBorrowed, tokio_postgres::Error> {
                    Ok(CreateVariantBorrowed {
                        id: row.try_get(0)?,
                        agent_type: row.try_get(1)?,
                        variant_name: row.try_get(2)?,
                        version: row.try_get(3)?,
                        is_active: row.try_get(4)?,
                        created_at: row.try_get(5)?,
                    })
                },
            mapper: |it| CreateVariant::from(it),
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
        CreateVariantParams<T1, T2, T3, T4, T5, T6>,
        CreateVariantQuery<'c, 'a, 's, C, CreateVariant, 8>,
        C,
    > for CreateVariantStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateVariantParams<T1, T2, T3, T4, T5, T6>,
    ) -> CreateVariantQuery<'c, 'a, 's, C, CreateVariant, 8> {
        self.bind(
            client,
            &params.id,
            &params.agent_type,
            &params.variant_name,
            &params.prompt_content,
            &params.version,
            &params.is_active,
            &params.source_recommendation_id,
            &params.performance_metrics,
        )
    }
}
pub struct ActivateVariantStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn activate_variant() -> ActivateVariantStmt {
    ActivateVariantStmt(
        "UPDATE prompt_registry SET is_active = true, updated_at = NOW() WHERE id = $1 RETURNING id, agent_type, variant_name, version, is_active",
        None,
    )
}
impl ActivateVariantStmt {
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
    ) -> ActivateVariantQuery<'c, 'a, 's, C, ActivateVariant, 1> {
        ActivateVariantQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ActivateVariantBorrowed, tokio_postgres::Error> {
                Ok(ActivateVariantBorrowed {
                    id: row.try_get(0)?,
                    agent_type: row.try_get(1)?,
                    variant_name: row.try_get(2)?,
                    version: row.try_get(3)?,
                    is_active: row.try_get(4)?,
                })
            },
            mapper: |it| ActivateVariant::from(it),
        }
    }
}
pub struct DeactivateVariantsForAgentStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn deactivate_variants_for_agent() -> DeactivateVariantsForAgentStmt {
    DeactivateVariantsForAgentStmt(
        "UPDATE prompt_registry SET is_active = false, updated_at = NOW() WHERE agent_type = $1 RETURNING id",
        None,
    )
}
impl DeactivateVariantsForAgentStmt {
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
        agent_type: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [agent_type],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct ListVariantsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_variants() -> ListVariantsStmt {
    ListVariantsStmt(
        "SELECT id, agent_type, variant_name, prompt_content, version, is_active, COALESCE(source_recommendation_id, '') as source_recommendation_id, COALESCE(performance_metrics, '{}') as performance_metrics, created_at, updated_at FROM prompt_registry ORDER BY agent_type, version DESC",
        None,
    )
}
impl ListVariantsStmt {
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
    ) -> ListVariantsQuery<'c, 'a, 's, C, ListVariants, 0> {
        ListVariantsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<ListVariantsBorrowed, tokio_postgres::Error> {
                    Ok(ListVariantsBorrowed {
                        id: row.try_get(0)?,
                        agent_type: row.try_get(1)?,
                        variant_name: row.try_get(2)?,
                        prompt_content: row.try_get(3)?,
                        version: row.try_get(4)?,
                        is_active: row.try_get(5)?,
                        source_recommendation_id: row.try_get(6)?,
                        performance_metrics: row.try_get(7)?,
                        created_at: row.try_get(8)?,
                        updated_at: row.try_get(9)?,
                    })
                },
            mapper: |it| ListVariants::from(it),
        }
    }
}
pub struct ListVariantsByAgentStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_variants_by_agent() -> ListVariantsByAgentStmt {
    ListVariantsByAgentStmt(
        "SELECT id, agent_type, variant_name, prompt_content, version, is_active, COALESCE(source_recommendation_id, '') as source_recommendation_id, COALESCE(performance_metrics, '{}') as performance_metrics, created_at, updated_at FROM prompt_registry WHERE agent_type = $1 ORDER BY version DESC",
        None,
    )
}
impl ListVariantsByAgentStmt {
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
        agent_type: &'a T1,
    ) -> ListVariantsByAgentQuery<'c, 'a, 's, C, ListVariantsByAgent, 1> {
        ListVariantsByAgentQuery {
            client,
            params: [agent_type],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListVariantsByAgentBorrowed, tokio_postgres::Error> {
                Ok(ListVariantsByAgentBorrowed {
                    id: row.try_get(0)?,
                    agent_type: row.try_get(1)?,
                    variant_name: row.try_get(2)?,
                    prompt_content: row.try_get(3)?,
                    version: row.try_get(4)?,
                    is_active: row.try_get(5)?,
                    source_recommendation_id: row.try_get(6)?,
                    performance_metrics: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                    updated_at: row.try_get(9)?,
                })
            },
            mapper: |it| ListVariantsByAgent::from(it),
        }
    }
}
pub struct UpdatePerformanceMetricsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_performance_metrics() -> UpdatePerformanceMetricsStmt {
    UpdatePerformanceMetricsStmt(
        "UPDATE prompt_registry SET performance_metrics = $1, updated_at = NOW() WHERE id = $2 RETURNING id, agent_type, variant_name, version, performance_metrics",
        None,
    )
}
impl UpdatePerformanceMetricsStmt {
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
        performance_metrics: &'a T1,
        id: &'a T2,
    ) -> UpdatePerformanceMetricsQuery<'c, 'a, 's, C, UpdatePerformanceMetrics, 2> {
        UpdatePerformanceMetricsQuery {
            client,
            params: [performance_metrics, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<UpdatePerformanceMetricsBorrowed, tokio_postgres::Error> {
                Ok(UpdatePerformanceMetricsBorrowed {
                    id: row.try_get(0)?,
                    agent_type: row.try_get(1)?,
                    variant_name: row.try_get(2)?,
                    version: row.try_get(3)?,
                    performance_metrics: row.try_get(4)?,
                })
            },
            mapper: |it| UpdatePerformanceMetrics::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        UpdatePerformanceMetricsParams<T1, T2>,
        UpdatePerformanceMetricsQuery<'c, 'a, 's, C, UpdatePerformanceMetrics, 2>,
        C,
    > for UpdatePerformanceMetricsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdatePerformanceMetricsParams<T1, T2>,
    ) -> UpdatePerformanceMetricsQuery<'c, 'a, 's, C, UpdatePerformanceMetrics, 2> {
        self.bind(client, &params.performance_metrics, &params.id)
    }
}
pub struct GetNextVersionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_next_version() -> GetNextVersionStmt {
    GetNextVersionStmt(
        "SELECT COALESCE(MAX(version), 0) + 1 as next_version FROM prompt_registry WHERE agent_type = $1",
        None,
    )
}
impl GetNextVersionStmt {
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
        agent_type: &'a T1,
    ) -> I32Query<'c, 'a, 's, C, i32, 1> {
        I32Query {
            client,
            params: [agent_type],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
