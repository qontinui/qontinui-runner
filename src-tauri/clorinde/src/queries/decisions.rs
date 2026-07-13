// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct SaveDecisionParams<
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
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
    T17: crate::StringSql,
    T18: crate::StringSql,
> {
    pub id: T1,
    pub scale: T2,
    pub category: T3,
    pub status: T4,
    pub title: T5,
    pub summary: T6,
    pub rationale: T7,
    pub alternatives_json: T8,
    pub tradeoffs_json: T9,
    pub triggered_by: Option<T10>,
    pub inspiration_json: Option<T11>,
    pub related_decisions_json: T12,
    pub affected_files_json: T13,
    pub affected_endpoints_json: T14,
    pub affected_tables_json: T15,
    pub created_by: Option<T16>,
    pub superseded_by: Option<T17>,
    pub tags_json: T18,
}
#[derive(Debug)]
pub struct ListDecisionsByCategoryParams<T1: crate::StringSql> {
    pub category: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct ListDecisionsByScaleParams<T1: crate::StringSql> {
    pub scale: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct SearchDecisionsParams<T1: crate::StringSql> {
    pub query: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct UpdateDecisionStatusParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub status: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct SupersedeDecisionParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub superseded_by: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct UpdateDecisionParams<
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
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
> {
    pub title: Option<T1>,
    pub summary: Option<T2>,
    pub rationale: Option<T3>,
    pub alternatives_json: Option<T4>,
    pub tradeoffs_json: Option<T5>,
    pub tags_json: Option<T6>,
    pub triggered_by: Option<T7>,
    pub inspiration_json: Option<T8>,
    pub related_decisions_json: Option<T9>,
    pub affected_files_json: Option<T10>,
    pub affected_endpoints_json: Option<T11>,
    pub affected_tables_json: Option<T12>,
    pub id: T13,
}
#[derive(Debug)]
pub struct SaveConceptSummaryParams<
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
    pub tagline: T3,
    pub description: T4,
    pub inspiration_json: Option<T5>,
    pub benefits_json: T6,
    pub components_json: T7,
    pub related_decisions_json: T8,
    pub metrics_json: Option<T9>,
}
#[derive(Debug)]
pub struct SearchConceptSummariesParams<T1: crate::StringSql> {
    pub query: T1,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct UpdateConceptSummaryParams<
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
    pub tagline: Option<T2>,
    pub description: Option<T3>,
    pub inspiration_json: Option<T4>,
    pub benefits_json: Option<T5>,
    pub components_json: Option<T6>,
    pub related_decisions_json: Option<T7>,
    pub metrics_json: Option<T8>,
    pub id: T9,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetDecision {
    pub id: String,
    pub timestamp: chrono::DateTime<chrono::FixedOffset>,
    pub scale: String,
    pub category: String,
    pub status: String,
    pub title: String,
    pub summary: String,
    pub rationale: String,
    pub alternatives_json: String,
    pub tradeoffs_json: String,
    pub triggered_by: Option<String>,
    pub inspiration_json: Option<String>,
    pub related_decisions_json: String,
    pub affected_files_json: String,
    pub affected_endpoints_json: String,
    pub affected_tables_json: String,
    pub created_by: Option<String>,
    pub superseded_by: Option<String>,
    pub tags_json: String,
    pub is_deleted: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetDecisionBorrowed<'a> {
    pub id: &'a str,
    pub timestamp: chrono::DateTime<chrono::FixedOffset>,
    pub scale: &'a str,
    pub category: &'a str,
    pub status: &'a str,
    pub title: &'a str,
    pub summary: &'a str,
    pub rationale: &'a str,
    pub alternatives_json: &'a str,
    pub tradeoffs_json: &'a str,
    pub triggered_by: Option<&'a str>,
    pub inspiration_json: Option<&'a str>,
    pub related_decisions_json: &'a str,
    pub affected_files_json: &'a str,
    pub affected_endpoints_json: &'a str,
    pub affected_tables_json: &'a str,
    pub created_by: Option<&'a str>,
    pub superseded_by: Option<&'a str>,
    pub tags_json: &'a str,
    pub is_deleted: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetDecisionBorrowed<'a>> for GetDecision {
    fn from(
        GetDecisionBorrowed {
            id,
            timestamp,
            scale,
            category,
            status,
            title,
            summary,
            rationale,
            alternatives_json,
            tradeoffs_json,
            triggered_by,
            inspiration_json,
            related_decisions_json,
            affected_files_json,
            affected_endpoints_json,
            affected_tables_json,
            created_by,
            superseded_by,
            tags_json,
            is_deleted,
            created_at,
            updated_at,
        }: GetDecisionBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            timestamp,
            scale: scale.into(),
            category: category.into(),
            status: status.into(),
            title: title.into(),
            summary: summary.into(),
            rationale: rationale.into(),
            alternatives_json: alternatives_json.into(),
            tradeoffs_json: tradeoffs_json.into(),
            triggered_by: triggered_by.map(|v| v.into()),
            inspiration_json: inspiration_json.map(|v| v.into()),
            related_decisions_json: related_decisions_json.into(),
            affected_files_json: affected_files_json.into(),
            affected_endpoints_json: affected_endpoints_json.into(),
            affected_tables_json: affected_tables_json.into(),
            created_by: created_by.map(|v| v.into()),
            superseded_by: superseded_by.map(|v| v.into()),
            tags_json: tags_json.into(),
            is_deleted,
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionPreview {
    pub id: String,
    pub timestamp: chrono::DateTime<chrono::FixedOffset>,
    pub scale: String,
    pub category: String,
    pub status: String,
    pub title: String,
    pub summary_preview: String,
    pub triggered_by: Option<String>,
    pub tags_json: String,
    pub created_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct DecisionPreviewBorrowed<'a> {
    pub id: &'a str,
    pub timestamp: chrono::DateTime<chrono::FixedOffset>,
    pub scale: &'a str,
    pub category: &'a str,
    pub status: &'a str,
    pub title: &'a str,
    pub summary_preview: &'a str,
    pub triggered_by: Option<&'a str>,
    pub tags_json: &'a str,
    pub created_by: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<DecisionPreviewBorrowed<'a>> for DecisionPreview {
    fn from(
        DecisionPreviewBorrowed {
            id,
            timestamp,
            scale,
            category,
            status,
            title,
            summary_preview,
            triggered_by,
            tags_json,
            created_by,
            created_at,
            updated_at,
        }: DecisionPreviewBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            timestamp,
            scale: scale.into(),
            category: category.into(),
            status: status.into(),
            title: title.into(),
            summary_preview: summary_preview.into(),
            triggered_by: triggered_by.map(|v| v.into()),
            tags_json: tags_json.into(),
            created_by: created_by.map(|v| v.into()),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionSearchRow {
    pub id: String,
    pub timestamp: chrono::DateTime<chrono::FixedOffset>,
    pub scale: String,
    pub category: String,
    pub status: String,
    pub title: String,
    pub summary_preview: String,
    pub triggered_by: Option<String>,
    pub tags_json: String,
    pub created_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: Option<f32>,
}
pub struct DecisionSearchRowBorrowed<'a> {
    pub id: &'a str,
    pub timestamp: chrono::DateTime<chrono::FixedOffset>,
    pub scale: &'a str,
    pub category: &'a str,
    pub status: &'a str,
    pub title: &'a str,
    pub summary_preview: &'a str,
    pub triggered_by: Option<&'a str>,
    pub tags_json: &'a str,
    pub created_by: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: Option<f32>,
}
impl<'a> From<DecisionSearchRowBorrowed<'a>> for DecisionSearchRow {
    fn from(
        DecisionSearchRowBorrowed {
            id,
            timestamp,
            scale,
            category,
            status,
            title,
            summary_preview,
            triggered_by,
            tags_json,
            created_by,
            created_at,
            updated_at,
            rank,
        }: DecisionSearchRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            timestamp,
            scale: scale.into(),
            category: category.into(),
            status: status.into(),
            title: title.into(),
            summary_preview: summary_preview.into(),
            triggered_by: triggered_by.map(|v| v.into()),
            tags_json: tags_json.into(),
            created_by: created_by.map(|v| v.into()),
            created_at,
            updated_at,
            rank,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetConceptSummary {
    pub id: String,
    pub name: String,
    pub tagline: String,
    pub description: String,
    pub inspiration_json: Option<String>,
    pub benefits_json: String,
    pub components_json: String,
    pub related_decisions_json: String,
    pub metrics_json: Option<String>,
    pub is_deleted: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetConceptSummaryBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub tagline: &'a str,
    pub description: &'a str,
    pub inspiration_json: Option<&'a str>,
    pub benefits_json: &'a str,
    pub components_json: &'a str,
    pub related_decisions_json: &'a str,
    pub metrics_json: Option<&'a str>,
    pub is_deleted: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetConceptSummaryBorrowed<'a>> for GetConceptSummary {
    fn from(
        GetConceptSummaryBorrowed {
            id,
            name,
            tagline,
            description,
            inspiration_json,
            benefits_json,
            components_json,
            related_decisions_json,
            metrics_json,
            is_deleted,
            created_at,
            updated_at,
        }: GetConceptSummaryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            tagline: tagline.into(),
            description: description.into(),
            inspiration_json: inspiration_json.map(|v| v.into()),
            benefits_json: benefits_json.into(),
            components_json: components_json.into(),
            related_decisions_json: related_decisions_json.into(),
            metrics_json: metrics_json.map(|v| v.into()),
            is_deleted,
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConceptSummaryPreview {
    pub id: String,
    pub name: String,
    pub tagline: String,
    pub description_preview: String,
    pub inspiration_json: Option<String>,
    pub benefits_json: String,
    pub components_json: String,
    pub related_decisions_json: String,
    pub metrics_json: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ConceptSummaryPreviewBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub tagline: &'a str,
    pub description_preview: &'a str,
    pub inspiration_json: Option<&'a str>,
    pub benefits_json: &'a str,
    pub components_json: &'a str,
    pub related_decisions_json: &'a str,
    pub metrics_json: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ConceptSummaryPreviewBorrowed<'a>> for ConceptSummaryPreview {
    fn from(
        ConceptSummaryPreviewBorrowed {
            id,
            name,
            tagline,
            description_preview,
            inspiration_json,
            benefits_json,
            components_json,
            related_decisions_json,
            metrics_json,
            created_at,
            updated_at,
        }: ConceptSummaryPreviewBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            tagline: tagline.into(),
            description_preview: description_preview.into(),
            inspiration_json: inspiration_json.map(|v| v.into()),
            benefits_json: benefits_json.into(),
            components_json: components_json.into(),
            related_decisions_json: related_decisions_json.into(),
            metrics_json: metrics_json.map(|v| v.into()),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConceptSummarySearchRow {
    pub id: String,
    pub name: String,
    pub tagline: String,
    pub description_preview: String,
    pub inspiration_json: Option<String>,
    pub benefits_json: String,
    pub components_json: String,
    pub related_decisions_json: String,
    pub metrics_json: Option<String>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: Option<f32>,
}
pub struct ConceptSummarySearchRowBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub tagline: &'a str,
    pub description_preview: &'a str,
    pub inspiration_json: Option<&'a str>,
    pub benefits_json: &'a str,
    pub components_json: &'a str,
    pub related_decisions_json: &'a str,
    pub metrics_json: Option<&'a str>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
    pub rank: Option<f32>,
}
impl<'a> From<ConceptSummarySearchRowBorrowed<'a>> for ConceptSummarySearchRow {
    fn from(
        ConceptSummarySearchRowBorrowed {
            id,
            name,
            tagline,
            description_preview,
            inspiration_json,
            benefits_json,
            components_json,
            related_decisions_json,
            metrics_json,
            created_at,
            updated_at,
            rank,
        }: ConceptSummarySearchRowBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            tagline: tagline.into(),
            description_preview: description_preview.into(),
            inspiration_json: inspiration_json.map(|v| v.into()),
            benefits_json: benefits_json.into(),
            components_json: components_json.into(),
            related_decisions_json: related_decisions_json.into(),
            metrics_json: metrics_json.map(|v| v.into()),
            created_at,
            updated_at,
            rank,
        }
    }
}
use futures::{self, StreamExt, TryStreamExt};
use crate::client::async_::GenericClient;
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
pub struct GetDecisionQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetDecisionBorrowed, tokio_postgres::Error>,
    mapper: fn(GetDecisionBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetDecisionQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetDecisionBorrowed) -> R,
    ) -> GetDecisionQuery<'c, 'a, 's, C, R, N> {
        GetDecisionQuery {
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
pub struct DecisionPreviewQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<DecisionPreviewBorrowed, tokio_postgres::Error>,
    mapper: fn(DecisionPreviewBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> DecisionPreviewQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(DecisionPreviewBorrowed) -> R,
    ) -> DecisionPreviewQuery<'c, 'a, 's, C, R, N> {
        DecisionPreviewQuery {
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
pub struct DecisionSearchRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<DecisionSearchRowBorrowed, tokio_postgres::Error>,
    mapper: fn(DecisionSearchRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> DecisionSearchRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(DecisionSearchRowBorrowed) -> R,
    ) -> DecisionSearchRowQuery<'c, 'a, 's, C, R, N> {
        DecisionSearchRowQuery {
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
pub struct GetConceptSummaryQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetConceptSummaryBorrowed, tokio_postgres::Error>,
    mapper: fn(GetConceptSummaryBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetConceptSummaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetConceptSummaryBorrowed) -> R,
    ) -> GetConceptSummaryQuery<'c, 'a, 's, C, R, N> {
        GetConceptSummaryQuery {
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
pub struct ConceptSummaryPreviewQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<ConceptSummaryPreviewBorrowed, tokio_postgres::Error>,
    mapper: fn(ConceptSummaryPreviewBorrowed) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> ConceptSummaryPreviewQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ConceptSummaryPreviewBorrowed) -> R,
    ) -> ConceptSummaryPreviewQuery<'c, 'a, 's, C, R, N> {
        ConceptSummaryPreviewQuery {
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
pub struct ConceptSummarySearchRowQuery<
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
    ) -> Result<ConceptSummarySearchRowBorrowed, tokio_postgres::Error>,
    mapper: fn(ConceptSummarySearchRowBorrowed) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> ConceptSummarySearchRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ConceptSummarySearchRowBorrowed) -> R,
    ) -> ConceptSummarySearchRowQuery<'c, 'a, 's, C, R, N> {
        ConceptSummarySearchRowQuery {
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
pub struct SaveDecisionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_decision() -> SaveDecisionStmt {
    SaveDecisionStmt(
        "INSERT INTO decisions (id, timestamp, scale, category, status, title, summary, rationale, alternatives_json, tradeoffs_json, triggered_by, inspiration_json, related_decisions_json, affected_files_json, affected_endpoints_json, affected_tables_json, created_by, superseded_by, tags_json) VALUES ($1, NOW(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) RETURNING id",
        None,
    )
}
impl SaveDecisionStmt {
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
        T11: crate::StringSql,
        T12: crate::StringSql,
        T13: crate::StringSql,
        T14: crate::StringSql,
        T15: crate::StringSql,
        T16: crate::StringSql,
        T17: crate::StringSql,
        T18: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        scale: &'a T2,
        category: &'a T3,
        status: &'a T4,
        title: &'a T5,
        summary: &'a T6,
        rationale: &'a T7,
        alternatives_json: &'a T8,
        tradeoffs_json: &'a T9,
        triggered_by: &'a Option<T10>,
        inspiration_json: &'a Option<T11>,
        related_decisions_json: &'a T12,
        affected_files_json: &'a T13,
        affected_endpoints_json: &'a T14,
        affected_tables_json: &'a T15,
        created_by: &'a Option<T16>,
        superseded_by: &'a Option<T17>,
        tags_json: &'a T18,
    ) -> StringQuery<'c, 'a, 's, C, String, 18> {
        StringQuery {
            client,
            params: [
                id,
                scale,
                category,
                status,
                title,
                summary,
                rationale,
                alternatives_json,
                tradeoffs_json,
                triggered_by,
                inspiration_json,
                related_decisions_json,
                affected_files_json,
                affected_endpoints_json,
                affected_tables_json,
                created_by,
                superseded_by,
                tags_json,
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
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
    T14: crate::StringSql,
    T15: crate::StringSql,
    T16: crate::StringSql,
    T17: crate::StringSql,
    T18: crate::StringSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    SaveDecisionParams<
        T1,
        T2,
        T3,
        T4,
        T5,
        T6,
        T7,
        T8,
        T9,
        T10,
        T11,
        T12,
        T13,
        T14,
        T15,
        T16,
        T17,
        T18,
    >,
    StringQuery<'c, 'a, 's, C, String, 18>,
    C,
> for SaveDecisionStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SaveDecisionParams<
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
            T14,
            T15,
            T16,
            T17,
            T18,
        >,
    ) -> StringQuery<'c, 'a, 's, C, String, 18> {
        self.bind(
            client,
            &params.id,
            &params.scale,
            &params.category,
            &params.status,
            &params.title,
            &params.summary,
            &params.rationale,
            &params.alternatives_json,
            &params.tradeoffs_json,
            &params.triggered_by,
            &params.inspiration_json,
            &params.related_decisions_json,
            &params.affected_files_json,
            &params.affected_endpoints_json,
            &params.affected_tables_json,
            &params.created_by,
            &params.superseded_by,
            &params.tags_json,
        )
    }
}
pub struct GetDecisionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_decision() -> GetDecisionStmt {
    GetDecisionStmt(
        "SELECT id, timestamp, scale, category, status, title, summary, rationale, alternatives_json, tradeoffs_json, triggered_by, inspiration_json, related_decisions_json, affected_files_json, affected_endpoints_json, affected_tables_json, created_by, superseded_by, tags_json, is_deleted, created_at, updated_at FROM decisions WHERE id = $1 AND NOT is_deleted",
        None,
    )
}
impl GetDecisionStmt {
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
    ) -> GetDecisionQuery<'c, 'a, 's, C, GetDecision, 1> {
        GetDecisionQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetDecisionBorrowed, tokio_postgres::Error> {
                Ok(GetDecisionBorrowed {
                    id: row.try_get(0)?,
                    timestamp: row.try_get(1)?,
                    scale: row.try_get(2)?,
                    category: row.try_get(3)?,
                    status: row.try_get(4)?,
                    title: row.try_get(5)?,
                    summary: row.try_get(6)?,
                    rationale: row.try_get(7)?,
                    alternatives_json: row.try_get(8)?,
                    tradeoffs_json: row.try_get(9)?,
                    triggered_by: row.try_get(10)?,
                    inspiration_json: row.try_get(11)?,
                    related_decisions_json: row.try_get(12)?,
                    affected_files_json: row.try_get(13)?,
                    affected_endpoints_json: row.try_get(14)?,
                    affected_tables_json: row.try_get(15)?,
                    created_by: row.try_get(16)?,
                    superseded_by: row.try_get(17)?,
                    tags_json: row.try_get(18)?,
                    is_deleted: row.try_get(19)?,
                    created_at: row.try_get(20)?,
                    updated_at: row.try_get(21)?,
                })
            },
            mapper: |it| GetDecision::from(it),
        }
    }
}
pub struct ListDecisionsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_decisions() -> ListDecisionsStmt {
    ListDecisionsStmt(
        "SELECT id, timestamp, scale, category, status, title, LEFT(summary, 300) as summary_preview, triggered_by, tags_json, created_by, created_at, updated_at FROM decisions WHERE NOT is_deleted ORDER BY timestamp DESC LIMIT $1",
        None,
    )
}
impl ListDecisionsStmt {
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
    ) -> DecisionPreviewQuery<'c, 'a, 's, C, DecisionPreview, 1> {
        DecisionPreviewQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<DecisionPreviewBorrowed, tokio_postgres::Error> {
                Ok(DecisionPreviewBorrowed {
                    id: row.try_get(0)?,
                    timestamp: row.try_get(1)?,
                    scale: row.try_get(2)?,
                    category: row.try_get(3)?,
                    status: row.try_get(4)?,
                    title: row.try_get(5)?,
                    summary_preview: row.try_get(6)?,
                    triggered_by: row.try_get(7)?,
                    tags_json: row.try_get(8)?,
                    created_by: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    updated_at: row.try_get(11)?,
                })
            },
            mapper: |it| DecisionPreview::from(it),
        }
    }
}
pub struct ListDecisionsByCategoryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_decisions_by_category() -> ListDecisionsByCategoryStmt {
    ListDecisionsByCategoryStmt(
        "SELECT id, timestamp, scale, category, status, title, LEFT(summary, 300) as summary_preview, triggered_by, tags_json, created_by, created_at, updated_at FROM decisions WHERE NOT is_deleted AND category = $1 ORDER BY timestamp DESC LIMIT $2",
        None,
    )
}
impl ListDecisionsByCategoryStmt {
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
        category: &'a T1,
        max_results: &'a i64,
    ) -> DecisionPreviewQuery<'c, 'a, 's, C, DecisionPreview, 2> {
        DecisionPreviewQuery {
            client,
            params: [category, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<DecisionPreviewBorrowed, tokio_postgres::Error> {
                Ok(DecisionPreviewBorrowed {
                    id: row.try_get(0)?,
                    timestamp: row.try_get(1)?,
                    scale: row.try_get(2)?,
                    category: row.try_get(3)?,
                    status: row.try_get(4)?,
                    title: row.try_get(5)?,
                    summary_preview: row.try_get(6)?,
                    triggered_by: row.try_get(7)?,
                    tags_json: row.try_get(8)?,
                    created_by: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    updated_at: row.try_get(11)?,
                })
            },
            mapper: |it| DecisionPreview::from(it),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    ListDecisionsByCategoryParams<T1>,
    DecisionPreviewQuery<'c, 'a, 's, C, DecisionPreview, 2>,
    C,
> for ListDecisionsByCategoryStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a ListDecisionsByCategoryParams<T1>,
    ) -> DecisionPreviewQuery<'c, 'a, 's, C, DecisionPreview, 2> {
        self.bind(client, &params.category, &params.max_results)
    }
}
pub struct ListDecisionsByScaleStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_decisions_by_scale() -> ListDecisionsByScaleStmt {
    ListDecisionsByScaleStmt(
        "SELECT id, timestamp, scale, category, status, title, LEFT(summary, 300) as summary_preview, triggered_by, tags_json, created_by, created_at, updated_at FROM decisions WHERE NOT is_deleted AND scale = $1 ORDER BY timestamp DESC LIMIT $2",
        None,
    )
}
impl ListDecisionsByScaleStmt {
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
        scale: &'a T1,
        max_results: &'a i64,
    ) -> DecisionPreviewQuery<'c, 'a, 's, C, DecisionPreview, 2> {
        DecisionPreviewQuery {
            client,
            params: [scale, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<DecisionPreviewBorrowed, tokio_postgres::Error> {
                Ok(DecisionPreviewBorrowed {
                    id: row.try_get(0)?,
                    timestamp: row.try_get(1)?,
                    scale: row.try_get(2)?,
                    category: row.try_get(3)?,
                    status: row.try_get(4)?,
                    title: row.try_get(5)?,
                    summary_preview: row.try_get(6)?,
                    triggered_by: row.try_get(7)?,
                    tags_json: row.try_get(8)?,
                    created_by: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    updated_at: row.try_get(11)?,
                })
            },
            mapper: |it| DecisionPreview::from(it),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    ListDecisionsByScaleParams<T1>,
    DecisionPreviewQuery<'c, 'a, 's, C, DecisionPreview, 2>,
    C,
> for ListDecisionsByScaleStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a ListDecisionsByScaleParams<T1>,
    ) -> DecisionPreviewQuery<'c, 'a, 's, C, DecisionPreview, 2> {
        self.bind(client, &params.scale, &params.max_results)
    }
}
pub struct SearchDecisionsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_decisions() -> SearchDecisionsStmt {
    SearchDecisionsStmt(
        "SELECT id, timestamp, scale, category, status, title, LEFT(summary, 300) as summary_preview, triggered_by, tags_json, created_by, created_at, updated_at, ts_rank(to_tsvector('english', title || ' ' || summary || ' ' || rationale), plainto_tsquery('english', $1)) as rank FROM decisions WHERE NOT is_deleted AND to_tsvector('english', title || ' ' || summary || ' ' || rationale) @@ plainto_tsquery('english', $1) ORDER BY rank DESC LIMIT $2",
        None,
    )
}
impl SearchDecisionsStmt {
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
    ) -> DecisionSearchRowQuery<'c, 'a, 's, C, DecisionSearchRow, 2> {
        DecisionSearchRowQuery {
            client,
            params: [query, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<DecisionSearchRowBorrowed, tokio_postgres::Error> {
                Ok(DecisionSearchRowBorrowed {
                    id: row.try_get(0)?,
                    timestamp: row.try_get(1)?,
                    scale: row.try_get(2)?,
                    category: row.try_get(3)?,
                    status: row.try_get(4)?,
                    title: row.try_get(5)?,
                    summary_preview: row.try_get(6)?,
                    triggered_by: row.try_get(7)?,
                    tags_json: row.try_get(8)?,
                    created_by: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    updated_at: row.try_get(11)?,
                    rank: row.try_get(12)?,
                })
            },
            mapper: |it| DecisionSearchRow::from(it),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    SearchDecisionsParams<T1>,
    DecisionSearchRowQuery<'c, 'a, 's, C, DecisionSearchRow, 2>,
    C,
> for SearchDecisionsStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SearchDecisionsParams<T1>,
    ) -> DecisionSearchRowQuery<'c, 'a, 's, C, DecisionSearchRow, 2> {
        self.bind(client, &params.query, &params.max_results)
    }
}
pub struct UpdateDecisionStatusStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_decision_status() -> UpdateDecisionStatusStmt {
    UpdateDecisionStatusStmt(
        "UPDATE decisions SET status = $1, updated_at = NOW() WHERE id = $2 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl UpdateDecisionStatusStmt {
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
    >(
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
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
    T2: crate::StringSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    UpdateDecisionStatusParams<T1, T2>,
    StringQuery<'c, 'a, 's, C, String, 2>,
    C,
> for UpdateDecisionStatusStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateDecisionStatusParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.status, &params.id)
    }
}
pub struct SupersedeDecisionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn supersede_decision() -> SupersedeDecisionStmt {
    SupersedeDecisionStmt(
        "UPDATE decisions SET status = 'superseded', superseded_by = $1, updated_at = NOW() WHERE id = $2 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl SupersedeDecisionStmt {
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
    >(
        &'s self,
        client: &'c C,
        superseded_by: &'a T1,
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [superseded_by, id],
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    SupersedeDecisionParams<T1, T2>,
    StringQuery<'c, 'a, 's, C, String, 2>,
    C,
> for SupersedeDecisionStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SupersedeDecisionParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.superseded_by, &params.id)
    }
}
pub struct UpdateDecisionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_decision() -> UpdateDecisionStmt {
    UpdateDecisionStmt(
        "UPDATE decisions SET title = COALESCE($1, title), summary = COALESCE($2, summary), rationale = COALESCE($3, rationale), alternatives_json = COALESCE($4, alternatives_json), tradeoffs_json = COALESCE($5, tradeoffs_json), tags_json = COALESCE($6, tags_json), triggered_by = COALESCE($7, triggered_by), inspiration_json = COALESCE($8, inspiration_json), related_decisions_json = COALESCE($9, related_decisions_json), affected_files_json = COALESCE($10, affected_files_json), affected_endpoints_json = COALESCE($11, affected_endpoints_json), affected_tables_json = COALESCE($12, affected_tables_json), updated_at = NOW() WHERE id = $13 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl UpdateDecisionStmt {
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
        T11: crate::StringSql,
        T12: crate::StringSql,
        T13: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        title: &'a Option<T1>,
        summary: &'a Option<T2>,
        rationale: &'a Option<T3>,
        alternatives_json: &'a Option<T4>,
        tradeoffs_json: &'a Option<T5>,
        tags_json: &'a Option<T6>,
        triggered_by: &'a Option<T7>,
        inspiration_json: &'a Option<T8>,
        related_decisions_json: &'a Option<T9>,
        affected_files_json: &'a Option<T10>,
        affected_endpoints_json: &'a Option<T11>,
        affected_tables_json: &'a Option<T12>,
        id: &'a T13,
    ) -> StringQuery<'c, 'a, 's, C, String, 13> {
        StringQuery {
            client,
            params: [
                title,
                summary,
                rationale,
                alternatives_json,
                tradeoffs_json,
                tags_json,
                triggered_by,
                inspiration_json,
                related_decisions_json,
                affected_files_json,
                affected_endpoints_json,
                affected_tables_json,
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
    T11: crate::StringSql,
    T12: crate::StringSql,
    T13: crate::StringSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    UpdateDecisionParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13>,
    StringQuery<'c, 'a, 's, C, String, 13>,
    C,
> for UpdateDecisionStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateDecisionParams<
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
        >,
    ) -> StringQuery<'c, 'a, 's, C, String, 13> {
        self.bind(
            client,
            &params.title,
            &params.summary,
            &params.rationale,
            &params.alternatives_json,
            &params.tradeoffs_json,
            &params.tags_json,
            &params.triggered_by,
            &params.inspiration_json,
            &params.related_decisions_json,
            &params.affected_files_json,
            &params.affected_endpoints_json,
            &params.affected_tables_json,
            &params.id,
        )
    }
}
pub struct SoftDeleteDecisionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn soft_delete_decision() -> SoftDeleteDecisionStmt {
    SoftDeleteDecisionStmt(
        "UPDATE decisions SET is_deleted = true, updated_at = NOW() WHERE id = $1 RETURNING id",
        None,
    )
}
impl SoftDeleteDecisionStmt {
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
pub struct SaveConceptSummaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_concept_summary() -> SaveConceptSummaryStmt {
    SaveConceptSummaryStmt(
        "INSERT INTO concept_summaries (id, name, tagline, description, inspiration_json, benefits_json, components_json, related_decisions_json, metrics_json) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        None,
    )
}
impl SaveConceptSummaryStmt {
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
        tagline: &'a T3,
        description: &'a T4,
        inspiration_json: &'a Option<T5>,
        benefits_json: &'a T6,
        components_json: &'a T7,
        related_decisions_json: &'a T8,
        metrics_json: &'a Option<T9>,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        StringQuery {
            client,
            params: [
                id,
                name,
                tagline,
                description,
                inspiration_json,
                benefits_json,
                components_json,
                related_decisions_json,
                metrics_json,
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    SaveConceptSummaryParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    StringQuery<'c, 'a, 's, C, String, 9>,
    C,
> for SaveConceptSummaryStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SaveConceptSummaryParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.tagline,
            &params.description,
            &params.inspiration_json,
            &params.benefits_json,
            &params.components_json,
            &params.related_decisions_json,
            &params.metrics_json,
        )
    }
}
pub struct GetConceptSummaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_concept_summary() -> GetConceptSummaryStmt {
    GetConceptSummaryStmt(
        "SELECT id, name, tagline, description, inspiration_json, benefits_json, components_json, related_decisions_json, metrics_json, is_deleted, created_at, updated_at FROM concept_summaries WHERE id = $1 AND NOT is_deleted",
        None,
    )
}
impl GetConceptSummaryStmt {
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
    ) -> GetConceptSummaryQuery<'c, 'a, 's, C, GetConceptSummary, 1> {
        GetConceptSummaryQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetConceptSummaryBorrowed, tokio_postgres::Error> {
                Ok(GetConceptSummaryBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    tagline: row.try_get(2)?,
                    description: row.try_get(3)?,
                    inspiration_json: row.try_get(4)?,
                    benefits_json: row.try_get(5)?,
                    components_json: row.try_get(6)?,
                    related_decisions_json: row.try_get(7)?,
                    metrics_json: row.try_get(8)?,
                    is_deleted: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                    updated_at: row.try_get(11)?,
                })
            },
            mapper: |it| GetConceptSummary::from(it),
        }
    }
}
pub struct ListConceptSummariesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_concept_summaries() -> ListConceptSummariesStmt {
    ListConceptSummariesStmt(
        "SELECT id, name, tagline, LEFT(description, 500) as description_preview, inspiration_json, benefits_json, components_json, related_decisions_json, metrics_json, created_at, updated_at FROM concept_summaries WHERE NOT is_deleted ORDER BY updated_at DESC LIMIT $1",
        None,
    )
}
impl ListConceptSummariesStmt {
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
    ) -> ConceptSummaryPreviewQuery<'c, 'a, 's, C, ConceptSummaryPreview, 1> {
        ConceptSummaryPreviewQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ConceptSummaryPreviewBorrowed, tokio_postgres::Error> {
                Ok(ConceptSummaryPreviewBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    tagline: row.try_get(2)?,
                    description_preview: row.try_get(3)?,
                    inspiration_json: row.try_get(4)?,
                    benefits_json: row.try_get(5)?,
                    components_json: row.try_get(6)?,
                    related_decisions_json: row.try_get(7)?,
                    metrics_json: row.try_get(8)?,
                    created_at: row.try_get(9)?,
                    updated_at: row.try_get(10)?,
                })
            },
            mapper: |it| ConceptSummaryPreview::from(it),
        }
    }
}
pub struct SearchConceptSummariesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_concept_summaries() -> SearchConceptSummariesStmt {
    SearchConceptSummariesStmt(
        "SELECT id, name, tagline, LEFT(description, 500) as description_preview, inspiration_json, benefits_json, components_json, related_decisions_json, metrics_json, created_at, updated_at, ts_rank(to_tsvector('english', name || ' ' || tagline || ' ' || description), plainto_tsquery('english', $1)) as rank FROM concept_summaries WHERE NOT is_deleted AND to_tsvector('english', name || ' ' || tagline || ' ' || description) @@ plainto_tsquery('english', $1) ORDER BY rank DESC LIMIT $2",
        None,
    )
}
impl SearchConceptSummariesStmt {
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
    ) -> ConceptSummarySearchRowQuery<'c, 'a, 's, C, ConceptSummarySearchRow, 2> {
        ConceptSummarySearchRowQuery {
            client,
            params: [query, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ConceptSummarySearchRowBorrowed, tokio_postgres::Error> {
                Ok(ConceptSummarySearchRowBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    tagline: row.try_get(2)?,
                    description_preview: row.try_get(3)?,
                    inspiration_json: row.try_get(4)?,
                    benefits_json: row.try_get(5)?,
                    components_json: row.try_get(6)?,
                    related_decisions_json: row.try_get(7)?,
                    metrics_json: row.try_get(8)?,
                    created_at: row.try_get(9)?,
                    updated_at: row.try_get(10)?,
                    rank: row.try_get(11)?,
                })
            },
            mapper: |it| ConceptSummarySearchRow::from(it),
        }
    }
}
impl<
    'c,
    'a,
    's,
    C: GenericClient,
    T1: crate::StringSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    SearchConceptSummariesParams<T1>,
    ConceptSummarySearchRowQuery<'c, 'a, 's, C, ConceptSummarySearchRow, 2>,
    C,
> for SearchConceptSummariesStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SearchConceptSummariesParams<T1>,
    ) -> ConceptSummarySearchRowQuery<'c, 'a, 's, C, ConceptSummarySearchRow, 2> {
        self.bind(client, &params.query, &params.max_results)
    }
}
pub struct UpdateConceptSummaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_concept_summary() -> UpdateConceptSummaryStmt {
    UpdateConceptSummaryStmt(
        "UPDATE concept_summaries SET name = COALESCE($1, name), tagline = COALESCE($2, tagline), description = COALESCE($3, description), inspiration_json = COALESCE($4, inspiration_json), benefits_json = COALESCE($5, benefits_json), components_json = COALESCE($6, components_json), related_decisions_json = COALESCE($7, related_decisions_json), metrics_json = COALESCE($8, metrics_json), updated_at = NOW() WHERE id = $9 AND NOT is_deleted RETURNING id",
        None,
    )
}
impl UpdateConceptSummaryStmt {
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
        tagline: &'a Option<T2>,
        description: &'a Option<T3>,
        inspiration_json: &'a Option<T4>,
        benefits_json: &'a Option<T5>,
        components_json: &'a Option<T6>,
        related_decisions_json: &'a Option<T7>,
        metrics_json: &'a Option<T8>,
        id: &'a T9,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        StringQuery {
            client,
            params: [
                name,
                tagline,
                description,
                inspiration_json,
                benefits_json,
                components_json,
                related_decisions_json,
                metrics_json,
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    UpdateConceptSummaryParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    StringQuery<'c, 'a, 's, C, String, 9>,
    C,
> for UpdateConceptSummaryStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateConceptSummaryParams<T1, T2, T3, T4, T5, T6, T7, T8, T9>,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        self.bind(
            client,
            &params.name,
            &params.tagline,
            &params.description,
            &params.inspiration_json,
            &params.benefits_json,
            &params.components_json,
            &params.related_decisions_json,
            &params.metrics_json,
            &params.id,
        )
    }
}
pub struct SoftDeleteConceptSummaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn soft_delete_concept_summary() -> SoftDeleteConceptSummaryStmt {
    SoftDeleteConceptSummaryStmt(
        "UPDATE concept_summaries SET is_deleted = true, updated_at = NOW() WHERE id = $1 RETURNING id",
        None,
    )
}
impl SoftDeleteConceptSummaryStmt {
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
