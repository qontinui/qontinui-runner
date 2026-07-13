// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct UpdateRecommendationStatusParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub status: T1,
    pub id: T2,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetRecommendation {
    pub id: String,
    pub optimizer_type: String,
    pub recommendation_type: String,
    pub target_agent: String,
    pub title: String,
    pub description: String,
    pub current_value: String,
    pub recommended_value: String,
    pub evidence: String,
    pub confidence: f64,
    pub status: String,
    pub applied_at: chrono::DateTime<chrono::FixedOffset>,
    pub outcome_after_apply: String,
    pub optimizer_run_id: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub content_hash: String,
    pub eval_result_id: String,
    pub eval_status: String,
}
pub struct GetRecommendationBorrowed<'a> {
    pub id: &'a str,
    pub optimizer_type: &'a str,
    pub recommendation_type: &'a str,
    pub target_agent: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub current_value: &'a str,
    pub recommended_value: &'a str,
    pub evidence: &'a str,
    pub confidence: f64,
    pub status: &'a str,
    pub applied_at: chrono::DateTime<chrono::FixedOffset>,
    pub outcome_after_apply: &'a str,
    pub optimizer_run_id: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub content_hash: &'a str,
    pub eval_result_id: &'a str,
    pub eval_status: &'a str,
}
impl<'a> From<GetRecommendationBorrowed<'a>> for GetRecommendation {
    fn from(
        GetRecommendationBorrowed {
            id,
            optimizer_type,
            recommendation_type,
            target_agent,
            title,
            description,
            current_value,
            recommended_value,
            evidence,
            confidence,
            status,
            applied_at,
            outcome_after_apply,
            optimizer_run_id,
            created_at,
            content_hash,
            eval_result_id,
            eval_status,
        }: GetRecommendationBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            optimizer_type: optimizer_type.into(),
            recommendation_type: recommendation_type.into(),
            target_agent: target_agent.into(),
            title: title.into(),
            description: description.into(),
            current_value: current_value.into(),
            recommended_value: recommended_value.into(),
            evidence: evidence.into(),
            confidence,
            status: status.into(),
            applied_at,
            outcome_after_apply: outcome_after_apply.into(),
            optimizer_run_id: optimizer_run_id.into(),
            created_at,
            content_hash: content_hash.into(),
            eval_result_id: eval_result_id.into(),
            eval_status: eval_status.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UpdateRecommendationStatus {
    pub id: String,
    pub optimizer_type: String,
    pub recommendation_type: String,
    pub status: String,
    pub applied_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct UpdateRecommendationStatusBorrowed<'a> {
    pub id: &'a str,
    pub optimizer_type: &'a str,
    pub recommendation_type: &'a str,
    pub status: &'a str,
    pub applied_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<UpdateRecommendationStatusBorrowed<'a>> for UpdateRecommendationStatus {
    fn from(
        UpdateRecommendationStatusBorrowed {
            id,
            optimizer_type,
            recommendation_type,
            status,
            applied_at,
        }: UpdateRecommendationStatusBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            optimizer_type: optimizer_type.into(),
            recommendation_type: recommendation_type.into(),
            status: status.into(),
            applied_at,
        }
    }
}
use futures::{self, StreamExt, TryStreamExt};
use crate::client::async_::GenericClient;
pub struct GetRecommendationQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetRecommendationBorrowed, tokio_postgres::Error>,
    mapper: fn(GetRecommendationBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetRecommendationQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetRecommendationBorrowed) -> R,
    ) -> GetRecommendationQuery<'c, 'a, 's, C, R, N> {
        GetRecommendationQuery {
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
pub struct UpdateRecommendationStatusQuery<
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
    ) -> Result<UpdateRecommendationStatusBorrowed, tokio_postgres::Error>,
    mapper: fn(UpdateRecommendationStatusBorrowed) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> UpdateRecommendationStatusQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(UpdateRecommendationStatusBorrowed) -> R,
    ) -> UpdateRecommendationStatusQuery<'c, 'a, 's, C, R, N> {
        UpdateRecommendationStatusQuery {
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
pub struct ChronoDateTimechronoFixedOffsetQuery<
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
    ) -> Result<chrono::DateTime<chrono::FixedOffset>, tokio_postgres::Error>,
    mapper: fn(chrono::DateTime<chrono::FixedOffset>) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> ChronoDateTimechronoFixedOffsetQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(chrono::DateTime<chrono::FixedOffset>) -> R,
    ) -> ChronoDateTimechronoFixedOffsetQuery<'c, 'a, 's, C, R, N> {
        ChronoDateTimechronoFixedOffsetQuery {
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
pub struct GetRecommendationStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_recommendation() -> GetRecommendationStmt {
    GetRecommendationStmt(
        "SELECT id, optimizer_type, recommendation_type, target_agent, title, description, current_value, recommended_value, evidence, confidence, status, COALESCE(applied_at, NOW()) as applied_at, COALESCE(outcome_after_apply, '') as outcome_after_apply, COALESCE(optimizer_run_id, '') as optimizer_run_id, created_at, COALESCE(content_hash, '') as content_hash, COALESCE(eval_result_id, '') as eval_result_id, COALESCE(eval_status, '') as eval_status FROM meta_optimizer_recommendations WHERE id = $1",
        None,
    )
}
impl GetRecommendationStmt {
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
    ) -> GetRecommendationQuery<'c, 'a, 's, C, GetRecommendation, 1> {
        GetRecommendationQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetRecommendationBorrowed, tokio_postgres::Error> {
                Ok(GetRecommendationBorrowed {
                    id: row.try_get(0)?,
                    optimizer_type: row.try_get(1)?,
                    recommendation_type: row.try_get(2)?,
                    target_agent: row.try_get(3)?,
                    title: row.try_get(4)?,
                    description: row.try_get(5)?,
                    current_value: row.try_get(6)?,
                    recommended_value: row.try_get(7)?,
                    evidence: row.try_get(8)?,
                    confidence: row.try_get(9)?,
                    status: row.try_get(10)?,
                    applied_at: row.try_get(11)?,
                    outcome_after_apply: row.try_get(12)?,
                    optimizer_run_id: row.try_get(13)?,
                    created_at: row.try_get(14)?,
                    content_hash: row.try_get(15)?,
                    eval_result_id: row.try_get(16)?,
                    eval_status: row.try_get(17)?,
                })
            },
            mapper: |it| GetRecommendation::from(it),
        }
    }
}
pub struct UpdateRecommendationStatusStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn update_recommendation_status() -> UpdateRecommendationStatusStmt {
    UpdateRecommendationStatusStmt(
        "UPDATE meta_optimizer_recommendations SET status = $1, applied_at = NOW() WHERE id = $2 RETURNING id, optimizer_type, recommendation_type, status, applied_at",
        None,
    )
}
impl UpdateRecommendationStatusStmt {
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
    ) -> UpdateRecommendationStatusQuery<'c, 'a, 's, C, UpdateRecommendationStatus, 2> {
        UpdateRecommendationStatusQuery {
            client,
            params: [status, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<UpdateRecommendationStatusBorrowed, tokio_postgres::Error> {
                Ok(UpdateRecommendationStatusBorrowed {
                    id: row.try_get(0)?,
                    optimizer_type: row.try_get(1)?,
                    recommendation_type: row.try_get(2)?,
                    status: row.try_get(3)?,
                    applied_at: row.try_get(4)?,
                })
            },
            mapper: |it| UpdateRecommendationStatus::from(it),
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
    UpdateRecommendationStatusParams<T1, T2>,
    UpdateRecommendationStatusQuery<'c, 'a, 's, C, UpdateRecommendationStatus, 2>,
    C,
> for UpdateRecommendationStatusStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateRecommendationStatusParams<T1, T2>,
    ) -> UpdateRecommendationStatusQuery<'c, 'a, 's, C, UpdateRecommendationStatus, 2> {
        self.bind(client, &params.status, &params.id)
    }
}
pub struct GetRecommendationAppliedAtStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn get_recommendation_applied_at() -> GetRecommendationAppliedAtStmt {
    GetRecommendationAppliedAtStmt(
        "SELECT COALESCE(applied_at, NOW()) as applied_at FROM meta_optimizer_recommendations WHERE id = $1",
        None,
    )
}
impl GetRecommendationAppliedAtStmt {
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
    ) -> ChronoDateTimechronoFixedOffsetQuery<
        'c,
        'a,
        's,
        C,
        chrono::DateTime<chrono::FixedOffset>,
        1,
    > {
        ChronoDateTimechronoFixedOffsetQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
