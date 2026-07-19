// This file was generated with `clorinde`. Do not modify.

#[derive(Clone, Copy, Debug)]
pub struct GetTaskRunCostsParams {
    pub days: i32,
    pub max_results: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetDailyCost {
    pub day: String,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
}
pub struct GetDailyCostBorrowed<'a> {
    pub day: &'a str,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
}
impl<'a> From<GetDailyCostBorrowed<'a>> for GetDailyCost {
    fn from(
        GetDailyCostBorrowed {
            day,
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
        }: GetDailyCostBorrowed<'a>,
    ) -> Self {
        Self {
            day: day.into(),
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetCostByModel {
    pub model_used: String,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
    pub avg_duration_ms: i64,
}
pub struct GetCostByModelBorrowed<'a> {
    pub model_used: &'a str,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
    pub avg_duration_ms: i64,
}
impl<'a> From<GetCostByModelBorrowed<'a>> for GetCostByModel {
    fn from(
        GetCostByModelBorrowed {
            model_used,
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
            avg_duration_ms,
        }: GetCostByModelBorrowed<'a>,
    ) -> Self {
        Self {
            model_used: model_used.into(),
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
            avg_duration_ms,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetCostByPhase {
    pub phase: String,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}
pub struct GetCostByPhaseBorrowed<'a> {
    pub phase: &'a str,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}
impl<'a> From<GetCostByPhaseBorrowed<'a>> for GetCostByPhase {
    fn from(
        GetCostByPhaseBorrowed {
            phase,
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
        }: GetCostByPhaseBorrowed<'a>,
    ) -> Self {
        Self {
            phase: phase.into(),
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetProviderLatency {
    pub provider_used: String,
    pub avg_duration_ms: i64,
    pub min_duration_ms: i64,
    pub max_duration_ms: i64,
    pub call_count: i64,
}
pub struct GetProviderLatencyBorrowed<'a> {
    pub provider_used: &'a str,
    pub avg_duration_ms: i64,
    pub min_duration_ms: i64,
    pub max_duration_ms: i64,
    pub call_count: i64,
}
impl<'a> From<GetProviderLatencyBorrowed<'a>> for GetProviderLatency {
    fn from(
        GetProviderLatencyBorrowed {
            provider_used,
            avg_duration_ms,
            min_duration_ms,
            max_duration_ms,
            call_count,
        }: GetProviderLatencyBorrowed<'a>,
    ) -> Self {
        Self {
            provider_used: provider_used.into(),
            avg_duration_ms,
            min_duration_ms,
            max_duration_ms,
            call_count,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskRunCosts {
    pub task_run_id: String,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
    pub started_at: String,
}
pub struct GetTaskRunCostsBorrowed<'a> {
    pub task_run_id: &'a str,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
    pub started_at: &'a str,
}
impl<'a> From<GetTaskRunCostsBorrowed<'a>> for GetTaskRunCosts {
    fn from(
        GetTaskRunCostsBorrowed {
            task_run_id,
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
            started_at,
        }: GetTaskRunCostsBorrowed<'a>,
    ) -> Self {
        Self {
            task_run_id: task_run_id.into(),
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
            started_at: started_at.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct GetTokenUsageTotals {
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_calls: i64,
    pub avg_duration_ms: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetCostByTargetApp {
    pub target_app: String,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
    pub avg_duration_ms: i64,
}
pub struct GetCostByTargetAppBorrowed<'a> {
    pub target_app: &'a str,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
    pub avg_duration_ms: i64,
}
impl<'a> From<GetCostByTargetAppBorrowed<'a>> for GetCostByTargetApp {
    fn from(
        GetCostByTargetAppBorrowed {
            target_app,
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
            avg_duration_ms,
        }: GetCostByTargetAppBorrowed<'a>,
    ) -> Self {
        Self {
            target_app: target_app.into(),
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
            avg_duration_ms,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetCostByTargetPage {
    pub target_page_url: String,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
}
pub struct GetCostByTargetPageBorrowed<'a> {
    pub target_page_url: &'a str,
    pub total_cost_cents: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub call_count: i64,
}
impl<'a> From<GetCostByTargetPageBorrowed<'a>> for GetCostByTargetPage {
    fn from(
        GetCostByTargetPageBorrowed {
            target_page_url,
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
        }: GetCostByTargetPageBorrowed<'a>,
    ) -> Self {
        Self {
            target_page_url: target_page_url.into(),
            total_cost_cents,
            total_input_tokens,
            total_output_tokens,
            call_count,
        }
    }
}
use futures::{self, StreamExt, TryStreamExt};
use crate::client::async_::GenericClient;
pub struct GetDailyCostQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetDailyCostBorrowed, tokio_postgres::Error>,
    mapper: fn(GetDailyCostBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetDailyCostQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetDailyCostBorrowed) -> R,
    ) -> GetDailyCostQuery<'c, 'a, 's, C, R, N> {
        GetDailyCostQuery {
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
pub struct GetCostByModelQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetCostByModelBorrowed, tokio_postgres::Error>,
    mapper: fn(GetCostByModelBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCostByModelQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetCostByModelBorrowed) -> R,
    ) -> GetCostByModelQuery<'c, 'a, 's, C, R, N> {
        GetCostByModelQuery {
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
pub struct GetCostByPhaseQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetCostByPhaseBorrowed, tokio_postgres::Error>,
    mapper: fn(GetCostByPhaseBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCostByPhaseQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetCostByPhaseBorrowed) -> R,
    ) -> GetCostByPhaseQuery<'c, 'a, 's, C, R, N> {
        GetCostByPhaseQuery {
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
pub struct GetProviderLatencyQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetProviderLatencyBorrowed, tokio_postgres::Error>,
    mapper: fn(GetProviderLatencyBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetProviderLatencyQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetProviderLatencyBorrowed) -> R,
    ) -> GetProviderLatencyQuery<'c, 'a, 's, C, R, N> {
        GetProviderLatencyQuery {
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
pub struct GetTaskRunCostsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetTaskRunCostsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskRunCostsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskRunCostsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskRunCostsBorrowed) -> R,
    ) -> GetTaskRunCostsQuery<'c, 'a, 's, C, R, N> {
        GetTaskRunCostsQuery {
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
pub struct GetTokenUsageTotalsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetTokenUsageTotals, tokio_postgres::Error>,
    mapper: fn(GetTokenUsageTotals) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTokenUsageTotalsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTokenUsageTotals) -> R,
    ) -> GetTokenUsageTotalsQuery<'c, 'a, 's, C, R, N> {
        GetTokenUsageTotalsQuery {
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
pub struct GetCostByTargetAppQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetCostByTargetAppBorrowed, tokio_postgres::Error>,
    mapper: fn(GetCostByTargetAppBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCostByTargetAppQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetCostByTargetAppBorrowed) -> R,
    ) -> GetCostByTargetAppQuery<'c, 'a, 's, C, R, N> {
        GetCostByTargetAppQuery {
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
pub struct GetCostByTargetPageQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetCostByTargetPageBorrowed, tokio_postgres::Error>,
    mapper: fn(GetCostByTargetPageBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetCostByTargetPageQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetCostByTargetPageBorrowed) -> R,
    ) -> GetCostByTargetPageQuery<'c, 'a, 's, C, R, N> {
        GetCostByTargetPageQuery {
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
pub struct GetDailyCostStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_daily_cost() -> GetDailyCostStmt {
    GetDailyCostStmt(
        "SELECT date(created_at)::text as day, COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents, COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens, COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens, COUNT(*)::bigint as call_count FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) GROUP BY date(created_at) ORDER BY day ASC",
        None,
    )
}
impl GetDailyCostStmt {
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
        days: &'a i32,
    ) -> GetDailyCostQuery<'c, 'a, 's, C, GetDailyCost, 1> {
        GetDailyCostQuery {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetDailyCostBorrowed, tokio_postgres::Error> {
                Ok(GetDailyCostBorrowed {
                    day: row.try_get(0)?,
                    total_cost_cents: row.try_get(1)?,
                    total_input_tokens: row.try_get(2)?,
                    total_output_tokens: row.try_get(3)?,
                    call_count: row.try_get(4)?,
                })
            },
            mapper: |it| GetDailyCost::from(it),
        }
    }
}
pub struct GetCostByModelStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_cost_by_model() -> GetCostByModelStmt {
    GetCostByModelStmt(
        "SELECT model_used, COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents, COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens, COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens, COUNT(*)::bigint as call_count, COALESCE(AVG(duration_ms), 0)::bigint as avg_duration_ms FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) AND model_used IS NOT NULL GROUP BY model_used ORDER BY SUM(cost_cents) DESC",
        None,
    )
}
impl GetCostByModelStmt {
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
        days: &'a i32,
    ) -> GetCostByModelQuery<'c, 'a, 's, C, GetCostByModel, 1> {
        GetCostByModelQuery {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetCostByModelBorrowed, tokio_postgres::Error> {
                Ok(GetCostByModelBorrowed {
                    model_used: row.try_get(0)?,
                    total_cost_cents: row.try_get(1)?,
                    total_input_tokens: row.try_get(2)?,
                    total_output_tokens: row.try_get(3)?,
                    call_count: row.try_get(4)?,
                    avg_duration_ms: row.try_get(5)?,
                })
            },
            mapper: |it| GetCostByModel::from(it),
        }
    }
}
pub struct GetCostByPhaseStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_cost_by_phase() -> GetCostByPhaseStmt {
    GetCostByPhaseStmt(
        "SELECT phase, COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents, COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens, COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) GROUP BY phase ORDER BY SUM(cost_cents) DESC",
        None,
    )
}
impl GetCostByPhaseStmt {
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
        days: &'a i32,
    ) -> GetCostByPhaseQuery<'c, 'a, 's, C, GetCostByPhase, 1> {
        GetCostByPhaseQuery {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetCostByPhaseBorrowed, tokio_postgres::Error> {
                Ok(GetCostByPhaseBorrowed {
                    phase: row.try_get(0)?,
                    total_cost_cents: row.try_get(1)?,
                    total_input_tokens: row.try_get(2)?,
                    total_output_tokens: row.try_get(3)?,
                })
            },
            mapper: |it| GetCostByPhase::from(it),
        }
    }
}
pub struct GetProviderLatencyStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_provider_latency() -> GetProviderLatencyStmt {
    GetProviderLatencyStmt(
        "SELECT provider_used, COALESCE(AVG(duration_ms), 0)::bigint as avg_duration_ms, MIN(duration_ms) as min_duration_ms, MAX(duration_ms) as max_duration_ms, COUNT(*)::bigint as call_count FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) AND provider_used IS NOT NULL AND duration_ms IS NOT NULL GROUP BY provider_used",
        None,
    )
}
impl GetProviderLatencyStmt {
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
        days: &'a i32,
    ) -> GetProviderLatencyQuery<'c, 'a, 's, C, GetProviderLatency, 1> {
        GetProviderLatencyQuery {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetProviderLatencyBorrowed, tokio_postgres::Error> {
                Ok(GetProviderLatencyBorrowed {
                    provider_used: row.try_get(0)?,
                    avg_duration_ms: row.try_get(1)?,
                    min_duration_ms: row.try_get(2)?,
                    max_duration_ms: row.try_get(3)?,
                    call_count: row.try_get(4)?,
                })
            },
            mapper: |it| GetProviderLatency::from(it),
        }
    }
}
pub struct GetTaskRunCostsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_run_costs() -> GetTaskRunCostsStmt {
    GetTaskRunCostsStmt(
        "SELECT task_run_id, COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents, COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens, COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens, COUNT(*)::bigint as call_count, MIN(created_at)::text as started_at FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) GROUP BY task_run_id ORDER BY SUM(cost_cents) DESC LIMIT $2",
        None,
    )
}
impl GetTaskRunCostsStmt {
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
        days: &'a i32,
        max_results: &'a i64,
    ) -> GetTaskRunCostsQuery<'c, 'a, 's, C, GetTaskRunCosts, 2> {
        GetTaskRunCostsQuery {
            client,
            params: [days, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskRunCostsBorrowed, tokio_postgres::Error> {
                Ok(GetTaskRunCostsBorrowed {
                    task_run_id: row.try_get(0)?,
                    total_cost_cents: row.try_get(1)?,
                    total_input_tokens: row.try_get(2)?,
                    total_output_tokens: row.try_get(3)?,
                    call_count: row.try_get(4)?,
                    started_at: row.try_get(5)?,
                })
            },
            mapper: |it| GetTaskRunCosts::from(it),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    GetTaskRunCostsParams,
    GetTaskRunCostsQuery<'c, 'a, 's, C, GetTaskRunCosts, 2>,
    C,
> for GetTaskRunCostsStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetTaskRunCostsParams,
    ) -> GetTaskRunCostsQuery<'c, 'a, 's, C, GetTaskRunCosts, 2> {
        self.bind(client, &params.days, &params.max_results)
    }
}
pub struct GetTokenUsageTotalsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_token_usage_totals() -> GetTokenUsageTotalsStmt {
    GetTokenUsageTotalsStmt(
        "SELECT COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents, COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens, COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens, COUNT(*)::bigint as total_calls, COALESCE(AVG(duration_ms), 0)::bigint as avg_duration_ms FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1)",
        None,
    )
}
impl GetTokenUsageTotalsStmt {
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
        days: &'a i32,
    ) -> GetTokenUsageTotalsQuery<'c, 'a, 's, C, GetTokenUsageTotals, 1> {
        GetTokenUsageTotalsQuery {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTokenUsageTotals, tokio_postgres::Error> {
                Ok(GetTokenUsageTotals {
                    total_cost_cents: row.try_get(0)?,
                    total_input_tokens: row.try_get(1)?,
                    total_output_tokens: row.try_get(2)?,
                    total_calls: row.try_get(3)?,
                    avg_duration_ms: row.try_get(4)?,
                })
            },
            mapper: |it| GetTokenUsageTotals::from(it),
        }
    }
}
pub struct GetUniqueModelsCountStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_unique_models_count() -> GetUniqueModelsCountStmt {
    GetUniqueModelsCountStmt(
        "SELECT COUNT(DISTINCT model_used)::bigint as count FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) AND model_used IS NOT NULL",
        None,
    )
}
impl GetUniqueModelsCountStmt {
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
        days: &'a i32,
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct GetUniqueProvidersCountStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_unique_providers_count() -> GetUniqueProvidersCountStmt {
    GetUniqueProvidersCountStmt(
        "SELECT COUNT(DISTINCT provider_used)::bigint as count FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) AND provider_used IS NOT NULL",
        None,
    )
}
impl GetUniqueProvidersCountStmt {
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
        days: &'a i32,
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct GetCostByTargetAppStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_cost_by_target_app() -> GetCostByTargetAppStmt {
    GetCostByTargetAppStmt(
        "SELECT COALESCE(target_app, '(no app)') as target_app, COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents, COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens, COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens, COUNT(*)::bigint as call_count, COALESCE(AVG(duration_ms), 0)::bigint as avg_duration_ms FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) AND target_app IS NOT NULL GROUP BY target_app ORDER BY SUM(cost_cents) DESC",
        None,
    )
}
impl GetCostByTargetAppStmt {
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
        days: &'a i32,
    ) -> GetCostByTargetAppQuery<'c, 'a, 's, C, GetCostByTargetApp, 1> {
        GetCostByTargetAppQuery {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetCostByTargetAppBorrowed, tokio_postgres::Error> {
                Ok(GetCostByTargetAppBorrowed {
                    target_app: row.try_get(0)?,
                    total_cost_cents: row.try_get(1)?,
                    total_input_tokens: row.try_get(2)?,
                    total_output_tokens: row.try_get(3)?,
                    call_count: row.try_get(4)?,
                    avg_duration_ms: row.try_get(5)?,
                })
            },
            mapper: |it| GetCostByTargetApp::from(it),
        }
    }
}
pub struct GetCostByTargetPageStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_cost_by_target_page() -> GetCostByTargetPageStmt {
    GetCostByTargetPageStmt(
        "SELECT COALESCE(target_page_url, '(no page)') as target_page_url, COALESCE(SUM(cost_cents), 0)::bigint as total_cost_cents, COALESCE(SUM(input_tokens), 0)::bigint as total_input_tokens, COALESCE(SUM(output_tokens), 0)::bigint as total_output_tokens, COUNT(*)::bigint as call_count FROM phase_token_usage WHERE created_at >= NOW() - make_interval(days => $1) AND target_page_url IS NOT NULL GROUP BY target_page_url ORDER BY SUM(cost_cents) DESC LIMIT 50",
        None,
    )
}
impl GetCostByTargetPageStmt {
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
        days: &'a i32,
    ) -> GetCostByTargetPageQuery<'c, 'a, 's, C, GetCostByTargetPage, 1> {
        GetCostByTargetPageQuery {
            client,
            params: [days],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetCostByTargetPageBorrowed, tokio_postgres::Error> {
                Ok(GetCostByTargetPageBorrowed {
                    target_page_url: row.try_get(0)?,
                    total_cost_cents: row.try_get(1)?,
                    total_input_tokens: row.try_get(2)?,
                    total_output_tokens: row.try_get(3)?,
                    call_count: row.try_get(4)?,
                })
            },
            mapper: |it| GetCostByTargetPage::from(it),
        }
    }
}
