// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct SaveWorkflowExecutionStateParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
> {
    pub execution_id: T1,
    pub workflow_type: T2,
    pub state_name: T3,
    pub state_data: Option<T4>,
    pub phase: Option<T5>,
    pub iteration: Option<i32>,
}
#[derive(Debug)]
pub struct GetStepCheckpointsPaginatedWithCursorParams<T1: crate::StringSql> {
    pub execution_id: T1,
    pub cursor: i32,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct GetStepCheckpointsPaginatedNoCursorParams<T1: crate::StringSql> {
    pub execution_id: T1,
    pub max_results: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetWorkflowExecutionState {
    pub execution_id: String,
    pub workflow_type: String,
    pub state_name: String,
    pub state_data: String,
    pub phase: String,
    pub iteration: i32,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetWorkflowExecutionStateBorrowed<'a> {
    pub execution_id: &'a str,
    pub workflow_type: &'a str,
    pub state_name: &'a str,
    pub state_data: &'a str,
    pub phase: &'a str,
    pub iteration: i32,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetWorkflowExecutionStateBorrowed<'a>> for GetWorkflowExecutionState {
    fn from(
        GetWorkflowExecutionStateBorrowed {
            execution_id,
            workflow_type,
            state_name,
            state_data,
            phase,
            iteration,
            updated_at,
        }: GetWorkflowExecutionStateBorrowed<'a>,
    ) -> Self {
        Self {
            execution_id: execution_id.into(),
            workflow_type: workflow_type.into(),
            state_name: state_name.into(),
            state_data: state_data.into(),
            phase: phase.into(),
            iteration,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetStepCheckpointsPaginatedWithCursor {
    pub id: String,
    pub execution_id: String,
    pub workflow_type: String,
    pub phase: String,
    pub iteration: i32,
    pub step_index: i32,
    pub stage_index: i32,
    pub step_type: String,
    pub step_name: String,
    pub status: String,
    pub result_json: String,
    pub step_config_json: String,
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: i32,
    pub error: String,
}
pub struct GetStepCheckpointsPaginatedWithCursorBorrowed<'a> {
    pub id: &'a str,
    pub execution_id: &'a str,
    pub workflow_type: &'a str,
    pub phase: &'a str,
    pub iteration: i32,
    pub step_index: i32,
    pub stage_index: i32,
    pub step_type: &'a str,
    pub step_name: &'a str,
    pub status: &'a str,
    pub result_json: &'a str,
    pub step_config_json: &'a str,
    pub started_at: &'a str,
    pub completed_at: &'a str,
    pub duration_ms: i32,
    pub error: &'a str,
}
impl<'a> From<GetStepCheckpointsPaginatedWithCursorBorrowed<'a>>
    for GetStepCheckpointsPaginatedWithCursor
{
    fn from(
        GetStepCheckpointsPaginatedWithCursorBorrowed {
            id,
            execution_id,
            workflow_type,
            phase,
            iteration,
            step_index,
            stage_index,
            step_type,
            step_name,
            status,
            result_json,
            step_config_json,
            started_at,
            completed_at,
            duration_ms,
            error,
        }: GetStepCheckpointsPaginatedWithCursorBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            execution_id: execution_id.into(),
            workflow_type: workflow_type.into(),
            phase: phase.into(),
            iteration,
            step_index,
            stage_index,
            step_type: step_type.into(),
            step_name: step_name.into(),
            status: status.into(),
            result_json: result_json.into(),
            step_config_json: step_config_json.into(),
            started_at: started_at.into(),
            completed_at: completed_at.into(),
            duration_ms,
            error: error.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetStepCheckpointsPaginatedNoCursor {
    pub id: String,
    pub execution_id: String,
    pub workflow_type: String,
    pub phase: String,
    pub iteration: i32,
    pub step_index: i32,
    pub stage_index: i32,
    pub step_type: String,
    pub step_name: String,
    pub status: String,
    pub result_json: String,
    pub step_config_json: String,
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: i32,
    pub error: String,
}
pub struct GetStepCheckpointsPaginatedNoCursorBorrowed<'a> {
    pub id: &'a str,
    pub execution_id: &'a str,
    pub workflow_type: &'a str,
    pub phase: &'a str,
    pub iteration: i32,
    pub step_index: i32,
    pub stage_index: i32,
    pub step_type: &'a str,
    pub step_name: &'a str,
    pub status: &'a str,
    pub result_json: &'a str,
    pub step_config_json: &'a str,
    pub started_at: &'a str,
    pub completed_at: &'a str,
    pub duration_ms: i32,
    pub error: &'a str,
}
impl<'a> From<GetStepCheckpointsPaginatedNoCursorBorrowed<'a>>
    for GetStepCheckpointsPaginatedNoCursor
{
    fn from(
        GetStepCheckpointsPaginatedNoCursorBorrowed {
            id,
            execution_id,
            workflow_type,
            phase,
            iteration,
            step_index,
            stage_index,
            step_type,
            step_name,
            status,
            result_json,
            step_config_json,
            started_at,
            completed_at,
            duration_ms,
            error,
        }: GetStepCheckpointsPaginatedNoCursorBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            execution_id: execution_id.into(),
            workflow_type: workflow_type.into(),
            phase: phase.into(),
            iteration,
            step_index,
            stage_index,
            step_type: step_type.into(),
            step_name: step_name.into(),
            status: status.into(),
            result_json: result_json.into(),
            step_config_json: step_config_json.into(),
            started_at: started_at.into(),
            completed_at: completed_at.into(),
            duration_ms,
            error: error.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetLatestStepProgressMarker {
    pub id: i64,
    pub checkpoint_id: String,
    pub marker_type: String,
    pub current_value: i64,
    pub total_value: i64,
    pub description: String,
    pub data_json: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetLatestStepProgressMarkerBorrowed<'a> {
    pub id: i64,
    pub checkpoint_id: &'a str,
    pub marker_type: &'a str,
    pub current_value: i64,
    pub total_value: i64,
    pub description: &'a str,
    pub data_json: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetLatestStepProgressMarkerBorrowed<'a>> for GetLatestStepProgressMarker {
    fn from(
        GetLatestStepProgressMarkerBorrowed {
            id,
            checkpoint_id,
            marker_type,
            current_value,
            total_value,
            description,
            data_json,
            created_at,
        }: GetLatestStepProgressMarkerBorrowed<'a>,
    ) -> Self {
        Self {
            id,
            checkpoint_id: checkpoint_id.into(),
            marker_type: marker_type.into(),
            current_value,
            total_value,
            description: description.into(),
            data_json: data_json.into(),
            created_at,
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
pub struct GetWorkflowExecutionStateQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetWorkflowExecutionStateBorrowed, tokio_postgres::Error>,
    mapper: fn(GetWorkflowExecutionStateBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetWorkflowExecutionStateQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetWorkflowExecutionStateBorrowed) -> R,
    ) -> GetWorkflowExecutionStateQuery<'c, 'a, 's, C, R, N> {
        GetWorkflowExecutionStateQuery {
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
pub struct GetStepCheckpointsPaginatedWithCursorQuery<
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
        -> Result<GetStepCheckpointsPaginatedWithCursorBorrowed, tokio_postgres::Error>,
    mapper: fn(GetStepCheckpointsPaginatedWithCursorBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize>
    GetStepCheckpointsPaginatedWithCursorQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetStepCheckpointsPaginatedWithCursorBorrowed) -> R,
    ) -> GetStepCheckpointsPaginatedWithCursorQuery<'c, 'a, 's, C, R, N> {
        GetStepCheckpointsPaginatedWithCursorQuery {
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
pub struct GetStepCheckpointsPaginatedNoCursorQuery<'c, 'a, 's, C: GenericClient, T, const N: usize>
{
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    )
        -> Result<GetStepCheckpointsPaginatedNoCursorBorrowed, tokio_postgres::Error>,
    mapper: fn(GetStepCheckpointsPaginatedNoCursorBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize>
    GetStepCheckpointsPaginatedNoCursorQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetStepCheckpointsPaginatedNoCursorBorrowed) -> R,
    ) -> GetStepCheckpointsPaginatedNoCursorQuery<'c, 'a, 's, C, R, N> {
        GetStepCheckpointsPaginatedNoCursorQuery {
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
pub struct GetLatestStepProgressMarkerQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetLatestStepProgressMarkerBorrowed, tokio_postgres::Error>,
    mapper: fn(GetLatestStepProgressMarkerBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetLatestStepProgressMarkerQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetLatestStepProgressMarkerBorrowed) -> R,
    ) -> GetLatestStepProgressMarkerQuery<'c, 'a, 's, C, R, N> {
        GetLatestStepProgressMarkerQuery {
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
pub struct SaveWorkflowExecutionStateStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_workflow_execution_state() -> SaveWorkflowExecutionStateStmt {
    SaveWorkflowExecutionStateStmt(
        "INSERT INTO workflow_execution_state (execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at) VALUES ($1, $2, $3, $4, $5, $6, NOW()) ON CONFLICT(execution_id) DO UPDATE SET workflow_type = EXCLUDED.workflow_type, state_name = EXCLUDED.state_name, state_data = EXCLUDED.state_data, phase = EXCLUDED.phase, iteration = EXCLUDED.iteration, updated_at = NOW() RETURNING execution_id",
        None,
    )
}
impl SaveWorkflowExecutionStateStmt {
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
    >(
        &'s self,
        client: &'c C,
        execution_id: &'a T1,
        workflow_type: &'a T2,
        state_name: &'a T3,
        state_data: &'a Option<T4>,
        phase: &'a Option<T5>,
        iteration: &'a Option<i32>,
    ) -> StringQuery<'c, 'a, 's, C, String, 6> {
        StringQuery {
            client,
            params: [
                execution_id,
                workflow_type,
                state_name,
                state_data,
                phase,
                iteration,
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
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        SaveWorkflowExecutionStateParams<T1, T2, T3, T4, T5>,
        StringQuery<'c, 'a, 's, C, String, 6>,
        C,
    > for SaveWorkflowExecutionStateStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SaveWorkflowExecutionStateParams<T1, T2, T3, T4, T5>,
    ) -> StringQuery<'c, 'a, 's, C, String, 6> {
        self.bind(
            client,
            &params.execution_id,
            &params.workflow_type,
            &params.state_name,
            &params.state_data,
            &params.phase,
            &params.iteration,
        )
    }
}
pub struct GetWorkflowExecutionStateStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_workflow_execution_state() -> GetWorkflowExecutionStateStmt {
    GetWorkflowExecutionStateStmt(
        "SELECT execution_id, workflow_type, state_name, COALESCE(state_data, '') as state_data, COALESCE(phase, '') as phase, COALESCE(iteration, 0) as iteration, updated_at FROM workflow_execution_state WHERE execution_id = $1",
        None,
    )
}
impl GetWorkflowExecutionStateStmt {
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
        execution_id: &'a T1,
    ) -> GetWorkflowExecutionStateQuery<'c, 'a, 's, C, GetWorkflowExecutionState, 1> {
        GetWorkflowExecutionStateQuery {
            client,
            params: [execution_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetWorkflowExecutionStateBorrowed, tokio_postgres::Error> {
                Ok(GetWorkflowExecutionStateBorrowed {
                    execution_id: row.try_get(0)?,
                    workflow_type: row.try_get(1)?,
                    state_name: row.try_get(2)?,
                    state_data: row.try_get(3)?,
                    phase: row.try_get(4)?,
                    iteration: row.try_get(5)?,
                    updated_at: row.try_get(6)?,
                })
            },
            mapper: |it| GetWorkflowExecutionState::from(it),
        }
    }
}
pub struct DeleteWorkflowExecutionStateStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_workflow_execution_state() -> DeleteWorkflowExecutionStateStmt {
    DeleteWorkflowExecutionStateStmt(
        "DELETE FROM workflow_execution_state WHERE execution_id = $1",
        None,
    )
}
impl DeleteWorkflowExecutionStateStmt {
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
        execution_id: &'a T1,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[execution_id]).await
    }
}
pub struct GetStepCheckpointsPaginatedWithCursorStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn get_step_checkpoints_paginated_with_cursor() -> GetStepCheckpointsPaginatedWithCursorStmt {
    GetStepCheckpointsPaginatedWithCursorStmt(
        "SELECT id, execution_id, workflow_type, phase, COALESCE(iteration, 0) as iteration, step_index, COALESCE(stage_index, 0) as stage_index, step_type, COALESCE(step_name, '') as step_name, status, COALESCE(result_json, '') as result_json, COALESCE(step_config_json, '') as step_config_json, COALESCE(started_at::TEXT, '') as started_at, COALESCE(completed_at::TEXT, '') as completed_at, COALESCE(duration_ms, 0) as duration_ms, COALESCE(error, '') as error FROM workflow_step_checkpoints WHERE execution_id = $1 AND step_index > $2 ORDER BY step_index ASC LIMIT $3",
        None,
    )
}
impl GetStepCheckpointsPaginatedWithCursorStmt {
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
        execution_id: &'a T1,
        cursor: &'a i32,
        max_results: &'a i64,
    ) -> GetStepCheckpointsPaginatedWithCursorQuery<
        'c,
        'a,
        's,
        C,
        GetStepCheckpointsPaginatedWithCursor,
        3,
    > {
        GetStepCheckpointsPaginatedWithCursorQuery {
            client,
            params: [execution_id, cursor, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row: &tokio_postgres::Row| -> Result<
                GetStepCheckpointsPaginatedWithCursorBorrowed,
                tokio_postgres::Error,
            > {
                Ok(GetStepCheckpointsPaginatedWithCursorBorrowed {
                    id: row.try_get(0)?,
                    execution_id: row.try_get(1)?,
                    workflow_type: row.try_get(2)?,
                    phase: row.try_get(3)?,
                    iteration: row.try_get(4)?,
                    step_index: row.try_get(5)?,
                    stage_index: row.try_get(6)?,
                    step_type: row.try_get(7)?,
                    step_name: row.try_get(8)?,
                    status: row.try_get(9)?,
                    result_json: row.try_get(10)?,
                    step_config_json: row.try_get(11)?,
                    started_at: row.try_get(12)?,
                    completed_at: row.try_get(13)?,
                    duration_ms: row.try_get(14)?,
                    error: row.try_get(15)?,
                })
            },
            mapper: |it| GetStepCheckpointsPaginatedWithCursor::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetStepCheckpointsPaginatedWithCursorParams<T1>,
        GetStepCheckpointsPaginatedWithCursorQuery<
            'c,
            'a,
            's,
            C,
            GetStepCheckpointsPaginatedWithCursor,
            3,
        >,
        C,
    > for GetStepCheckpointsPaginatedWithCursorStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetStepCheckpointsPaginatedWithCursorParams<T1>,
    ) -> GetStepCheckpointsPaginatedWithCursorQuery<
        'c,
        'a,
        's,
        C,
        GetStepCheckpointsPaginatedWithCursor,
        3,
    > {
        self.bind(
            client,
            &params.execution_id,
            &params.cursor,
            &params.max_results,
        )
    }
}
pub struct GetStepCheckpointsPaginatedNoCursorStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_step_checkpoints_paginated_no_cursor() -> GetStepCheckpointsPaginatedNoCursorStmt {
    GetStepCheckpointsPaginatedNoCursorStmt(
        "SELECT id, execution_id, workflow_type, phase, COALESCE(iteration, 0) as iteration, step_index, COALESCE(stage_index, 0) as stage_index, step_type, COALESCE(step_name, '') as step_name, status, COALESCE(result_json, '') as result_json, COALESCE(step_config_json, '') as step_config_json, COALESCE(started_at::TEXT, '') as started_at, COALESCE(completed_at::TEXT, '') as completed_at, COALESCE(duration_ms, 0) as duration_ms, COALESCE(error, '') as error FROM workflow_step_checkpoints WHERE execution_id = $1 ORDER BY step_index ASC LIMIT $2",
        None,
    )
}
impl GetStepCheckpointsPaginatedNoCursorStmt {
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
        execution_id: &'a T1,
        max_results: &'a i64,
    ) -> GetStepCheckpointsPaginatedNoCursorQuery<
        'c,
        'a,
        's,
        C,
        GetStepCheckpointsPaginatedNoCursor,
        2,
    > {
        GetStepCheckpointsPaginatedNoCursorQuery {
            client,
            params: [execution_id, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row: &tokio_postgres::Row| -> Result<
                GetStepCheckpointsPaginatedNoCursorBorrowed,
                tokio_postgres::Error,
            > {
                Ok(GetStepCheckpointsPaginatedNoCursorBorrowed {
                    id: row.try_get(0)?,
                    execution_id: row.try_get(1)?,
                    workflow_type: row.try_get(2)?,
                    phase: row.try_get(3)?,
                    iteration: row.try_get(4)?,
                    step_index: row.try_get(5)?,
                    stage_index: row.try_get(6)?,
                    step_type: row.try_get(7)?,
                    step_name: row.try_get(8)?,
                    status: row.try_get(9)?,
                    result_json: row.try_get(10)?,
                    step_config_json: row.try_get(11)?,
                    started_at: row.try_get(12)?,
                    completed_at: row.try_get(13)?,
                    duration_ms: row.try_get(14)?,
                    error: row.try_get(15)?,
                })
            },
            mapper: |it| GetStepCheckpointsPaginatedNoCursor::from(it),
        }
    }
}
impl<'c, 'a, 's, C: GenericClient, T1: crate::StringSql>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        GetStepCheckpointsPaginatedNoCursorParams<T1>,
        GetStepCheckpointsPaginatedNoCursorQuery<
            'c,
            'a,
            's,
            C,
            GetStepCheckpointsPaginatedNoCursor,
            2,
        >,
        C,
    > for GetStepCheckpointsPaginatedNoCursorStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a GetStepCheckpointsPaginatedNoCursorParams<T1>,
    ) -> GetStepCheckpointsPaginatedNoCursorQuery<
        'c,
        'a,
        's,
        C,
        GetStepCheckpointsPaginatedNoCursor,
        2,
    > {
        self.bind(client, &params.execution_id, &params.max_results)
    }
}
pub struct GetRunningCheckpointIdStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_running_checkpoint_id() -> GetRunningCheckpointIdStmt {
    GetRunningCheckpointIdStmt(
        "SELECT id FROM workflow_step_checkpoints WHERE execution_id = $1 AND status = 'running' ORDER BY step_index DESC LIMIT 1",
        None,
    )
}
impl GetRunningCheckpointIdStmt {
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
        execution_id: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [execution_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct GetLatestStepProgressMarkerStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_latest_step_progress_marker() -> GetLatestStepProgressMarkerStmt {
    GetLatestStepProgressMarkerStmt(
        "SELECT id, COALESCE(checkpoint_id, '') as checkpoint_id, marker_type, COALESCE(current_value, 0) as current_value, COALESCE(total_value, 0) as total_value, COALESCE(description, '') as description, COALESCE(data_json, '') as data_json, created_at FROM step_progress_markers WHERE checkpoint_id = $1 ORDER BY id DESC LIMIT 1",
        None,
    )
}
impl GetLatestStepProgressMarkerStmt {
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
        checkpoint_id: &'a Option<T1>,
    ) -> GetLatestStepProgressMarkerQuery<'c, 'a, 's, C, GetLatestStepProgressMarker, 1> {
        GetLatestStepProgressMarkerQuery {
            client,
            params: [checkpoint_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetLatestStepProgressMarkerBorrowed, tokio_postgres::Error> {
                Ok(GetLatestStepProgressMarkerBorrowed {
                    id: row.try_get(0)?,
                    checkpoint_id: row.try_get(1)?,
                    marker_type: row.try_get(2)?,
                    current_value: row.try_get(3)?,
                    total_value: row.try_get(4)?,
                    description: row.try_get(5)?,
                    data_json: row.try_get(6)?,
                    created_at: row.try_get(7)?,
                })
            },
            mapper: |it| GetLatestStepProgressMarker::from(it),
        }
    }
}
