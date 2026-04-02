// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct InsertUiBridgeEventParams<
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
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub sequence: i64,
    pub event_type: T1,
    pub element_id: Option<T2>,
    pub state_id: Option<T3>,
    pub transition_id: Option<T4>,
    pub action: Option<T5>,
    pub params: Option<T6>,
    pub result: Option<T7>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub error_message: Option<T8>,
    pub metadata: Option<T9>,
}
#[derive(Debug)]
pub struct GetElementHistoryParams<T1: crate::StringSql> {
    pub element_id: T1,
    pub max_results: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct GetFlakyElementsParams {
    pub min_interactions: i64,
    pub max_success_rate: f64,
}
#[derive(Debug)]
pub struct InsertStallEventParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
> {
    pub id: T1,
    pub task_run_id: T2,
    pub iteration: i32,
    pub pattern_type: T3,
    pub pattern_details: Option<T4>,
    pub action_count: Option<i32>,
    pub intervention_action: Option<T5>,
    pub intervention_result: Option<T6>,
}
#[derive(Debug)]
pub struct GetElementDecayCurveParams<T1: crate::StringSql> {
    pub window_ms: i64,
    pub element_id: T1,
    pub num_windows: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct GetFailureTaxonomyParams {
    pub since_epoch_ms: i64,
    pub max_results: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct GetAutomationRegressionsParams {
    pub since_epoch_ms: i64,
    pub max_results: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UiBridgeEventRow {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub sequence: i64,
    pub event_type: String,
    pub element_id: Option<String>,
    pub state_id: Option<String>,
    pub transition_id: Option<String>,
    pub action: Option<String>,
    pub params: Option<String>,
    pub result: Option<String>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub error_message: Option<String>,
    pub metadata: Option<String>,
}
pub struct UiBridgeEventRowBorrowed<'a> {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub sequence: i64,
    pub event_type: &'a str,
    pub element_id: Option<&'a str>,
    pub state_id: Option<&'a str>,
    pub transition_id: Option<&'a str>,
    pub action: Option<&'a str>,
    pub params: Option<&'a str>,
    pub result: Option<&'a str>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub error_message: Option<&'a str>,
    pub metadata: Option<&'a str>,
}
impl<'a> From<UiBridgeEventRowBorrowed<'a>> for UiBridgeEventRow {
    fn from(
        UiBridgeEventRowBorrowed {
            id,
            task_run_id,
            timestamp,
            sequence,
            event_type,
            element_id,
            state_id,
            transition_id,
            action,
            params,
            result,
            duration_ms,
            success,
            error_message,
            metadata,
        }: UiBridgeEventRowBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            task_run_id,
            timestamp,
            sequence,
            event_type: event_type.into(),
            element_id: element_id.map(|v| v.into()),
            state_id: state_id.map(|v| v.into()),
            transition_id: transition_id.map(|v| v.into()),
            action: action.map(|v| v.into()),
            params: params.map(|v| v.into()),
            result: result.map(|v| v.into()),
            duration_ms,
            success,
            error_message: error_message.map(|v| v.into()),
            metadata: metadata.map(|v| v.into()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetFlakyElements {
    pub element_id: String,
    pub total: i64,
    pub successes: i64,
    pub rate: f64,
}
pub struct GetFlakyElementsBorrowed<'a> {
    pub element_id: &'a str,
    pub total: i64,
    pub successes: i64,
    pub rate: f64,
}
impl<'a> From<GetFlakyElementsBorrowed<'a>> for GetFlakyElements {
    fn from(
        GetFlakyElementsBorrowed {
            element_id,
            total,
            successes,
            rate,
        }: GetFlakyElementsBorrowed<'a>,
    ) -> Self {
        Self {
            element_id: element_id.into(),
            total,
            successes,
            rate,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetElementReliability {
    pub element_id: String,
    pub total: i64,
    pub successes: i64,
    pub rate: f64,
}
pub struct GetElementReliabilityBorrowed<'a> {
    pub element_id: &'a str,
    pub total: i64,
    pub successes: i64,
    pub rate: f64,
}
impl<'a> From<GetElementReliabilityBorrowed<'a>> for GetElementReliability {
    fn from(
        GetElementReliabilityBorrowed {
            element_id,
            total,
            successes,
            rate,
        }: GetElementReliabilityBorrowed<'a>,
    ) -> Self {
        Self {
            element_id: element_id.into(),
            total,
            successes,
            rate,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetStallEvents {
    pub id: String,
    pub task_run_id: String,
    pub iteration: i32,
    pub pattern_type: String,
    pub pattern_details: String,
    pub action_count: i32,
    pub intervention_action: String,
    pub intervention_result: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetStallEventsBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub iteration: i32,
    pub pattern_type: &'a str,
    pub pattern_details: &'a str,
    pub action_count: i32,
    pub intervention_action: &'a str,
    pub intervention_result: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetStallEventsBorrowed<'a>> for GetStallEvents {
    fn from(
        GetStallEventsBorrowed {
            id,
            task_run_id,
            iteration,
            pattern_type,
            pattern_details,
            action_count,
            intervention_action,
            intervention_result,
            created_at,
        }: GetStallEventsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            iteration,
            pattern_type: pattern_type.into(),
            pattern_details: pattern_details.into(),
            action_count,
            intervention_action: intervention_action.into(),
            intervention_result: intervention_result.into(),
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct GetElementDecayCurve {
    pub bucket: i64,
    pub total: i64,
    pub successes: i64,
    pub rate: f64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetActionLatencyBaselines {
    pub action: String,
    pub cnt: i64,
    pub avg_dur: f64,
    pub min_dur: f64,
    pub max_dur: f64,
    pub rate: f64,
}
pub struct GetActionLatencyBaselinesBorrowed<'a> {
    pub action: &'a str,
    pub cnt: i64,
    pub avg_dur: f64,
    pub min_dur: f64,
    pub max_dur: f64,
    pub rate: f64,
}
impl<'a> From<GetActionLatencyBaselinesBorrowed<'a>> for GetActionLatencyBaselines {
    fn from(
        GetActionLatencyBaselinesBorrowed {
            action,
            cnt,
            avg_dur,
            min_dur,
            max_dur,
            rate,
        }: GetActionLatencyBaselinesBorrowed<'a>,
    ) -> Self {
        Self {
            action: action.into(),
            cnt,
            avg_dur,
            min_dur,
            max_dur,
            rate,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetFailureTaxonomy {
    pub error_message: String,
    pub freq: i64,
    pub elements: String,
}
pub struct GetFailureTaxonomyBorrowed<'a> {
    pub error_message: &'a str,
    pub freq: i64,
    pub elements: &'a str,
}
impl<'a> From<GetFailureTaxonomyBorrowed<'a>> for GetFailureTaxonomy {
    fn from(
        GetFailureTaxonomyBorrowed {
            error_message,
            freq,
            elements,
        }: GetFailureTaxonomyBorrowed<'a>,
    ) -> Self {
        Self {
            error_message: error_message.into(),
            freq,
            elements: elements.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetElementFragilityByRegion {
    pub element_id: String,
    pub bounds: String,
    pub cnt: i64,
    pub rate: f64,
}
pub struct GetElementFragilityByRegionBorrowed<'a> {
    pub element_id: &'a str,
    pub bounds: &'a str,
    pub cnt: i64,
    pub rate: f64,
}
impl<'a> From<GetElementFragilityByRegionBorrowed<'a>> for GetElementFragilityByRegion {
    fn from(
        GetElementFragilityByRegionBorrowed {
            element_id,
            bounds,
            cnt,
            rate,
        }: GetElementFragilityByRegionBorrowed<'a>,
    ) -> Self {
        Self {
            element_id: element_id.into(),
            bounds: bounds.into(),
            cnt,
            rate,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAutomationRegressions {
    pub element_id: String,
    pub action: String,
    pub prior_rate: f64,
    pub recent_rate: f64,
}
pub struct GetAutomationRegressionsBorrowed<'a> {
    pub element_id: &'a str,
    pub action: &'a str,
    pub prior_rate: f64,
    pub recent_rate: f64,
}
impl<'a> From<GetAutomationRegressionsBorrowed<'a>> for GetAutomationRegressions {
    fn from(
        GetAutomationRegressionsBorrowed {
            element_id,
            action,
            prior_rate,
            recent_rate,
        }: GetAutomationRegressionsBorrowed<'a>,
    ) -> Self {
        Self {
            element_id: element_id.into(),
            action: action.into(),
            prior_rate,
            recent_rate,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetStallFrequency {
    pub pattern_type: String,
    pub cnt: i64,
}
pub struct GetStallFrequencyBorrowed<'a> {
    pub pattern_type: &'a str,
    pub cnt: i64,
}
impl<'a> From<GetStallFrequencyBorrowed<'a>> for GetStallFrequency {
    fn from(
        GetStallFrequencyBorrowed { pattern_type, cnt }: GetStallFrequencyBorrowed<'a>,
    ) -> Self {
        Self {
            pattern_type: pattern_type.into(),
            cnt,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetInterventionEffectiveness {
    pub intervention_action: String,
    pub total: i64,
    pub successes: i64,
    pub rate: f64,
}
pub struct GetInterventionEffectivenessBorrowed<'a> {
    pub intervention_action: &'a str,
    pub total: i64,
    pub successes: i64,
    pub rate: f64,
}
impl<'a> From<GetInterventionEffectivenessBorrowed<'a>> for GetInterventionEffectiveness {
    fn from(
        GetInterventionEffectivenessBorrowed {
            intervention_action,
            total,
            successes,
            rate,
        }: GetInterventionEffectivenessBorrowed<'a>,
    ) -> Self {
        Self {
            intervention_action: intervention_action.into(),
            total,
            successes,
            rate,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetUnannotatedHighInteractionElements {
    pub element_id: String,
    pub interaction_count: i64,
    pub action_types: i64,
    pub success_rate: f64,
}
pub struct GetUnannotatedHighInteractionElementsBorrowed<'a> {
    pub element_id: &'a str,
    pub interaction_count: i64,
    pub action_types: i64,
    pub success_rate: f64,
}
impl<'a> From<GetUnannotatedHighInteractionElementsBorrowed<'a>>
    for GetUnannotatedHighInteractionElements
{
    fn from(
        GetUnannotatedHighInteractionElementsBorrowed {
            element_id,
            interaction_count,
            action_types,
            success_rate,
        }: GetUnannotatedHighInteractionElementsBorrowed<'a>,
    ) -> Self {
        Self {
            element_id: element_id.into(),
            interaction_count,
            action_types,
            success_rate,
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
pub struct UiBridgeEventRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<UiBridgeEventRowBorrowed, tokio_postgres::Error>,
    mapper: fn(UiBridgeEventRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> UiBridgeEventRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(UiBridgeEventRowBorrowed) -> R,
    ) -> UiBridgeEventRowQuery<'c, 'a, 's, C, R, N> {
        UiBridgeEventRowQuery {
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
pub struct GetFlakyElementsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetFlakyElementsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetFlakyElementsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetFlakyElementsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetFlakyElementsBorrowed) -> R,
    ) -> GetFlakyElementsQuery<'c, 'a, 's, C, R, N> {
        GetFlakyElementsQuery {
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
pub struct GetElementReliabilityQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetElementReliabilityBorrowed, tokio_postgres::Error>,
    mapper: fn(GetElementReliabilityBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetElementReliabilityQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetElementReliabilityBorrowed) -> R,
    ) -> GetElementReliabilityQuery<'c, 'a, 's, C, R, N> {
        GetElementReliabilityQuery {
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
pub struct GetStallEventsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetStallEventsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetStallEventsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetStallEventsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetStallEventsBorrowed) -> R,
    ) -> GetStallEventsQuery<'c, 'a, 's, C, R, N> {
        GetStallEventsQuery {
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
pub struct GetElementDecayCurveQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetElementDecayCurve, tokio_postgres::Error>,
    mapper: fn(GetElementDecayCurve) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetElementDecayCurveQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetElementDecayCurve) -> R,
    ) -> GetElementDecayCurveQuery<'c, 'a, 's, C, R, N> {
        GetElementDecayCurveQuery {
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
pub struct GetActionLatencyBaselinesQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetActionLatencyBaselinesBorrowed, tokio_postgres::Error>,
    mapper: fn(GetActionLatencyBaselinesBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetActionLatencyBaselinesQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetActionLatencyBaselinesBorrowed) -> R,
    ) -> GetActionLatencyBaselinesQuery<'c, 'a, 's, C, R, N> {
        GetActionLatencyBaselinesQuery {
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
pub struct GetFailureTaxonomyQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetFailureTaxonomyBorrowed, tokio_postgres::Error>,
    mapper: fn(GetFailureTaxonomyBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetFailureTaxonomyQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetFailureTaxonomyBorrowed) -> R,
    ) -> GetFailureTaxonomyQuery<'c, 'a, 's, C, R, N> {
        GetFailureTaxonomyQuery {
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
pub struct GetElementFragilityByRegionQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetElementFragilityByRegionBorrowed, tokio_postgres::Error>,
    mapper: fn(GetElementFragilityByRegionBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetElementFragilityByRegionQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetElementFragilityByRegionBorrowed) -> R,
    ) -> GetElementFragilityByRegionQuery<'c, 'a, 's, C, R, N> {
        GetElementFragilityByRegionQuery {
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
pub struct GetAutomationRegressionsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetAutomationRegressionsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetAutomationRegressionsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetAutomationRegressionsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetAutomationRegressionsBorrowed) -> R,
    ) -> GetAutomationRegressionsQuery<'c, 'a, 's, C, R, N> {
        GetAutomationRegressionsQuery {
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
pub struct GetStallFrequencyQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetStallFrequencyBorrowed, tokio_postgres::Error>,
    mapper: fn(GetStallFrequencyBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetStallFrequencyQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetStallFrequencyBorrowed) -> R,
    ) -> GetStallFrequencyQuery<'c, 'a, 's, C, R, N> {
        GetStallFrequencyQuery {
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
pub struct GetInterventionEffectivenessQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetInterventionEffectivenessBorrowed, tokio_postgres::Error>,
    mapper: fn(GetInterventionEffectivenessBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetInterventionEffectivenessQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetInterventionEffectivenessBorrowed) -> R,
    ) -> GetInterventionEffectivenessQuery<'c, 'a, 's, C, R, N> {
        GetInterventionEffectivenessQuery {
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
pub struct GetUnannotatedHighInteractionElementsQuery<
    'c,
    'a,
    's,
    C: GenericClient,
    T,
    const N: usize,
> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    )
        -> Result<GetUnannotatedHighInteractionElementsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetUnannotatedHighInteractionElementsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize>
    GetUnannotatedHighInteractionElementsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetUnannotatedHighInteractionElementsBorrowed) -> R,
    ) -> GetUnannotatedHighInteractionElementsQuery<'c, 'a, 's, C, R, N> {
        GetUnannotatedHighInteractionElementsQuery {
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
pub struct InsertUiBridgeEventStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_ui_bridge_event() -> InsertUiBridgeEventStmt {
    InsertUiBridgeEventStmt(
        "INSERT INTO ui_bridge_events (task_run_id, timestamp, sequence, event_type, element_id, state_id, transition_id, action, params, result, duration_ms, success, error_message, metadata) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING id",
        None,
    )
}
impl InsertUiBridgeEventStmt {
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
        task_run_id: &'a Option<i64>,
        timestamp: &'a i64,
        sequence: &'a i64,
        event_type: &'a T1,
        element_id: &'a Option<T2>,
        state_id: &'a Option<T3>,
        transition_id: &'a Option<T4>,
        action: &'a Option<T5>,
        params: &'a Option<T6>,
        result: &'a Option<T7>,
        duration_ms: &'a Option<f64>,
        success: &'a bool,
        error_message: &'a Option<T8>,
        metadata: &'a Option<T9>,
    ) -> I64Query<'c, 'a, 's, C, i64, 14> {
        I64Query {
            client,
            params: [
                task_run_id,
                timestamp,
                sequence,
                event_type,
                element_id,
                state_id,
                transition_id,
                action,
                params,
                result,
                duration_ms,
                success,
                error_message,
                metadata,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        InsertUiBridgeEventParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
        I64Query<'c, 'a, 's, C, i64, 14>,
        C,
    > for InsertUiBridgeEventStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a InsertUiBridgeEventParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    ) -> I64Query<'c, 'a, 's, C, i64, 14> {
        self.bind(
            client,
            &params.task_run_id,
            &params.timestamp,
            &params.sequence,
            &params.event_type,
            &params.element_id,
            &params.state_id,
            &params.transition_id,
            &params.action,
            &params.params,
            &params.result,
            &params.duration_ms,
            &params.success,
            &params.error_message,
            &params.metadata,
        )
    }
}
pub struct GetElementInteractionsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_element_interactions() -> GetElementInteractionsStmt {
    GetElementInteractionsStmt(
        "SELECT id, task_run_id, timestamp, sequence, event_type, element_id, state_id, transition_id, action, params, result, duration_ms, success, error_message, metadata FROM ui_bridge_events WHERE task_run_id = $1 ORDER BY sequence ASC",
        None,
    )
}
impl GetElementInteractionsStmt {
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
        task_run_id: &'a i64,
    ) -> UiBridgeEventRowQuery<'c, 'a, 's, C, UiBridgeEventRow, 1> {
        UiBridgeEventRowQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<UiBridgeEventRowBorrowed, tokio_postgres::Error> {
                Ok(UiBridgeEventRowBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    timestamp: row.try_get(2)?,
                    sequence: row.try_get(3)?,
                    event_type: row.try_get(4)?,
                    element_id: row.try_get(5)?,
                    state_id: row.try_get(6)?,
                    transition_id: row.try_get(7)?,
                    action: row.try_get(8)?,
                    params: row.try_get(9)?,
                    result: row.try_get(10)?,
                    duration_ms: row.try_get(11)?,
                    success: row.try_get(12)?,
                    error_message: row.try_get(13)?,
                    metadata: row.try_get(14)?,
                })
            },
            mapper: |it| UiBridgeEventRow::from(it),
        }
    }
}
pub struct GetElementHistoryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_element_history() -> GetElementHistoryStmt {
    GetElementHistoryStmt(
        "SELECT id, task_run_id, timestamp, sequence, event_type, element_id, state_id, transition_id, action, params, result, duration_ms, success, error_message, metadata FROM ui_bridge_events WHERE element_id = $1 ORDER BY timestamp DESC LIMIT $2",
        None,
    )
}
impl GetElementHistoryStmt {
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
        element_id: &'a T1,
        max_results: &'a i64,
    ) -> UiBridgeEventRowQuery<'c, 'a, 's, C, UiBridgeEventRow, 2> {
        UiBridgeEventRowQuery {
            client,
            params: [element_id, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<UiBridgeEventRowBorrowed, tokio_postgres::Error> {
                Ok(UiBridgeEventRowBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    timestamp: row.try_get(2)?,
                    sequence: row.try_get(3)?,
                    event_type: row.try_get(4)?,
                    element_id: row.try_get(5)?,
                    state_id: row.try_get(6)?,
                    transition_id: row.try_get(7)?,
                    action: row.try_get(8)?,
                    params: row.try_get(9)?,
                    result: row.try_get(10)?,
                    duration_ms: row.try_get(11)?,
                    success: row.try_get(12)?,
                    error_message: row.try_get(13)?,
                    metadata: row.try_get(14)?,
                })
            },
            mapper: |it| UiBridgeEventRow::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetElementHistoryParams<T1>,
        UiBridgeEventRowQuery<'c, 'a, 's, C, UiBridgeEventRow, 2>,
        C,
    > for GetElementHistoryStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetElementHistoryParams<T1>,
    ) -> UiBridgeEventRowQuery<'c, 'a, 's, C, UiBridgeEventRow, 2> {
        self.bind(client, &params.element_id, &params.max_results)
    }
}
pub struct GetFlakyElementsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_flaky_elements() -> GetFlakyElementsStmt {
    GetFlakyElementsStmt(
        "SELECT element_id, COUNT(*)::bigint AS total, COUNT(*) FILTER (WHERE success)::bigint AS successes, COUNT(*) FILTER (WHERE success)::double precision / COUNT(*) AS rate FROM ui_bridge_events WHERE element_id IS NOT NULL AND event_type = 'action_executed' GROUP BY element_id HAVING COUNT(*) >= $1 AND COUNT(*) FILTER (WHERE success)::double precision / COUNT(*) <= $2 ORDER BY rate ASC",
        None,
    )
}
impl GetFlakyElementsStmt {
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
        min_interactions: &'a i64,
        max_success_rate: &'a f64,
    ) -> GetFlakyElementsQuery<'c, 'a, 's, C, GetFlakyElements, 2> {
        GetFlakyElementsQuery {
            client,
            params: [min_interactions, max_success_rate],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetFlakyElementsBorrowed, tokio_postgres::Error> {
                Ok(GetFlakyElementsBorrowed {
                    element_id: row.try_get(0)?,
                    total: row.try_get(1)?,
                    successes: row.try_get(2)?,
                    rate: row.try_get(3)?,
                })
            },
            mapper: |it| GetFlakyElements::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetFlakyElementsParams,
        GetFlakyElementsQuery<'c, 'a, 's, C, GetFlakyElements, 2>,
        C,
    > for GetFlakyElementsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetFlakyElementsParams,
    ) -> GetFlakyElementsQuery<'c, 'a, 's, C, GetFlakyElements, 2> {
        self.bind(client, &params.min_interactions, &params.max_success_rate)
    }
}
pub struct GetElementReliabilityStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_element_reliability() -> GetElementReliabilityStmt {
    GetElementReliabilityStmt(
        "SELECT element_id, COUNT(*)::bigint AS total, COUNT(*) FILTER (WHERE success)::bigint AS successes, COUNT(*) FILTER (WHERE success)::double precision / GREATEST(COUNT(*), 1) AS rate FROM ui_bridge_events WHERE element_id = $1 AND event_type = 'action_executed' GROUP BY element_id",
        None,
    )
}
impl GetElementReliabilityStmt {
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
        element_id: &'a T1,
    ) -> GetElementReliabilityQuery<'c, 'a, 's, C, GetElementReliability, 1> {
        GetElementReliabilityQuery {
            client,
            params: [element_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetElementReliabilityBorrowed, tokio_postgres::Error> {
                Ok(GetElementReliabilityBorrowed {
                    element_id: row.try_get(0)?,
                    total: row.try_get(1)?,
                    successes: row.try_get(2)?,
                    rate: row.try_get(3)?,
                })
            },
            mapper: |it| GetElementReliability::from(it),
        }
    }
}
pub struct GetLastFailureReasonStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_last_failure_reason() -> GetLastFailureReasonStmt {
    GetLastFailureReasonStmt(
        "SELECT error_message FROM ui_bridge_events WHERE element_id = $1 AND NOT success AND error_message IS NOT NULL ORDER BY timestamp DESC LIMIT 1",
        None,
    )
}
impl GetLastFailureReasonStmt {
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
        element_id: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [element_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct InsertStallEventStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_stall_event() -> InsertStallEventStmt {
    InsertStallEventStmt(
        "INSERT INTO stall_events (id, task_run_id, iteration, pattern_type, pattern_details, action_count, intervention_action, intervention_result) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        None,
    )
}
impl InsertStallEventStmt {
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
        task_run_id: &'a T2,
        iteration: &'a i32,
        pattern_type: &'a T3,
        pattern_details: &'a Option<T4>,
        action_count: &'a Option<i32>,
        intervention_action: &'a Option<T5>,
        intervention_result: &'a Option<T6>,
    ) -> StringQuery<'c, 'a, 's, C, String, 8> {
        StringQuery {
            client,
            params: [
                id,
                task_run_id,
                iteration,
                pattern_type,
                pattern_details,
                action_count,
                intervention_action,
                intervention_result,
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
        InsertStallEventParams<T1, T2, T3, T4, T5, T6>,
        StringQuery<'c, 'a, 's, C, String, 8>,
        C,
    > for InsertStallEventStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a InsertStallEventParams<T1, T2, T3, T4, T5, T6>,
    ) -> StringQuery<'c, 'a, 's, C, String, 8> {
        self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.iteration,
            &params.pattern_type,
            &params.pattern_details,
            &params.action_count,
            &params.intervention_action,
            &params.intervention_result,
        )
    }
}
pub struct GetStallEventsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_stall_events() -> GetStallEventsStmt {
    GetStallEventsStmt(
        "SELECT id, task_run_id, iteration, pattern_type, COALESCE(pattern_details, '') as pattern_details, COALESCE(action_count, 0) as action_count, COALESCE(intervention_action, '') as intervention_action, COALESCE(intervention_result, '') as intervention_result, created_at FROM stall_events WHERE task_run_id = $1 ORDER BY created_at ASC",
        None,
    )
}
impl GetStallEventsStmt {
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
    ) -> GetStallEventsQuery<'c, 'a, 's, C, GetStallEvents, 1> {
        GetStallEventsQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetStallEventsBorrowed, tokio_postgres::Error> {
                Ok(GetStallEventsBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    iteration: row.try_get(2)?,
                    pattern_type: row.try_get(3)?,
                    pattern_details: row.try_get(4)?,
                    action_count: row.try_get(5)?,
                    intervention_action: row.try_get(6)?,
                    intervention_result: row.try_get(7)?,
                    created_at: row.try_get(8)?,
                })
            },
            mapper: |it| GetStallEvents::from(it),
        }
    }
}
pub struct GetElementDecayCurveStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_element_decay_curve() -> GetElementDecayCurveStmt {
    GetElementDecayCurveStmt(
        "SELECT (timestamp / $1) AS bucket, COUNT(*)::bigint AS total, COALESCE(COUNT(*) FILTER (WHERE success), 0)::bigint AS successes, COALESCE(COUNT(*) FILTER (WHERE success), 0)::double precision / GREATEST(COUNT(*), 1) AS rate FROM ui_bridge_events WHERE element_id = $2 AND event_type = 'action_executed' GROUP BY bucket ORDER BY bucket DESC LIMIT $3",
        None,
    )
}
impl GetElementDecayCurveStmt {
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
        window_ms: &'a i64,
        element_id: &'a T1,
        num_windows: &'a i64,
    ) -> GetElementDecayCurveQuery<'c, 'a, 's, C, GetElementDecayCurve, 3> {
        GetElementDecayCurveQuery {
            client,
            params: [window_ms, element_id, num_windows],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<GetElementDecayCurve, tokio_postgres::Error> {
                    Ok(GetElementDecayCurve {
                        bucket: row.try_get(0)?,
                        total: row.try_get(1)?,
                        successes: row.try_get(2)?,
                        rate: row.try_get(3)?,
                    })
                },
            mapper: |it| GetElementDecayCurve::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetElementDecayCurveParams<T1>,
        GetElementDecayCurveQuery<'c, 'a, 's, C, GetElementDecayCurve, 3>,
        C,
    > for GetElementDecayCurveStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetElementDecayCurveParams<T1>,
    ) -> GetElementDecayCurveQuery<'c, 'a, 's, C, GetElementDecayCurve, 3> {
        self.bind(
            client,
            &params.window_ms,
            &params.element_id,
            &params.num_windows,
        )
    }
}
pub struct GetActionLatencyBaselinesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_action_latency_baselines() -> GetActionLatencyBaselinesStmt {
    GetActionLatencyBaselinesStmt(
        "SELECT action, COUNT(*)::bigint AS cnt, COALESCE(AVG(duration_ms), 0)::double precision AS avg_dur, COALESCE(MIN(duration_ms), 0)::double precision AS min_dur, COALESCE(MAX(duration_ms), 0)::double precision AS max_dur, COALESCE(COUNT(*) FILTER (WHERE success), 0)::double precision / GREATEST(COUNT(*), 1) AS rate FROM ui_bridge_events WHERE event_type = 'action_executed' AND action IS NOT NULL AND timestamp >= $1 GROUP BY action ORDER BY cnt DESC LIMIT 50",
        None,
    )
}
impl GetActionLatencyBaselinesStmt {
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
        since_epoch_ms: &'a i64,
    ) -> GetActionLatencyBaselinesQuery<'c, 'a, 's, C, GetActionLatencyBaselines, 1> {
        GetActionLatencyBaselinesQuery {
            client,
            params: [since_epoch_ms],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetActionLatencyBaselinesBorrowed, tokio_postgres::Error> {
                Ok(GetActionLatencyBaselinesBorrowed {
                    action: row.try_get(0)?,
                    cnt: row.try_get(1)?,
                    avg_dur: row.try_get(2)?,
                    min_dur: row.try_get(3)?,
                    max_dur: row.try_get(4)?,
                    rate: row.try_get(5)?,
                })
            },
            mapper: |it| GetActionLatencyBaselines::from(it),
        }
    }
}
pub struct GetFailureTaxonomyStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_failure_taxonomy() -> GetFailureTaxonomyStmt {
    GetFailureTaxonomyStmt(
        "SELECT error_message, COUNT(*)::bigint AS freq, STRING_AGG(DISTINCT element_id, ',') AS elements FROM ui_bridge_events WHERE NOT success AND error_message IS NOT NULL AND timestamp >= $1 GROUP BY error_message ORDER BY freq DESC LIMIT $2",
        None,
    )
}
impl GetFailureTaxonomyStmt {
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
        since_epoch_ms: &'a i64,
        max_results: &'a i64,
    ) -> GetFailureTaxonomyQuery<'c, 'a, 's, C, GetFailureTaxonomy, 2> {
        GetFailureTaxonomyQuery {
            client,
            params: [since_epoch_ms, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetFailureTaxonomyBorrowed, tokio_postgres::Error> {
                Ok(GetFailureTaxonomyBorrowed {
                    error_message: row.try_get(0)?,
                    freq: row.try_get(1)?,
                    elements: row.try_get(2)?,
                })
            },
            mapper: |it| GetFailureTaxonomy::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetFailureTaxonomyParams,
        GetFailureTaxonomyQuery<'c, 'a, 's, C, GetFailureTaxonomy, 2>,
        C,
    > for GetFailureTaxonomyStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetFailureTaxonomyParams,
    ) -> GetFailureTaxonomyQuery<'c, 'a, 's, C, GetFailureTaxonomy, 2> {
        self.bind(client, &params.since_epoch_ms, &params.max_results)
    }
}
pub struct GetElementFragilityByRegionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_element_fragility_by_region() -> GetElementFragilityByRegionStmt {
    GetElementFragilityByRegionStmt(
        "SELECT ev.element_id, el.bounds, COUNT(*)::bigint AS cnt, COALESCE(COUNT(*) FILTER (WHERE ev.success), 0)::double precision / GREATEST(COUNT(*), 1) AS rate FROM ui_bridge_events ev LEFT JOIN ui_bridge_elements el ON ev.element_id = el.element_id WHERE ev.event_type = 'action_executed' AND ev.element_id IS NOT NULL AND ev.timestamp >= $1 GROUP BY ev.element_id, el.bounds HAVING COUNT(*) >= 3 ORDER BY rate ASC LIMIT 100",
        None,
    )
}
impl GetElementFragilityByRegionStmt {
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
        since_epoch_ms: &'a i64,
    ) -> GetElementFragilityByRegionQuery<'c, 'a, 's, C, GetElementFragilityByRegion, 1> {
        GetElementFragilityByRegionQuery {
            client,
            params: [since_epoch_ms],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetElementFragilityByRegionBorrowed, tokio_postgres::Error> {
                Ok(GetElementFragilityByRegionBorrowed {
                    element_id: row.try_get(0)?,
                    bounds: row.try_get(1)?,
                    cnt: row.try_get(2)?,
                    rate: row.try_get(3)?,
                })
            },
            mapper: |it| GetElementFragilityByRegion::from(it),
        }
    }
}
pub struct GetAutomationRegressionsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_automation_regressions() -> GetAutomationRegressionsStmt {
    GetAutomationRegressionsStmt(
        "WITH element_splits AS ( SELECT element_id, action, timestamp, success, NTILE(2) OVER (PARTITION BY element_id, action ORDER BY timestamp) AS half FROM ui_bridge_events WHERE event_type = 'action_executed' AND element_id IS NOT NULL AND action IS NOT NULL AND timestamp >= $1 ) SELECT element_id, action, SUM(CASE WHEN half = 1 AND success THEN 1 ELSE 0 END)::double precision / GREATEST(SUM(CASE WHEN half = 1 THEN 1 ELSE 0 END), 1) AS prior_rate, SUM(CASE WHEN half = 2 AND success THEN 1 ELSE 0 END)::double precision / GREATEST(SUM(CASE WHEN half = 2 THEN 1 ELSE 0 END), 1) AS recent_rate FROM element_splits GROUP BY element_id, action HAVING COUNT(*) >= 4 AND SUM(CASE WHEN half = 2 AND success THEN 1 ELSE 0 END)::double precision / GREATEST(SUM(CASE WHEN half = 2 THEN 1 ELSE 0 END), 1) < SUM(CASE WHEN half = 1 AND success THEN 1 ELSE 0 END)::double precision / GREATEST(SUM(CASE WHEN half = 1 THEN 1 ELSE 0 END), 1) - 0.1 ORDER BY ( SUM(CASE WHEN half = 1 AND success THEN 1 ELSE 0 END)::double precision / GREATEST(SUM(CASE WHEN half = 1 THEN 1 ELSE 0 END), 1) - SUM(CASE WHEN half = 2 AND success THEN 1 ELSE 0 END)::double precision / GREATEST(SUM(CASE WHEN half = 2 THEN 1 ELSE 0 END), 1) ) DESC LIMIT $2",
        None,
    )
}
impl GetAutomationRegressionsStmt {
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
        since_epoch_ms: &'a i64,
        max_results: &'a i64,
    ) -> GetAutomationRegressionsQuery<'c, 'a, 's, C, GetAutomationRegressions, 2> {
        GetAutomationRegressionsQuery {
            client,
            params: [since_epoch_ms, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetAutomationRegressionsBorrowed, tokio_postgres::Error> {
                Ok(GetAutomationRegressionsBorrowed {
                    element_id: row.try_get(0)?,
                    action: row.try_get(1)?,
                    prior_rate: row.try_get(2)?,
                    recent_rate: row.try_get(3)?,
                })
            },
            mapper: |it| GetAutomationRegressions::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetAutomationRegressionsParams,
        GetAutomationRegressionsQuery<'c, 'a, 's, C, GetAutomationRegressions, 2>,
        C,
    > for GetAutomationRegressionsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetAutomationRegressionsParams,
    ) -> GetAutomationRegressionsQuery<'c, 'a, 's, C, GetAutomationRegressions, 2> {
        self.bind(client, &params.since_epoch_ms, &params.max_results)
    }
}
pub struct GetStallFrequencyStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_stall_frequency() -> GetStallFrequencyStmt {
    GetStallFrequencyStmt(
        "SELECT pattern_type, COUNT(*)::bigint AS cnt FROM stall_events WHERE created_at >= $1::timestamptz GROUP BY pattern_type ORDER BY cnt DESC",
        None,
    )
}
impl GetStallFrequencyStmt {
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
        since_timestamp: &'a chrono::DateTime<chrono::FixedOffset>,
    ) -> GetStallFrequencyQuery<'c, 'a, 's, C, GetStallFrequency, 1> {
        GetStallFrequencyQuery {
            client,
            params: [since_timestamp],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetStallFrequencyBorrowed, tokio_postgres::Error> {
                Ok(GetStallFrequencyBorrowed {
                    pattern_type: row.try_get(0)?,
                    cnt: row.try_get(1)?,
                })
            },
            mapper: |it| GetStallFrequency::from(it),
        }
    }
}
pub struct GetInterventionEffectivenessStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_intervention_effectiveness() -> GetInterventionEffectivenessStmt {
    GetInterventionEffectivenessStmt(
        "SELECT intervention_action, COUNT(*)::bigint AS total, COUNT(*) FILTER (WHERE intervention_result = 'success')::bigint AS successes, COUNT(*) FILTER (WHERE intervention_result = 'success')::double precision / GREATEST(COUNT(*), 1) AS rate FROM stall_events WHERE created_at >= $1::timestamptz AND intervention_action IS NOT NULL GROUP BY intervention_action ORDER BY total DESC",
        None,
    )
}
impl GetInterventionEffectivenessStmt {
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
        since_timestamp: &'a chrono::DateTime<chrono::FixedOffset>,
    ) -> GetInterventionEffectivenessQuery<'c, 'a, 's, C, GetInterventionEffectiveness, 1> {
        GetInterventionEffectivenessQuery {
            client,
            params: [since_timestamp],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetInterventionEffectivenessBorrowed, tokio_postgres::Error> {
                Ok(GetInterventionEffectivenessBorrowed {
                    intervention_action: row.try_get(0)?,
                    total: row.try_get(1)?,
                    successes: row.try_get(2)?,
                    rate: row.try_get(3)?,
                })
            },
            mapper: |it| GetInterventionEffectiveness::from(it),
        }
    }
}
pub struct GetStateCoverageStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_state_coverage() -> GetStateCoverageStmt {
    GetStateCoverageStmt(
        "SELECT DISTINCT state_id FROM ui_bridge_events WHERE task_run_id = $1 AND state_id IS NOT NULL",
        None,
    )
}
impl GetStateCoverageStmt {
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
        task_run_id: &'a i64,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct GetUnannotatedHighInteractionElementsStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn get_unannotated_high_interaction_elements() -> GetUnannotatedHighInteractionElementsStmt {
    GetUnannotatedHighInteractionElementsStmt(
        "SELECT element_id, COUNT(*)::bigint AS interaction_count, COUNT(DISTINCT action)::bigint AS action_types, COALESCE(COUNT(*) FILTER (WHERE success), 0)::double precision / GREATEST(COUNT(*), 1) AS success_rate FROM ui_bridge_events WHERE element_id IS NOT NULL AND event_type = 'action_executed' GROUP BY element_id HAVING COUNT(*) >= $1 ORDER BY interaction_count DESC",
        None,
    )
}
impl GetUnannotatedHighInteractionElementsStmt {
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
        min_interactions: &'a i64,
    ) -> GetUnannotatedHighInteractionElementsQuery<
        'c,
        'a,
        's,
        C,
        GetUnannotatedHighInteractionElements,
        1,
    > {
        GetUnannotatedHighInteractionElementsQuery {
            client,
            params: [min_interactions],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row: &tokio_postgres::Row| -> Result<
                GetUnannotatedHighInteractionElementsBorrowed,
                tokio_postgres::Error,
            > {
                Ok(GetUnannotatedHighInteractionElementsBorrowed {
                    element_id: row.try_get(0)?,
                    interaction_count: row.try_get(1)?,
                    action_types: row.try_get(2)?,
                    success_rate: row.try_get(3)?,
                })
            },
            mapper: |it| GetUnannotatedHighInteractionElements::from(it),
        }
    }
}
