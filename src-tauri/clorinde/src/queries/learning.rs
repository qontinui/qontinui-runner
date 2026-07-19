// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct RecordLearningOutcomeParams<
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
    pub id: T1,
    pub task_id: T2,
    pub status: T3,
    pub duration_secs: Option<f64>,
    pub iterations: Option<i32>,
    pub strategy: Option<T4>,
    pub tools_used: Option<T5>,
    pub files_modified: Option<T6>,
    pub error_type: Option<T7>,
    pub error_message: Option<T8>,
    pub feedback: Option<T9>,
    pub workflow_architecture: Option<T10>,
    pub step_count: Option<i64>,
    pub verification_step_count: Option<i64>,
    pub agentic_step_count: Option<i64>,
    pub has_ui_bridge: bool,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub technology_tags: Option<T11>,
    pub domain_tags: Option<T12>,
    pub complexity_tier: Option<T13>,
}
#[derive(Debug)]
pub struct SaveLearningPatternParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
> {
    pub id: T1,
    pub pattern_type: T2,
    pub description: T3,
    pub confidence: f64,
    pub occurrences: i32,
    pub context: Option<T4>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetLearningOutcomes {
    pub id: String,
    pub task_id: String,
    pub status: String,
    pub duration_secs: f64,
    pub iterations: i32,
    pub strategy: String,
    pub tools_used: String,
    pub files_modified: String,
    pub error_type: String,
    pub error_message: String,
    pub feedback: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub workflow_architecture: String,
    pub step_count: i64,
    pub verification_step_count: i64,
    pub agentic_step_count: i64,
    pub has_ui_bridge: bool,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    pub composite_agentic_score: f64,
    pub technology_tags: String,
    pub domain_tags: String,
    pub complexity_tier: String,
}
pub struct GetLearningOutcomesBorrowed<'a> {
    pub id: &'a str,
    pub task_id: &'a str,
    pub status: &'a str,
    pub duration_secs: f64,
    pub iterations: i32,
    pub strategy: &'a str,
    pub tools_used: &'a str,
    pub files_modified: &'a str,
    pub error_type: &'a str,
    pub error_message: &'a str,
    pub feedback: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub workflow_architecture: &'a str,
    pub step_count: i64,
    pub verification_step_count: i64,
    pub agentic_step_count: i64,
    pub has_ui_bridge: bool,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    pub composite_agentic_score: f64,
    pub technology_tags: &'a str,
    pub domain_tags: &'a str,
    pub complexity_tier: &'a str,
}
impl<'a> From<GetLearningOutcomesBorrowed<'a>> for GetLearningOutcomes {
    fn from(
        GetLearningOutcomesBorrowed {
            id,
            task_id,
            status,
            duration_secs,
            iterations,
            strategy,
            tools_used,
            files_modified,
            error_type,
            error_message,
            feedback,
            created_at,
            workflow_architecture,
            step_count,
            verification_step_count,
            agentic_step_count,
            has_ui_bridge,
            total_tokens,
            total_cost_usd,
            composite_agentic_score,
            technology_tags,
            domain_tags,
            complexity_tier,
        }: GetLearningOutcomesBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_id: task_id.into(),
            status: status.into(),
            duration_secs,
            iterations,
            strategy: strategy.into(),
            tools_used: tools_used.into(),
            files_modified: files_modified.into(),
            error_type: error_type.into(),
            error_message: error_message.into(),
            feedback: feedback.into(),
            created_at,
            workflow_architecture: workflow_architecture.into(),
            step_count,
            verification_step_count,
            agentic_step_count,
            has_ui_bridge,
            total_tokens,
            total_cost_usd,
            composite_agentic_score,
            technology_tags: technology_tags.into(),
            domain_tags: domain_tags.into(),
            complexity_tier: complexity_tier.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetLearningPatterns {
    pub id: String,
    pub pattern_type: String,
    pub description: String,
    pub confidence: f64,
    pub occurrences: i32,
    pub context: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetLearningPatternsBorrowed<'a> {
    pub id: &'a str,
    pub pattern_type: &'a str,
    pub description: &'a str,
    pub confidence: f64,
    pub occurrences: i32,
    pub context: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetLearningPatternsBorrowed<'a>> for GetLearningPatterns {
    fn from(
        GetLearningPatternsBorrowed {
            id,
            pattern_type,
            description,
            confidence,
            occurrences,
            context,
            created_at,
            updated_at,
        }: GetLearningPatternsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            pattern_type: pattern_type.into(),
            description: description.into(),
            confidence,
            occurrences,
            context: context.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct GetLearningStatsSummary {
    pub total: i64,
    pub successes: i64,
    pub failures: i64,
    pub partials: i64,
    pub avg_duration: f64,
    pub avg_iterations: f64,
    pub avg_cost: f64,
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
pub struct GetLearningOutcomesQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetLearningOutcomesBorrowed, tokio_postgres::Error>,
    mapper: fn(GetLearningOutcomesBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetLearningOutcomesQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetLearningOutcomesBorrowed) -> R,
    ) -> GetLearningOutcomesQuery<'c, 'a, 's, C, R, N> {
        GetLearningOutcomesQuery {
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
pub struct GetLearningPatternsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetLearningPatternsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetLearningPatternsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetLearningPatternsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetLearningPatternsBorrowed) -> R,
    ) -> GetLearningPatternsQuery<'c, 'a, 's, C, R, N> {
        GetLearningPatternsQuery {
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
pub struct GetLearningStatsSummaryQuery<
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
    ) -> Result<GetLearningStatsSummary, tokio_postgres::Error>,
    mapper: fn(GetLearningStatsSummary) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> GetLearningStatsSummaryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetLearningStatsSummary) -> R,
    ) -> GetLearningStatsSummaryQuery<'c, 'a, 's, C, R, N> {
        GetLearningStatsSummaryQuery {
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
pub struct RecordLearningOutcomeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn record_learning_outcome() -> RecordLearningOutcomeStmt {
    RecordLearningOutcomeStmt(
        "INSERT INTO learning_outcomes ( id, task_id, status, duration_secs, iterations, strategy, tools_used, files_modified, error_type, error_message, feedback, workflow_architecture, step_count, verification_step_count, agentic_step_count, has_ui_bridge, total_tokens, total_cost_usd, technology_tags, domain_tags, complexity_tier ) VALUES ( $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21 ) RETURNING id",
        None,
    )
}
impl RecordLearningOutcomeStmt {
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
        id: &'a T1,
        task_id: &'a T2,
        status: &'a T3,
        duration_secs: &'a Option<f64>,
        iterations: &'a Option<i32>,
        strategy: &'a Option<T4>,
        tools_used: &'a Option<T5>,
        files_modified: &'a Option<T6>,
        error_type: &'a Option<T7>,
        error_message: &'a Option<T8>,
        feedback: &'a Option<T9>,
        workflow_architecture: &'a Option<T10>,
        step_count: &'a Option<i64>,
        verification_step_count: &'a Option<i64>,
        agentic_step_count: &'a Option<i64>,
        has_ui_bridge: &'a bool,
        total_tokens: &'a Option<i64>,
        total_cost_usd: &'a Option<f64>,
        technology_tags: &'a Option<T11>,
        domain_tags: &'a Option<T12>,
        complexity_tier: &'a Option<T13>,
    ) -> StringQuery<'c, 'a, 's, C, String, 21> {
        StringQuery {
            client,
            params: [
                id,
                task_id,
                status,
                duration_secs,
                iterations,
                strategy,
                tools_used,
                files_modified,
                error_type,
                error_message,
                feedback,
                workflow_architecture,
                step_count,
                verification_step_count,
                agentic_step_count,
                has_ui_bridge,
                total_tokens,
                total_cost_usd,
                technology_tags,
                domain_tags,
                complexity_tier,
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
    RecordLearningOutcomeParams<T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13>,
    StringQuery<'c, 'a, 's, C, String, 21>,
    C,
> for RecordLearningOutcomeStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a RecordLearningOutcomeParams<
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
    ) -> StringQuery<'c, 'a, 's, C, String, 21> {
        self.bind(
            client,
            &params.id,
            &params.task_id,
            &params.status,
            &params.duration_secs,
            &params.iterations,
            &params.strategy,
            &params.tools_used,
            &params.files_modified,
            &params.error_type,
            &params.error_message,
            &params.feedback,
            &params.workflow_architecture,
            &params.step_count,
            &params.verification_step_count,
            &params.agentic_step_count,
            &params.has_ui_bridge,
            &params.total_tokens,
            &params.total_cost_usd,
            &params.technology_tags,
            &params.domain_tags,
            &params.complexity_tier,
        )
    }
}
pub struct GetLearningOutcomesStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_learning_outcomes() -> GetLearningOutcomesStmt {
    GetLearningOutcomesStmt(
        "SELECT id, task_id, status, COALESCE(duration_secs, 0) as duration_secs, COALESCE(iterations, 0) as iterations, COALESCE(strategy, '') as strategy, COALESCE(tools_used, '') as tools_used, COALESCE(files_modified, '') as files_modified, COALESCE(error_type, '') as error_type, COALESCE(error_message, '') as error_message, COALESCE(feedback, '') as feedback, created_at, COALESCE(workflow_architecture, '') as workflow_architecture, COALESCE(step_count, 0) as step_count, COALESCE(verification_step_count, 0) as verification_step_count, COALESCE(agentic_step_count, 0) as agentic_step_count, COALESCE(has_ui_bridge, false) as has_ui_bridge, COALESCE(total_tokens, 0) as total_tokens, COALESCE(total_cost_usd, 0) as total_cost_usd, COALESCE(composite_agentic_score, 0) as composite_agentic_score, COALESCE(technology_tags, '') as technology_tags, COALESCE(domain_tags, '') as domain_tags, COALESCE(complexity_tier, '') as complexity_tier FROM learning_outcomes ORDER BY created_at DESC LIMIT $1",
        None,
    )
}
impl GetLearningOutcomesStmt {
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
    ) -> GetLearningOutcomesQuery<'c, 'a, 's, C, GetLearningOutcomes, 1> {
        GetLearningOutcomesQuery {
            client,
            params: [max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetLearningOutcomesBorrowed, tokio_postgres::Error> {
                Ok(GetLearningOutcomesBorrowed {
                    id: row.try_get(0)?,
                    task_id: row.try_get(1)?,
                    status: row.try_get(2)?,
                    duration_secs: row.try_get(3)?,
                    iterations: row.try_get(4)?,
                    strategy: row.try_get(5)?,
                    tools_used: row.try_get(6)?,
                    files_modified: row.try_get(7)?,
                    error_type: row.try_get(8)?,
                    error_message: row.try_get(9)?,
                    feedback: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                    workflow_architecture: row.try_get(12)?,
                    step_count: row.try_get(13)?,
                    verification_step_count: row.try_get(14)?,
                    agentic_step_count: row.try_get(15)?,
                    has_ui_bridge: row.try_get(16)?,
                    total_tokens: row.try_get(17)?,
                    total_cost_usd: row.try_get(18)?,
                    composite_agentic_score: row.try_get(19)?,
                    technology_tags: row.try_get(20)?,
                    domain_tags: row.try_get(21)?,
                    complexity_tier: row.try_get(22)?,
                })
            },
            mapper: |it| GetLearningOutcomes::from(it),
        }
    }
}
pub struct SaveLearningPatternStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_learning_pattern() -> SaveLearningPatternStmt {
    SaveLearningPatternStmt(
        "INSERT INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT(id) DO UPDATE SET description = EXCLUDED.description, confidence = EXCLUDED.confidence, occurrences = EXCLUDED.occurrences, context = EXCLUDED.context, updated_at = NOW() RETURNING id",
        None,
    )
}
impl SaveLearningPatternStmt {
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
        pattern_type: &'a T2,
        description: &'a T3,
        confidence: &'a f64,
        occurrences: &'a i32,
        context: &'a Option<T4>,
    ) -> StringQuery<'c, 'a, 's, C, String, 6> {
        StringQuery {
            client,
            params: [id, pattern_type, description, confidence, occurrences, context],
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    SaveLearningPatternParams<T1, T2, T3, T4>,
    StringQuery<'c, 'a, 's, C, String, 6>,
    C,
> for SaveLearningPatternStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SaveLearningPatternParams<T1, T2, T3, T4>,
    ) -> StringQuery<'c, 'a, 's, C, String, 6> {
        self.bind(
            client,
            &params.id,
            &params.pattern_type,
            &params.description,
            &params.confidence,
            &params.occurrences,
            &params.context,
        )
    }
}
pub struct GetLearningPatternsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_learning_patterns() -> GetLearningPatternsStmt {
    GetLearningPatternsStmt(
        "SELECT id, pattern_type, description, confidence, occurrences, COALESCE(context, '') as context, created_at, updated_at FROM learning_patterns ORDER BY confidence DESC",
        None,
    )
}
impl GetLearningPatternsStmt {
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
    ) -> GetLearningPatternsQuery<'c, 'a, 's, C, GetLearningPatterns, 0> {
        GetLearningPatternsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetLearningPatternsBorrowed, tokio_postgres::Error> {
                Ok(GetLearningPatternsBorrowed {
                    id: row.try_get(0)?,
                    pattern_type: row.try_get(1)?,
                    description: row.try_get(2)?,
                    confidence: row.try_get(3)?,
                    occurrences: row.try_get(4)?,
                    context: row.try_get(5)?,
                    created_at: row.try_get(6)?,
                    updated_at: row.try_get(7)?,
                })
            },
            mapper: |it| GetLearningPatterns::from(it),
        }
    }
}
pub struct GetLearningOutcomesCountStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_learning_outcomes_count() -> GetLearningOutcomesCountStmt {
    GetLearningOutcomesCountStmt(
        "SELECT COUNT(*)::bigint as count FROM learning_outcomes",
        None,
    )
}
impl GetLearningOutcomesCountStmt {
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
    ) -> I64Query<'c, 'a, 's, C, i64, 0> {
        I64Query {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
pub struct GetLearningStatsSummaryStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_learning_stats_summary() -> GetLearningStatsSummaryStmt {
    GetLearningStatsSummaryStmt(
        "SELECT COUNT(*)::bigint as total, COUNT(*) FILTER (WHERE status = 'success')::bigint as successes, COUNT(*) FILTER (WHERE status = 'failure')::bigint as failures, COUNT(*) FILTER (WHERE status = 'partial')::bigint as partials, COALESCE(AVG(duration_secs), 0)::double precision as avg_duration, COALESCE(AVG(iterations)::double precision, 0)::double precision as avg_iterations, COALESCE(AVG(total_cost_usd), 0)::double precision as avg_cost FROM learning_outcomes",
        None,
    )
}
impl GetLearningStatsSummaryStmt {
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
    ) -> GetLearningStatsSummaryQuery<'c, 'a, 's, C, GetLearningStatsSummary, 0> {
        GetLearningStatsSummaryQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetLearningStatsSummary, tokio_postgres::Error> {
                Ok(GetLearningStatsSummary {
                    total: row.try_get(0)?,
                    successes: row.try_get(1)?,
                    failures: row.try_get(2)?,
                    partials: row.try_get(3)?,
                    avg_duration: row.try_get(4)?,
                    avg_iterations: row.try_get(5)?,
                    avg_cost: row.try_get(6)?,
                })
            },
            mapper: |it| GetLearningStatsSummary::from(it),
        }
    }
}
