// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct InsertDeferredQuestionParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
    T6: crate::StringSql,
    T7: crate::StringSql,
    T8: crate::StringSql,
> {
    pub id: T1,
    pub task_run_id: T2,
    pub iteration: i32,
    pub question: T3,
    pub context_json: T4,
    pub auto_decision_type: T5,
    pub auto_decision_detail: Option<T6>,
    pub confidence: f64,
    pub risk_level: T7,
    pub git_checkpoint: Option<T8>,
}
#[derive(Debug)]
pub struct ReviewDeferredQuestionParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
> {
    pub status: T1,
    pub reviewer_comment: Option<T2>,
    pub id: T3,
}
#[derive(Debug)]
pub struct UpdateContingentIterationsParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub contingent_iterations: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct GetDeferredQuestionsByStatusParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub task_run_id: T1,
    pub status: T2,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetDeferredQuestionsForTaskRun {
    pub id: String,
    pub task_run_id: String,
    pub iteration: i32,
    pub question: String,
    pub context_json: String,
    pub auto_decision_type: String,
    pub auto_decision_detail: String,
    pub confidence: f64,
    pub risk_level: String,
    pub status: String,
    pub git_checkpoint: String,
    pub contingent_iterations: String,
    pub reviewer_comment: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub reviewed_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetDeferredQuestionsForTaskRunBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub iteration: i32,
    pub question: &'a str,
    pub context_json: &'a str,
    pub auto_decision_type: &'a str,
    pub auto_decision_detail: &'a str,
    pub confidence: f64,
    pub risk_level: &'a str,
    pub status: &'a str,
    pub git_checkpoint: &'a str,
    pub contingent_iterations: &'a str,
    pub reviewer_comment: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub reviewed_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetDeferredQuestionsForTaskRunBorrowed<'a>> for GetDeferredQuestionsForTaskRun {
    fn from(
        GetDeferredQuestionsForTaskRunBorrowed {
            id,
            task_run_id,
            iteration,
            question,
            context_json,
            auto_decision_type,
            auto_decision_detail,
            confidence,
            risk_level,
            status,
            git_checkpoint,
            contingent_iterations,
            reviewer_comment,
            created_at,
            reviewed_at,
        }: GetDeferredQuestionsForTaskRunBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            iteration,
            question: question.into(),
            context_json: context_json.into(),
            auto_decision_type: auto_decision_type.into(),
            auto_decision_detail: auto_decision_detail.into(),
            confidence,
            risk_level: risk_level.into(),
            status: status.into(),
            git_checkpoint: git_checkpoint.into(),
            contingent_iterations: contingent_iterations.into(),
            reviewer_comment: reviewer_comment.into(),
            created_at,
            reviewed_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetDeferredQuestionsByStatus {
    pub id: String,
    pub task_run_id: String,
    pub iteration: i32,
    pub question: String,
    pub context_json: String,
    pub auto_decision_type: String,
    pub auto_decision_detail: String,
    pub confidence: f64,
    pub risk_level: String,
    pub status: String,
    pub git_checkpoint: String,
    pub contingent_iterations: String,
    pub reviewer_comment: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub reviewed_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetDeferredQuestionsByStatusBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub iteration: i32,
    pub question: &'a str,
    pub context_json: &'a str,
    pub auto_decision_type: &'a str,
    pub auto_decision_detail: &'a str,
    pub confidence: f64,
    pub risk_level: &'a str,
    pub status: &'a str,
    pub git_checkpoint: &'a str,
    pub contingent_iterations: &'a str,
    pub reviewer_comment: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub reviewed_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetDeferredQuestionsByStatusBorrowed<'a>> for GetDeferredQuestionsByStatus {
    fn from(
        GetDeferredQuestionsByStatusBorrowed {
            id,
            task_run_id,
            iteration,
            question,
            context_json,
            auto_decision_type,
            auto_decision_detail,
            confidence,
            risk_level,
            status,
            git_checkpoint,
            contingent_iterations,
            reviewer_comment,
            created_at,
            reviewed_at,
        }: GetDeferredQuestionsByStatusBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            iteration,
            question: question.into(),
            context_json: context_json.into(),
            auto_decision_type: auto_decision_type.into(),
            auto_decision_detail: auto_decision_detail.into(),
            confidence,
            risk_level: risk_level.into(),
            status: status.into(),
            git_checkpoint: git_checkpoint.into(),
            contingent_iterations: contingent_iterations.into(),
            reviewer_comment: reviewer_comment.into(),
            created_at,
            reviewed_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetDeferredQuestionById {
    pub id: String,
    pub task_run_id: String,
    pub iteration: i32,
    pub question: String,
    pub context_json: String,
    pub auto_decision_type: String,
    pub auto_decision_detail: String,
    pub confidence: f64,
    pub risk_level: String,
    pub status: String,
    pub git_checkpoint: String,
    pub contingent_iterations: String,
    pub reviewer_comment: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub reviewed_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetDeferredQuestionByIdBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub iteration: i32,
    pub question: &'a str,
    pub context_json: &'a str,
    pub auto_decision_type: &'a str,
    pub auto_decision_detail: &'a str,
    pub confidence: f64,
    pub risk_level: &'a str,
    pub status: &'a str,
    pub git_checkpoint: &'a str,
    pub contingent_iterations: &'a str,
    pub reviewer_comment: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub reviewed_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetDeferredQuestionByIdBorrowed<'a>> for GetDeferredQuestionById {
    fn from(
        GetDeferredQuestionByIdBorrowed {
            id,
            task_run_id,
            iteration,
            question,
            context_json,
            auto_decision_type,
            auto_decision_detail,
            confidence,
            risk_level,
            status,
            git_checkpoint,
            contingent_iterations,
            reviewer_comment,
            created_at,
            reviewed_at,
        }: GetDeferredQuestionByIdBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            iteration,
            question: question.into(),
            context_json: context_json.into(),
            auto_decision_type: auto_decision_type.into(),
            auto_decision_detail: auto_decision_detail.into(),
            confidence,
            risk_level: risk_level.into(),
            status: status.into(),
            git_checkpoint: git_checkpoint.into(),
            contingent_iterations: contingent_iterations.into(),
            reviewer_comment: reviewer_comment.into(),
            created_at,
            reviewed_at,
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
pub struct GetDeferredQuestionsForTaskRunQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetDeferredQuestionsForTaskRunBorrowed, tokio_postgres::Error>,
    mapper: fn(GetDeferredQuestionsForTaskRunBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetDeferredQuestionsForTaskRunQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetDeferredQuestionsForTaskRunBorrowed) -> R,
    ) -> GetDeferredQuestionsForTaskRunQuery<'c, 'a, 's, C, R, N> {
        GetDeferredQuestionsForTaskRunQuery {
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
pub struct GetDeferredQuestionsByStatusQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetDeferredQuestionsByStatusBorrowed, tokio_postgres::Error>,
    mapper: fn(GetDeferredQuestionsByStatusBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetDeferredQuestionsByStatusQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetDeferredQuestionsByStatusBorrowed) -> R,
    ) -> GetDeferredQuestionsByStatusQuery<'c, 'a, 's, C, R, N> {
        GetDeferredQuestionsByStatusQuery {
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
pub struct GetDeferredQuestionByIdQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<GetDeferredQuestionByIdBorrowed, tokio_postgres::Error>,
    mapper: fn(GetDeferredQuestionByIdBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetDeferredQuestionByIdQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetDeferredQuestionByIdBorrowed) -> R,
    ) -> GetDeferredQuestionByIdQuery<'c, 'a, 's, C, R, N> {
        GetDeferredQuestionByIdQuery {
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
pub struct InsertDeferredQuestionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn insert_deferred_question() -> InsertDeferredQuestionStmt {
    InsertDeferredQuestionStmt(
        "INSERT INTO deferred_questions (id, task_run_id, iteration, question, context_json, auto_decision_type, auto_decision_detail, confidence, risk_level, git_checkpoint) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        None,
    )
}
impl InsertDeferredQuestionStmt {
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
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        task_run_id: &'a T2,
        iteration: &'a i32,
        question: &'a T3,
        context_json: &'a T4,
        auto_decision_type: &'a T5,
        auto_decision_detail: &'a Option<T6>,
        confidence: &'a f64,
        risk_level: &'a T7,
        git_checkpoint: &'a Option<T8>,
    ) -> StringQuery<'c, 'a, 's, C, String, 10> {
        StringQuery {
            client,
            params: [
                id,
                task_run_id,
                iteration,
                question,
                context_json,
                auto_decision_type,
                auto_decision_detail,
                confidence,
                risk_level,
                git_checkpoint,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        InsertDeferredQuestionParams<T1, T2, T3, T4, T5, T6, T7, T8>,
        StringQuery<'c, 'a, 's, C, String, 10>,
        C,
    > for InsertDeferredQuestionStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a InsertDeferredQuestionParams<T1, T2, T3, T4, T5, T6, T7, T8>,
    ) -> StringQuery<'c, 'a, 's, C, String, 10> {
        self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.iteration,
            &params.question,
            &params.context_json,
            &params.auto_decision_type,
            &params.auto_decision_detail,
            &params.confidence,
            &params.risk_level,
            &params.git_checkpoint,
        )
    }
}
pub struct ReviewDeferredQuestionStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn review_deferred_question() -> ReviewDeferredQuestionStmt {
    ReviewDeferredQuestionStmt(
        "UPDATE deferred_questions SET status = $1, reviewer_comment = $2, reviewed_at = NOW() WHERE id = $3 RETURNING id",
        None,
    )
}
impl ReviewDeferredQuestionStmt {
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
    >(
        &'s self,
        client: &'c C,
        status: &'a T1,
        reviewer_comment: &'a Option<T2>,
        id: &'a T3,
    ) -> StringQuery<'c, 'a, 's, C, String, 3> {
        StringQuery {
            client,
            params: [status, reviewer_comment, id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql, T3: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        ReviewDeferredQuestionParams<T1, T2, T3>,
        StringQuery<'c, 'a, 's, C, String, 3>,
        C,
    > for ReviewDeferredQuestionStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a ReviewDeferredQuestionParams<T1, T2, T3>,
    ) -> StringQuery<'c, 'a, 's, C, String, 3> {
        self.bind(client, &params.status, &params.reviewer_comment, &params.id)
    }
}
pub struct UpdateContingentIterationsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_contingent_iterations() -> UpdateContingentIterationsStmt {
    UpdateContingentIterationsStmt(
        "UPDATE deferred_questions SET contingent_iterations = $1 WHERE id = $2 RETURNING id",
        None,
    )
}
impl UpdateContingentIterationsStmt {
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
        contingent_iterations: &'a T1,
        id: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [contingent_iterations, id],
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
        UpdateContingentIterationsParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for UpdateContingentIterationsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a UpdateContingentIterationsParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.contingent_iterations, &params.id)
    }
}
pub struct GetDeferredQuestionsForTaskRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_deferred_questions_for_task_run() -> GetDeferredQuestionsForTaskRunStmt {
    GetDeferredQuestionsForTaskRunStmt(
        "SELECT id, task_run_id, iteration, question, context_json, auto_decision_type, COALESCE(auto_decision_detail, '') as auto_decision_detail, confidence, risk_level, status, COALESCE(git_checkpoint, '') as git_checkpoint, contingent_iterations, COALESCE(reviewer_comment, '') as reviewer_comment, created_at, COALESCE(reviewed_at, NOW()) as reviewed_at FROM deferred_questions WHERE task_run_id = $1 ORDER BY created_at ASC",
        None,
    )
}
impl GetDeferredQuestionsForTaskRunStmt {
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
    ) -> GetDeferredQuestionsForTaskRunQuery<'c, 'a, 's, C, GetDeferredQuestionsForTaskRun, 1> {
        GetDeferredQuestionsForTaskRunQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row: &tokio_postgres::Row| -> Result<
                GetDeferredQuestionsForTaskRunBorrowed,
                tokio_postgres::Error,
            > {
                Ok(GetDeferredQuestionsForTaskRunBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    iteration: row.try_get(2)?,
                    question: row.try_get(3)?,
                    context_json: row.try_get(4)?,
                    auto_decision_type: row.try_get(5)?,
                    auto_decision_detail: row.try_get(6)?,
                    confidence: row.try_get(7)?,
                    risk_level: row.try_get(8)?,
                    status: row.try_get(9)?,
                    git_checkpoint: row.try_get(10)?,
                    contingent_iterations: row.try_get(11)?,
                    reviewer_comment: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    reviewed_at: row.try_get(14)?,
                })
            },
            mapper: |it| GetDeferredQuestionsForTaskRun::from(it),
        }
    }
}
pub struct GetDeferredQuestionsByStatusStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_deferred_questions_by_status() -> GetDeferredQuestionsByStatusStmt {
    GetDeferredQuestionsByStatusStmt(
        "SELECT id, task_run_id, iteration, question, context_json, auto_decision_type, COALESCE(auto_decision_detail, '') as auto_decision_detail, confidence, risk_level, status, COALESCE(git_checkpoint, '') as git_checkpoint, contingent_iterations, COALESCE(reviewer_comment, '') as reviewer_comment, created_at, COALESCE(reviewed_at, NOW()) as reviewed_at FROM deferred_questions WHERE task_run_id = $1 AND status = $2 ORDER BY created_at ASC",
        None,
    )
}
impl GetDeferredQuestionsByStatusStmt {
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
        task_run_id: &'a T1,
        status: &'a T2,
    ) -> GetDeferredQuestionsByStatusQuery<'c, 'a, 's, C, GetDeferredQuestionsByStatus, 2> {
        GetDeferredQuestionsByStatusQuery {
            client,
            params: [task_run_id, status],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetDeferredQuestionsByStatusBorrowed, tokio_postgres::Error> {
                Ok(GetDeferredQuestionsByStatusBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    iteration: row.try_get(2)?,
                    question: row.try_get(3)?,
                    context_json: row.try_get(4)?,
                    auto_decision_type: row.try_get(5)?,
                    auto_decision_detail: row.try_get(6)?,
                    confidence: row.try_get(7)?,
                    risk_level: row.try_get(8)?,
                    status: row.try_get(9)?,
                    git_checkpoint: row.try_get(10)?,
                    contingent_iterations: row.try_get(11)?,
                    reviewer_comment: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    reviewed_at: row.try_get(14)?,
                })
            },
            mapper: |it| GetDeferredQuestionsByStatus::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetDeferredQuestionsByStatusParams<T1, T2>,
        GetDeferredQuestionsByStatusQuery<'c, 'a, 's, C, GetDeferredQuestionsByStatus, 2>,
        C,
    > for GetDeferredQuestionsByStatusStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetDeferredQuestionsByStatusParams<T1, T2>,
    ) -> GetDeferredQuestionsByStatusQuery<'c, 'a, 's, C, GetDeferredQuestionsByStatus, 2> {
        self.bind(client, &params.task_run_id, &params.status)
    }
}
pub struct GetDeferredQuestionByIdStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_deferred_question_by_id() -> GetDeferredQuestionByIdStmt {
    GetDeferredQuestionByIdStmt(
        "SELECT id, task_run_id, iteration, question, context_json, auto_decision_type, COALESCE(auto_decision_detail, '') as auto_decision_detail, confidence, risk_level, status, COALESCE(git_checkpoint, '') as git_checkpoint, contingent_iterations, COALESCE(reviewer_comment, '') as reviewer_comment, created_at, COALESCE(reviewed_at, NOW()) as reviewed_at FROM deferred_questions WHERE id = $1",
        None,
    )
}
impl GetDeferredQuestionByIdStmt {
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
    ) -> GetDeferredQuestionByIdQuery<'c, 'a, 's, C, GetDeferredQuestionById, 1> {
        GetDeferredQuestionByIdQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetDeferredQuestionByIdBorrowed, tokio_postgres::Error> {
                Ok(GetDeferredQuestionByIdBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    iteration: row.try_get(2)?,
                    question: row.try_get(3)?,
                    context_json: row.try_get(4)?,
                    auto_decision_type: row.try_get(5)?,
                    auto_decision_detail: row.try_get(6)?,
                    confidence: row.try_get(7)?,
                    risk_level: row.try_get(8)?,
                    status: row.try_get(9)?,
                    git_checkpoint: row.try_get(10)?,
                    contingent_iterations: row.try_get(11)?,
                    reviewer_comment: row.try_get(12)?,
                    created_at: row.try_get(13)?,
                    reviewed_at: row.try_get(14)?,
                })
            },
            mapper: |it| GetDeferredQuestionById::from(it),
        }
    }
}
pub struct CountPendingDeferredQuestionsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn count_pending_deferred_questions() -> CountPendingDeferredQuestionsStmt {
    CountPendingDeferredQuestionsStmt(
        "SELECT COUNT(*) as count FROM deferred_questions WHERE task_run_id = $1 AND status = 'pending'",
        None,
    )
}
impl CountPendingDeferredQuestionsStmt {
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
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
