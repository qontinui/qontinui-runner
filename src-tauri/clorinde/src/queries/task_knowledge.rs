// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreateTaskKnowledgeParams<
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
    pub category: T3,
    pub agent_type: T4,
    pub iteration: i32,
    pub content: T5,
    pub evidence: Option<T6>,
    pub confidence: T7,
    pub related_files: Option<T8>,
}
#[derive(Debug)]
pub struct ListTaskKnowledgeByCategoryParams<T1: crate::StringSql> {
    pub category: Option<T1>,
    pub max_results: i64,
}
#[derive(Debug)]
pub struct StoreKnowledgeEmbeddingParams<T1: crate::BytesSql, T2: crate::StringSql> {
    pub embedding: T1,
    pub id: T2,
}
#[derive(Debug)]
pub struct ResolveTaskKnowledgeParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub resolution_notes: Option<T1>,
    pub id: T2,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetTaskKnowledge {
    pub id: String,
    pub task_run_id: String,
    pub category: String,
    pub agent_type: String,
    pub iteration: i32,
    pub content: String,
    pub evidence: String,
    pub confidence: String,
    pub related_files: String,
    pub is_resolved: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetTaskKnowledgeBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub category: &'a str,
    pub agent_type: &'a str,
    pub iteration: i32,
    pub content: &'a str,
    pub evidence: &'a str,
    pub confidence: &'a str,
    pub related_files: &'a str,
    pub is_resolved: bool,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetTaskKnowledgeBorrowed<'a>> for GetTaskKnowledge {
    fn from(
        GetTaskKnowledgeBorrowed {
            id,
            task_run_id,
            category,
            agent_type,
            iteration,
            content,
            evidence,
            confidence,
            related_files,
            is_resolved,
            created_at,
        }: GetTaskKnowledgeBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            category: category.into(),
            agent_type: agent_type.into(),
            iteration,
            content: content.into(),
            evidence: evidence.into(),
            confidence: confidence.into(),
            related_files: related_files.into(),
            is_resolved,
            created_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListTaskKnowledgeByCategory {
    pub id: String,
    pub task_run_id: String,
    pub category: String,
    pub agent_type: String,
    pub iteration: i32,
    pub content: String,
    pub evidence: String,
    pub confidence: String,
    pub related_files: String,
    pub is_resolved: bool,
    pub content_embedding: Vec<u8>,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListTaskKnowledgeByCategoryBorrowed<'a> {
    pub id: &'a str,
    pub task_run_id: &'a str,
    pub category: &'a str,
    pub agent_type: &'a str,
    pub iteration: i32,
    pub content: &'a str,
    pub evidence: &'a str,
    pub confidence: &'a str,
    pub related_files: &'a str,
    pub is_resolved: bool,
    pub content_embedding: &'a [u8],
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListTaskKnowledgeByCategoryBorrowed<'a>> for ListTaskKnowledgeByCategory {
    fn from(
        ListTaskKnowledgeByCategoryBorrowed {
            id,
            task_run_id,
            category,
            agent_type,
            iteration,
            content,
            evidence,
            confidence,
            related_files,
            is_resolved,
            content_embedding,
            created_at,
        }: ListTaskKnowledgeByCategoryBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            task_run_id: task_run_id.into(),
            category: category.into(),
            agent_type: agent_type.into(),
            iteration,
            content: content.into(),
            evidence: evidence.into(),
            confidence: confidence.into(),
            related_files: related_files.into(),
            is_resolved,
            content_embedding: content_embedding.into(),
            created_at,
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
pub struct GetTaskKnowledgeQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<GetTaskKnowledgeBorrowed, tokio_postgres::Error>,
    mapper: fn(GetTaskKnowledgeBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetTaskKnowledgeQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetTaskKnowledgeBorrowed) -> R,
    ) -> GetTaskKnowledgeQuery<'c, 'a, 's, C, R, N> {
        GetTaskKnowledgeQuery {
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
pub struct ListTaskKnowledgeByCategoryQuery<
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
    ) -> Result<ListTaskKnowledgeByCategoryBorrowed, tokio_postgres::Error>,
    mapper: fn(ListTaskKnowledgeByCategoryBorrowed) -> T,
}
impl<
    'c,
    'a,
    's,
    C,
    T: 'c,
    const N: usize,
> ListTaskKnowledgeByCategoryQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListTaskKnowledgeByCategoryBorrowed) -> R,
    ) -> ListTaskKnowledgeByCategoryQuery<'c, 'a, 's, C, R, N> {
        ListTaskKnowledgeByCategoryQuery {
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
pub struct CreateTaskKnowledgeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_task_knowledge() -> CreateTaskKnowledgeStmt {
    CreateTaskKnowledgeStmt(
        "INSERT INTO task_knowledge ( id, task_run_id, category, agent_type, iteration, content, evidence, confidence, related_files, created_at ) VALUES ( $1, $2, $3, $4, $5, $6, $7, $8, $9, NOW() ) RETURNING id",
        None,
    )
}
impl CreateTaskKnowledgeStmt {
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
        category: &'a T3,
        agent_type: &'a T4,
        iteration: &'a i32,
        content: &'a T5,
        evidence: &'a Option<T6>,
        confidence: &'a T7,
        related_files: &'a Option<T8>,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        StringQuery {
            client,
            params: [
                id,
                task_run_id,
                category,
                agent_type,
                iteration,
                content,
                evidence,
                confidence,
                related_files,
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
> crate::client::async_::Params<
    'c,
    'a,
    's,
    CreateTaskKnowledgeParams<T1, T2, T3, T4, T5, T6, T7, T8>,
    StringQuery<'c, 'a, 's, C, String, 9>,
    C,
> for CreateTaskKnowledgeStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateTaskKnowledgeParams<T1, T2, T3, T4, T5, T6, T7, T8>,
    ) -> StringQuery<'c, 'a, 's, C, String, 9> {
        self.bind(
            client,
            &params.id,
            &params.task_run_id,
            &params.category,
            &params.agent_type,
            &params.iteration,
            &params.content,
            &params.evidence,
            &params.confidence,
            &params.related_files,
        )
    }
}
pub struct GetTaskKnowledgeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_task_knowledge() -> GetTaskKnowledgeStmt {
    GetTaskKnowledgeStmt(
        "SELECT id, task_run_id, category, agent_type, iteration, content, COALESCE(evidence, '') as evidence, confidence, COALESCE(related_files, '[]') as related_files, is_resolved, created_at FROM task_knowledge WHERE id = $1",
        None,
    )
}
impl GetTaskKnowledgeStmt {
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
    ) -> GetTaskKnowledgeQuery<'c, 'a, 's, C, GetTaskKnowledge, 1> {
        GetTaskKnowledgeQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetTaskKnowledgeBorrowed, tokio_postgres::Error> {
                Ok(GetTaskKnowledgeBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    category: row.try_get(2)?,
                    agent_type: row.try_get(3)?,
                    iteration: row.try_get(4)?,
                    content: row.try_get(5)?,
                    evidence: row.try_get(6)?,
                    confidence: row.try_get(7)?,
                    related_files: row.try_get(8)?,
                    is_resolved: row.try_get(9)?,
                    created_at: row.try_get(10)?,
                })
            },
            mapper: |it| GetTaskKnowledge::from(it),
        }
    }
}
pub struct ListTaskKnowledgeByCategoryStmt(
    &'static str,
    Option<tokio_postgres::Statement>,
);
pub fn list_task_knowledge_by_category() -> ListTaskKnowledgeByCategoryStmt {
    ListTaskKnowledgeByCategoryStmt(
        "SELECT id, task_run_id, category, agent_type, iteration, content, COALESCE(evidence, '') as evidence, confidence, COALESCE(related_files, '[]') as related_files, is_resolved, content_embedding, created_at FROM task_knowledge WHERE ($1::TEXT IS NULL OR category = $1) AND content_embedding IS NOT NULL ORDER BY created_at DESC LIMIT $2",
        None,
    )
}
impl ListTaskKnowledgeByCategoryStmt {
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
        category: &'a Option<T1>,
        max_results: &'a i64,
    ) -> ListTaskKnowledgeByCategoryQuery<
        'c,
        'a,
        's,
        C,
        ListTaskKnowledgeByCategory,
        2,
    > {
        ListTaskKnowledgeByCategoryQuery {
            client,
            params: [category, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListTaskKnowledgeByCategoryBorrowed, tokio_postgres::Error> {
                Ok(ListTaskKnowledgeByCategoryBorrowed {
                    id: row.try_get(0)?,
                    task_run_id: row.try_get(1)?,
                    category: row.try_get(2)?,
                    agent_type: row.try_get(3)?,
                    iteration: row.try_get(4)?,
                    content: row.try_get(5)?,
                    evidence: row.try_get(6)?,
                    confidence: row.try_get(7)?,
                    related_files: row.try_get(8)?,
                    is_resolved: row.try_get(9)?,
                    content_embedding: row.try_get(10)?,
                    created_at: row.try_get(11)?,
                })
            },
            mapper: |it| ListTaskKnowledgeByCategory::from(it),
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
    ListTaskKnowledgeByCategoryParams<T1>,
    ListTaskKnowledgeByCategoryQuery<'c, 'a, 's, C, ListTaskKnowledgeByCategory, 2>,
    C,
> for ListTaskKnowledgeByCategoryStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a ListTaskKnowledgeByCategoryParams<T1>,
    ) -> ListTaskKnowledgeByCategoryQuery<
        'c,
        'a,
        's,
        C,
        ListTaskKnowledgeByCategory,
        2,
    > {
        self.bind(client, &params.category, &params.max_results)
    }
}
pub struct StoreKnowledgeEmbeddingStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn store_knowledge_embedding() -> StoreKnowledgeEmbeddingStmt {
    StoreKnowledgeEmbeddingStmt(
        "UPDATE task_knowledge SET content_embedding = $1 WHERE id = $2",
        None,
    )
}
impl StoreKnowledgeEmbeddingStmt {
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
        T1: crate::BytesSql,
        T2: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        embedding: &'a T1,
        id: &'a T2,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[embedding, id]).await
    }
}
impl<
    'a,
    C: GenericClient + Send + Sync,
    T1: crate::BytesSql,
    T2: crate::StringSql,
> crate::client::async_::Params<
    'a,
    'a,
    'a,
    StoreKnowledgeEmbeddingParams<T1, T2>,
    std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    >,
    C,
> for StoreKnowledgeEmbeddingStmt {
    fn params(
        &'a self,
        client: &'a C,
        params: &'a StoreKnowledgeEmbeddingParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(client, &params.embedding, &params.id))
    }
}
pub struct ResolveTaskKnowledgeStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn resolve_task_knowledge() -> ResolveTaskKnowledgeStmt {
    ResolveTaskKnowledgeStmt(
        "UPDATE task_knowledge SET is_resolved = true, resolution_notes = $1, resolved_at = NOW() WHERE id = $2",
        None,
    )
}
impl ResolveTaskKnowledgeStmt {
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
        resolution_notes: &'a Option<T1>,
        id: &'a T2,
    ) -> Result<u64, tokio_postgres::Error> {
        client.execute(self.0, &[resolution_notes, id]).await
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
    ResolveTaskKnowledgeParams<T1, T2>,
    std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    >,
    C,
> for ResolveTaskKnowledgeStmt {
    fn params(
        &'a self,
        client: &'a C,
        params: &'a ResolveTaskKnowledgeParams<T1, T2>,
    ) -> std::pin::Pin<
        Box<dyn futures::Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a>,
    > {
        Box::pin(self.bind(client, &params.resolution_notes, &params.id))
    }
}
