// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreatePhaseTokenUsageParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
> {
    pub task_run_id: T1,
    pub phase: T2,
    pub stage_index: i32,
    pub iteration: i32,
    pub model_used: T3,
    pub provider_used: T4,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_cents: i64,
    pub duration_ms: i64,
}
#[derive(Debug)]
pub struct GetIterationTokenTotalsParams<T1: crate::StringSql> {
    pub task_run_id: T1,
    pub iteration: i32,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PhaseTokenUsageRow {
    pub phase: String,
    pub stage_index: Option<i32>,
    pub iteration: Option<i32>,
    pub model_used: Option<String>,
    pub provider_used: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_cents: i64,
    pub duration_ms: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct PhaseTokenUsageRowBorrowed<'a> {
    pub phase: &'a str,
    pub stage_index: Option<i32>,
    pub iteration: Option<i32>,
    pub model_used: Option<&'a str>,
    pub provider_used: Option<&'a str>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_cents: i64,
    pub duration_ms: Option<i64>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<PhaseTokenUsageRowBorrowed<'a>> for PhaseTokenUsageRow {
    fn from(
        PhaseTokenUsageRowBorrowed {
            phase,
            stage_index,
            iteration,
            model_used,
            provider_used,
            input_tokens,
            output_tokens,
            cost_cents,
            duration_ms,
            created_at,
        }: PhaseTokenUsageRowBorrowed<'a>,
    ) -> Self {
        Self {
            phase: phase.into(),
            stage_index,
            iteration,
            model_used: model_used.map(|v| v.into()),
            provider_used: provider_used.map(|v| v.into()),
            input_tokens,
            output_tokens,
            cost_cents,
            duration_ms,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct GetIterationTokenTotals {
    pub total_input: rust_decimal::Decimal,
    pub total_output: rust_decimal::Decimal,
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct PhaseTokenUsageRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor:
        fn(&tokio_postgres::Row) -> Result<PhaseTokenUsageRowBorrowed, tokio_postgres::Error>,
    mapper: fn(PhaseTokenUsageRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> PhaseTokenUsageRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(PhaseTokenUsageRowBorrowed) -> R,
    ) -> PhaseTokenUsageRowQuery<'c, 'a, 's, C, R, N> {
        PhaseTokenUsageRowQuery {
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
pub struct GetIterationTokenTotalsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetIterationTokenTotals, tokio_postgres::Error>,
    mapper: fn(GetIterationTokenTotals) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetIterationTokenTotalsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetIterationTokenTotals) -> R,
    ) -> GetIterationTokenTotalsQuery<'c, 'a, 's, C, R, N> {
        GetIterationTokenTotalsQuery {
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
pub struct CreatePhaseTokenUsageStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_phase_token_usage() -> CreatePhaseTokenUsageStmt {
    CreatePhaseTokenUsageStmt(
        "INSERT INTO phase_token_usage (task_run_id, phase, stage_index, iteration, model_used, provider_used, input_tokens, output_tokens, cost_cents, duration_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        None,
    )
}
impl CreatePhaseTokenUsageStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<
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
        task_run_id: &'a T1,
        phase: &'a T2,
        stage_index: &'a i32,
        iteration: &'a i32,
        model_used: &'a T3,
        provider_used: &'a T4,
        input_tokens: &'a i64,
        output_tokens: &'a i64,
        cost_cents: &'a i64,
        duration_ms: &'a i64,
    ) -> Result<u64, tokio_postgres::Error> {
        client
            .execute(
                self.0,
                &[
                    task_run_id,
                    phase,
                    stage_index,
                    iteration,
                    model_used,
                    provider_used,
                    input_tokens,
                    output_tokens,
                    cost_cents,
                    duration_ms,
                ],
            )
            .await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
>
    crate::client::async_::Params<
        'a,
        'a,
        'a,
        CreatePhaseTokenUsageParams<T1, T2, T3, T4>,
        std::pin::Pin<
            Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
        >,
        C,
    > for CreatePhaseTokenUsageStmt
{
    fn params(
        &'a self,
        client: &'a C,
        params: &'a CreatePhaseTokenUsageParams<T1, T2, T3, T4>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(
            client,
            &params.task_run_id,
            &params.phase,
            &params.stage_index,
            &params.iteration,
            &params.model_used,
            &params.provider_used,
            &params.input_tokens,
            &params.output_tokens,
            &params.cost_cents,
            &params.duration_ms,
        ))
    }
}
pub struct GetPhaseTokenUsageStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_phase_token_usage() -> GetPhaseTokenUsageStmt {
    GetPhaseTokenUsageStmt(
        "SELECT phase, stage_index, iteration, model_used, provider_used, input_tokens, output_tokens, cost_cents, duration_ms, created_at FROM phase_token_usage WHERE task_run_id = $1 ORDER BY created_at ASC",
        None,
    )
}
impl GetPhaseTokenUsageStmt {
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
    ) -> PhaseTokenUsageRowQuery<'c, 'a, 's, C, PhaseTokenUsageRow, 1> {
        PhaseTokenUsageRowQuery {
            client,
            params: [task_run_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<PhaseTokenUsageRowBorrowed, tokio_postgres::Error> {
                Ok(PhaseTokenUsageRowBorrowed {
                    phase: row.try_get(0)?,
                    stage_index: row.try_get(1)?,
                    iteration: row.try_get(2)?,
                    model_used: row.try_get(3)?,
                    provider_used: row.try_get(4)?,
                    input_tokens: row.try_get(5)?,
                    output_tokens: row.try_get(6)?,
                    cost_cents: row.try_get(7)?,
                    duration_ms: row.try_get(8)?,
                    created_at: row.try_get(9)?,
                })
            },
            mapper: |it| PhaseTokenUsageRow::from(it),
        }
    }
}
pub struct GetIterationTokenTotalsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_iteration_token_totals() -> GetIterationTokenTotalsStmt {
    GetIterationTokenTotalsStmt(
        "SELECT COALESCE(SUM(input_tokens), 0) as total_input, COALESCE(SUM(output_tokens), 0) as total_output FROM phase_token_usage WHERE task_run_id = $1 AND iteration = $2",
        None,
    )
}
impl GetIterationTokenTotalsStmt {
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
        iteration: &'a i32,
    ) -> GetIterationTokenTotalsQuery<'c, 'a, 's, C, GetIterationTokenTotals, 2> {
        GetIterationTokenTotalsQuery {
            client,
            params: [task_run_id, iteration],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetIterationTokenTotals, tokio_postgres::Error> {
                Ok(GetIterationTokenTotals {
                    total_input: row.try_get(0)?,
                    total_output: row.try_get(1)?,
                })
            },
            mapper: |it| GetIterationTokenTotals::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetIterationTokenTotalsParams<T1>,
        GetIterationTokenTotalsQuery<'c, 'a, 's, C, GetIterationTokenTotals, 2>,
        C,
    > for GetIterationTokenTotalsStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetIterationTokenTotalsParams<T1>,
    ) -> GetIterationTokenTotalsQuery<'c, 'a, 's, C, GetIterationTokenTotals, 2> {
        self.bind(client, &params.task_run_id, &params.iteration)
    }
}
pub struct UpdateTaskRunTokenTotalsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn update_task_run_token_totals() -> UpdateTaskRunTokenTotalsStmt {
    UpdateTaskRunTokenTotalsStmt(
        "UPDATE task_runs SET total_input_tokens = COALESCE((SELECT SUM(input_tokens) FROM phase_token_usage WHERE task_run_id = $1), 0), total_output_tokens = COALESCE((SELECT SUM(output_tokens) FROM phase_token_usage WHERE task_run_id = $1), 0), total_cost_cents = COALESCE((SELECT SUM(cost_cents) FROM phase_token_usage WHERE task_run_id = $1), 0) WHERE id = $1",
        None,
    )
}
impl UpdateTaskRunTokenTotalsStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>(
        &'s self,
        client: &'c C,
        task_run_id: &'a T1,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[task_run_id]).await
    }
}
