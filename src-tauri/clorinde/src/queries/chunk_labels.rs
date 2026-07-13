// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct UpsertChunkLabelParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
> {
    pub config_id: T1,
    pub chunk_id: T2,
    pub label: T3,
}
#[derive(Debug)]
pub struct DeleteChunkLabelParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub config_id: T1,
    pub chunk_id: T2,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListChunkLabels {
    pub chunk_id: String,
    pub label: String,
    pub updated_at: String,
}
pub struct ListChunkLabelsBorrowed<'a> {
    pub chunk_id: &'a str,
    pub label: &'a str,
    pub updated_at: &'a str,
}
impl<'a> From<ListChunkLabelsBorrowed<'a>> for ListChunkLabels {
    fn from(
        ListChunkLabelsBorrowed {
            chunk_id,
            label,
            updated_at,
        }: ListChunkLabelsBorrowed<'a>,
    ) -> Self {
        Self {
            chunk_id: chunk_id.into(),
            label: label.into(),
            updated_at: updated_at.into(),
        }
    }
}
use futures::{self, StreamExt, TryStreamExt};
use crate::client::async_::GenericClient;
pub struct ListChunkLabelsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<ListChunkLabelsBorrowed, tokio_postgres::Error>,
    mapper: fn(ListChunkLabelsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListChunkLabelsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListChunkLabelsBorrowed) -> R,
    ) -> ListChunkLabelsQuery<'c, 'a, 's, C, R, N> {
        ListChunkLabelsQuery {
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
pub struct ListChunkLabelsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_chunk_labels() -> ListChunkLabelsStmt {
    ListChunkLabelsStmt(
        "SELECT chunk_id, label, updated_at::TEXT AS updated_at FROM chunk_labels WHERE config_id = $1",
        None,
    )
}
impl ListChunkLabelsStmt {
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
        config_id: &'a T1,
    ) -> ListChunkLabelsQuery<'c, 'a, 's, C, ListChunkLabels, 1> {
        ListChunkLabelsQuery {
            client,
            params: [config_id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListChunkLabelsBorrowed, tokio_postgres::Error> {
                Ok(ListChunkLabelsBorrowed {
                    chunk_id: row.try_get(0)?,
                    label: row.try_get(1)?,
                    updated_at: row.try_get(2)?,
                })
            },
            mapper: |it| ListChunkLabels::from(it),
        }
    }
}
pub struct UpsertChunkLabelStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn upsert_chunk_label() -> UpsertChunkLabelStmt {
    UpsertChunkLabelStmt(
        "INSERT INTO chunk_labels (config_id, chunk_id, label, updated_at) VALUES ($1, $2, $3, NOW()) ON CONFLICT (config_id, chunk_id) DO UPDATE SET label = EXCLUDED.label, updated_at = NOW()",
        None,
    )
}
impl UpsertChunkLabelStmt {
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
    >(
        &'s self,
        client: &'c C,
        config_id: &'a T1,
        chunk_id: &'a T2,
        label: &'a T3,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[config_id, chunk_id, label]).await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
> crate::client::async_::Params<
    'a,
    'a,
    'a,
    UpsertChunkLabelParams<T1, T2, T3>,
    std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    >,
    C,
> for UpsertChunkLabelStmt {
    fn params(
        &'a self,
        client: &'a C,
        params: &'a UpsertChunkLabelParams<T1, T2, T3>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(client, &params.config_id, &params.chunk_id, &params.label))
    }
}
pub struct DeleteChunkLabelStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_chunk_label() -> DeleteChunkLabelStmt {
    DeleteChunkLabelStmt(
        "DELETE FROM chunk_labels WHERE config_id = $1 AND chunk_id = $2",
        None,
    )
}
impl DeleteChunkLabelStmt {
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
    >(
        &'s self,
        client: &'c C,
        config_id: &'a T1,
        chunk_id: &'a T2,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[config_id, chunk_id]).await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::StringSql,
    T2: crate::StringSql,
> crate::client::async_::Params<
    'a,
    'a,
    'a,
    DeleteChunkLabelParams<T1, T2>,
    std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    >,
    C,
> for DeleteChunkLabelStmt {
    fn params(
        &'a self,
        client: &'a C,
        params: &'a DeleteChunkLabelParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(client, &params.config_id, &params.chunk_id))
    }
}
