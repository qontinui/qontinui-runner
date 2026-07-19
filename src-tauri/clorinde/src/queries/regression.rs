// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct SaveRegressionSuiteParams<T1: crate::StringSql, T2: crate::JsonSql> {
    pub id: uuid::Uuid,
    pub ir_doc_id: T1,
    pub suite_json: T2,
}
#[derive(Debug)]
pub struct RecordRegressionRunParams<T1: crate::StringSql, T2: crate::JsonSql> {
    pub id: uuid::Uuid,
    pub suite_id: uuid::Uuid,
    pub run_id: T1,
    pub passed: i32,
    pub failed: i32,
    pub started_at: chrono::DateTime<chrono::FixedOffset>,
    pub completed_at: chrono::DateTime<chrono::FixedOffset>,
    pub run_result_json: T2,
}
#[derive(Debug)]
pub struct RecordRegressionDiagnosisParams<T1: crate::JsonSql> {
    pub id: uuid::Uuid,
    pub run_id: uuid::Uuid,
    pub diagnosis_json: T1,
}
#[derive(Clone, Copy, Debug)]
pub struct GetRecentDiagnosesForSuiteParams {
    pub suite_id: uuid::Uuid,
    pub limit: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAssertionExecutionsForSuite {
    pub case_id: String,
    pub assertion_id: String,
    pub started_at: String,
    pub status: String,
    pub assertion_kind: String,
    pub duration_ms: i32,
    pub failure_kind: String,
    pub error_message: String,
}
pub struct GetAssertionExecutionsForSuiteBorrowed<'a> {
    pub case_id: &'a str,
    pub assertion_id: &'a str,
    pub started_at: &'a str,
    pub status: &'a str,
    pub assertion_kind: &'a str,
    pub duration_ms: i32,
    pub failure_kind: &'a str,
    pub error_message: &'a str,
}
impl<'a> From<GetAssertionExecutionsForSuiteBorrowed<'a>>
for GetAssertionExecutionsForSuite {
    fn from(
        GetAssertionExecutionsForSuiteBorrowed {
            case_id,
            assertion_id,
            started_at,
            status,
            assertion_kind,
            duration_ms,
            failure_kind,
            error_message,
        }: GetAssertionExecutionsForSuiteBorrowed<'a>,
    ) -> Self {
        Self {
            case_id: case_id.into(),
            assertion_id: assertion_id.into(),
            started_at: started_at.into(),
            status: status.into(),
            assertion_kind: assertion_kind.into(),
            duration_ms,
            failure_kind: failure_kind.into(),
            error_message: error_message.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetRecentDiagnosesForSuite {
    pub id: uuid::Uuid,
    pub run_id: uuid::Uuid,
    pub diagnosis_json: serde_json::Value,
    pub created_at: String,
}
pub struct GetRecentDiagnosesForSuiteBorrowed<'a> {
    pub id: uuid::Uuid,
    pub run_id: uuid::Uuid,
    pub diagnosis_json: postgres_types::Json<&'a serde_json::value::RawValue>,
    pub created_at: &'a str,
}
impl<'a> From<GetRecentDiagnosesForSuiteBorrowed<'a>> for GetRecentDiagnosesForSuite {
    fn from(
        GetRecentDiagnosesForSuiteBorrowed {
            id,
            run_id,
            diagnosis_json,
            created_at,
        }: GetRecentDiagnosesForSuiteBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            run_id,
            diagnosis_json: serde_json::from_str(diagnosis_json.0.get()).unwrap(),
            created_at: created_at.into(),
        }
    }
}
use futures::{self, StreamExt, TryStreamExt};
use crate::client::async_::GenericClient;
pub struct UuidUuidQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<uuid::Uuid, tokio_postgres::Error>,
    mapper: fn(uuid::Uuid) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> UuidUuidQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(uuid::Uuid) -> R,
    ) -> UuidUuidQuery<'c, 'a, 's, C, R, N> {
        UuidUuidQuery {
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
pub struct GetAssertionExecutionsForSuiteQuery<
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
    ) -> Result<GetAssertionExecutionsForSuiteBorrowed, tokio_postgres::Error>,
    mapper: fn(GetAssertionExecutionsForSuiteBorrowed) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> GetAssertionExecutionsForSuiteQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetAssertionExecutionsForSuiteBorrowed) -> R,
    ) -> GetAssertionExecutionsForSuiteQuery<'c, 'a, 's, C, R, N> {
        GetAssertionExecutionsForSuiteQuery {
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
pub struct GetRecentDiagnosesForSuiteQuery<
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
    ) -> Result<GetRecentDiagnosesForSuiteBorrowed, tokio_postgres::Error>,
    mapper: fn(GetRecentDiagnosesForSuiteBorrowed) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> GetRecentDiagnosesForSuiteQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetRecentDiagnosesForSuiteBorrowed) -> R,
    ) -> GetRecentDiagnosesForSuiteQuery<'c, 'a, 's, C, R, N> {
        GetRecentDiagnosesForSuiteQuery {
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
pub struct SaveRegressionSuiteStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_regression_suite() -> SaveRegressionSuiteStmt {
    SaveRegressionSuiteStmt(
        "INSERT INTO regression_suites (id, ir_doc_id, suite_json) VALUES ($1::uuid, $2, $3::jsonb) RETURNING id",
        None,
    )
}
impl SaveRegressionSuiteStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::JsonSql>(
        &'s self,
        client: &'c C,
        id: &'a uuid::Uuid,
        ir_doc_id: &'a T1,
        suite_json: &'a T2,
    ) -> UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 3> {
        UuidUuidQuery {
            client,
            params: [id, ir_doc_id, suite_json],
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
    T2: crate::JsonSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    SaveRegressionSuiteParams<T1, T2>,
    UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 3>,
    C,
> for SaveRegressionSuiteStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SaveRegressionSuiteParams<T1, T2>,
    ) -> UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 3> {
        self.bind(client, &params.id, &params.ir_doc_id, &params.suite_json)
    }
}
pub struct RecordRegressionRunStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn record_regression_run() -> RecordRegressionRunStmt {
    RecordRegressionRunStmt(
        "INSERT INTO regression_runs ( id, suite_id, run_id, passed, failed, started_at, completed_at, run_result_json ) VALUES ( $1::uuid, $2::uuid, $3, $4, $5, $6::timestamptz, $7::timestamptz, $8::jsonb ) RETURNING id",
        None,
    )
}
impl RecordRegressionRunStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::StringSql, T2: crate::JsonSql>(
        &'s self,
        client: &'c C,
        id: &'a uuid::Uuid,
        suite_id: &'a uuid::Uuid,
        run_id: &'a T1,
        passed: &'a i32,
        failed: &'a i32,
        started_at: &'a chrono::DateTime<chrono::FixedOffset>,
        completed_at: &'a chrono::DateTime<chrono::FixedOffset>,
        run_result_json: &'a T2,
    ) -> UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 8> {
        UuidUuidQuery {
            client,
            params: [
                id,
                suite_id,
                run_id,
                passed,
                failed,
                started_at,
                completed_at,
                run_result_json,
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
    T2: crate::JsonSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    RecordRegressionRunParams<T1, T2>,
    UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 8>,
    C,
> for RecordRegressionRunStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a RecordRegressionRunParams<T1, T2>,
    ) -> UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 8> {
        self.bind(
            client,
            &params.id,
            &params.suite_id,
            &params.run_id,
            &params.passed,
            &params.failed,
            &params.started_at,
            &params.completed_at,
            &params.run_result_json,
        )
    }
}
pub struct RecordRegressionDiagnosisStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn record_regression_diagnosis() -> RecordRegressionDiagnosisStmt {
    RecordRegressionDiagnosisStmt(
        "INSERT INTO regression_diagnoses (id, run_id, diagnosis_json) VALUES ($1::uuid, $2::uuid, $3::jsonb) RETURNING id",
        None,
    )
}
impl RecordRegressionDiagnosisStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub fn bind<'c, 'a, 's, C: GenericClient, T1: crate::JsonSql>(
        &'s self,
        client: &'c C,
        id: &'a uuid::Uuid,
        run_id: &'a uuid::Uuid,
        diagnosis_json: &'a T1,
    ) -> UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 3> {
        UuidUuidQuery {
            client,
            params: [id, run_id, diagnosis_json],
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
    T1: crate::JsonSql,
> crate::client::async_::Params<
    'c,
    'a,
    's,
    RecordRegressionDiagnosisParams<T1>,
    UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 3>,
    C,
> for RecordRegressionDiagnosisStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a RecordRegressionDiagnosisParams<T1>,
    ) -> UuidUuidQuery<'c, 'a, 's, C, uuid::Uuid, 3> {
        self.bind(client, &params.id, &params.run_id, &params.diagnosis_json)
    }
}
pub struct RecordAssertionExecutionsBatchStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn record_assertion_executions_batch() -> RecordAssertionExecutionsBatchStmt {
    RecordAssertionExecutionsBatchStmt(
        "INSERT INTO regression_assertion_executions ( id, run_id, case_id, assertion_id, assertion_kind, status, started_at, duration_ms, failure_kind, failure_evidence_json, error_message ) SELECT (e->>'id')::uuid, (e->>'run_id')::uuid, e->>'case_id', e->>'assertion_id', e->>'assertion_kind', e->>'status', (e->>'started_at')::timestamptz, (e->>'duration_ms')::int, e->>'failure_kind', e->'failure_evidence_json', e->>'error_message' FROM jsonb_array_elements($1::jsonb) AS e",
        None,
    )
}
impl RecordAssertionExecutionsBatchStmt {
    pub async fn prepare<'a, C: GenericClient>(
        mut self,
        client: &'a C,
    ) -> Result<Self, tokio_postgres::Error> {
        self.1 = Some(client.prepare(self.0).await?);
        Ok(self)
    }
    pub async fn bind<'c, 'a, 's, C: GenericClient, T1: crate::JsonSql>(
        &'s self,
        client: &'c C,
        executions_json: &'a T1,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[executions_json]).await
    }
}
pub struct GetAssertionExecutionsForSuiteStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn get_assertion_executions_for_suite() -> GetAssertionExecutionsForSuiteStmt {
    GetAssertionExecutionsForSuiteStmt(
        "SELECT ae.case_id, ae.assertion_id, ae.started_at::TEXT AS started_at, ae.status, ae.assertion_kind, ae.duration_ms, ae.failure_kind, ae.error_message FROM regression_assertion_executions ae JOIN regression_runs r ON r.id = ae.run_id WHERE r.suite_id = $1::uuid ORDER BY ae.started_at ASC",
        None,
    )
}
impl GetAssertionExecutionsForSuiteStmt {
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
        suite_id: &'a uuid::Uuid,
    ) -> GetAssertionExecutionsForSuiteQuery<
        'c,
        'a,
        's,
        C,
        GetAssertionExecutionsForSuite,
        1,
    > {
        GetAssertionExecutionsForSuiteQuery {
            client,
            params: [suite_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetAssertionExecutionsForSuiteBorrowed, tokio_postgres::Error> {
                Ok(GetAssertionExecutionsForSuiteBorrowed {
                    case_id: row.try_get(0)?,
                    assertion_id: row.try_get(1)?,
                    started_at: row.try_get(2)?,
                    status: row.try_get(3)?,
                    assertion_kind: row.try_get(4)?,
                    duration_ms: row.try_get(5)?,
                    failure_kind: row.try_get(6)?,
                    error_message: row.try_get(7)?,
                })
            },
            mapper: |it| GetAssertionExecutionsForSuite::from(it),
        }
    }
}
pub struct GetRecentDiagnosesForSuiteStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn get_recent_diagnoses_for_suite() -> GetRecentDiagnosesForSuiteStmt {
    GetRecentDiagnosesForSuiteStmt(
        "SELECT d.id, d.run_id, d.diagnosis_json, d.created_at::TEXT AS created_at FROM regression_diagnoses d JOIN regression_runs r ON r.id = d.run_id WHERE r.suite_id = $1::uuid ORDER BY d.created_at DESC LIMIT $2",
        None,
    )
}
impl GetRecentDiagnosesForSuiteStmt {
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
        suite_id: &'a uuid::Uuid,
        limit: &'a i64,
    ) -> GetRecentDiagnosesForSuiteQuery<'c, 'a, 's, C, GetRecentDiagnosesForSuite, 2> {
        GetRecentDiagnosesForSuiteQuery {
            client,
            params: [suite_id, limit],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetRecentDiagnosesForSuiteBorrowed, tokio_postgres::Error> {
                Ok(GetRecentDiagnosesForSuiteBorrowed {
                    id: row.try_get(0)?,
                    run_id: row.try_get(1)?,
                    diagnosis_json: row.try_get(2)?,
                    created_at: row.try_get(3)?,
                })
            },
            mapper: |it| GetRecentDiagnosesForSuite::from(it),
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
    GetRecentDiagnosesForSuiteParams,
    GetRecentDiagnosesForSuiteQuery<'c, 'a, 's, C, GetRecentDiagnosesForSuite, 2>,
    C,
> for GetRecentDiagnosesForSuiteStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetRecentDiagnosesForSuiteParams,
    ) -> GetRecentDiagnosesForSuiteQuery<'c, 'a, 's, C, GetRecentDiagnosesForSuite, 2> {
        self.bind(client, &params.suite_id, &params.limit)
    }
}
