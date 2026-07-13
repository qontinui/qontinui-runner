// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct SearchEventsParams<T1: crate::StringSql> {
    pub query_text: T1,
    pub since_ts: chrono::DateTime<chrono::FixedOffset>,
    pub max_results: i64,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchEventsRow {
    pub source_table: String,
    pub record_id: String,
    pub snippet: String,
    pub event_ts: chrono::DateTime<chrono::FixedOffset>,
    pub score: f32,
}
pub struct SearchEventsRowBorrowed<'a> {
    pub source_table: &'a str,
    pub record_id: &'a str,
    pub snippet: &'a str,
    pub event_ts: chrono::DateTime<chrono::FixedOffset>,
    pub score: f32,
}
impl<'a> From<SearchEventsRowBorrowed<'a>> for SearchEventsRow {
    fn from(
        SearchEventsRowBorrowed {
            source_table,
            record_id,
            snippet,
            event_ts,
            score,
        }: SearchEventsRowBorrowed<'a>,
    ) -> Self {
        Self {
            source_table: source_table.into(),
            record_id: record_id.into(),
            snippet: snippet.into(),
            event_ts,
            score,
        }
    }
}
use futures::{self, StreamExt, TryStreamExt};
use crate::client::async_::GenericClient;
pub struct SearchEventsRowQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(
        &tokio_postgres::Row,
    ) -> Result<SearchEventsRowBorrowed, tokio_postgres::Error>,
    mapper: fn(SearchEventsRowBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> SearchEventsRowQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(SearchEventsRowBorrowed) -> R,
    ) -> SearchEventsRowQuery<'c, 'a, 's, C, R, N> {
        SearchEventsRowQuery {
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
pub struct SearchEventsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn search_events() -> SearchEventsStmt {
    SearchEventsStmt(
        "WITH q AS ( SELECT plainto_tsquery('english', $1) AS tsq, $2::timestamptz AS since_ts, $3::bigint AS max_rows ) SELECT source_table, record_id, snippet, event_ts, score FROM ( SELECT 'activity_timeline'::text AS source_table, at.id::text AS record_id, LEFT(at.text_content, 400) AS snippet, at.created_at AS event_ts, ts_rank_cd(to_tsvector('english', at.text_content), q.tsq)::float4 AS score FROM activity_timeline at, q WHERE to_tsvector('english', at.text_content) @@ q.tsq AND at.created_at >= q.since_ts AND NOT at.is_deleted UNION ALL SELECT 'observations'::text, o.id::text, LEFT(o.title || ' — ' || o.content, 400), o.created_at, ts_rank_cd(to_tsvector('english', o.title || ' ' || o.content), q.tsq)::float4 FROM observations o, q WHERE to_tsvector('english', o.title || ' ' || o.content) @@ q.tsq AND o.created_at >= q.since_ts AND NOT o.is_deleted UNION ALL SELECT 'deferred_questions'::text, dq.id, LEFT(dq.question, 400), dq.created_at, ts_rank_cd( to_tsvector('english', dq.question || ' ' || COALESCE(dq.context_json, '')), q.tsq )::float4 FROM deferred_questions dq, q WHERE to_tsvector('english', dq.question || ' ' || COALESCE(dq.context_json, '')) @@ q.tsq AND dq.created_at >= q.since_ts UNION ALL SELECT 'error_events'::text, ee.id::text, LEFT(ee.message, 400), ee.captured_at, ts_rank_cd( to_tsvector( 'english', ee.message || ' ' || COALESCE(ee.stack_trace, '') || ' ' || COALESCE(ee.context_lines, '') ), q.tsq )::float4 FROM error_events ee, q WHERE to_tsvector( 'english', ee.message || ' ' || COALESCE(ee.stack_trace, '') || ' ' || COALESCE(ee.context_lines, '') ) @@ q.tsq AND ee.captured_at >= q.since_ts ) sub ORDER BY score DESC, event_ts DESC LIMIT (SELECT max_rows FROM q)",
        None,
    )
}
impl SearchEventsStmt {
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
        query_text: &'a T1,
        since_ts: &'a chrono::DateTime<chrono::FixedOffset>,
        max_results: &'a i64,
    ) -> SearchEventsRowQuery<'c, 'a, 's, C, SearchEventsRow, 3> {
        SearchEventsRowQuery {
            client,
            params: [query_text, since_ts, max_results],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<SearchEventsRowBorrowed, tokio_postgres::Error> {
                Ok(SearchEventsRowBorrowed {
                    source_table: row.try_get(0)?,
                    record_id: row.try_get(1)?,
                    snippet: row.try_get(2)?,
                    event_ts: row.try_get(3)?,
                    score: row.try_get(4)?,
                })
            },
            mapper: |it| SearchEventsRow::from(it),
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
    SearchEventsParams<T1>,
    SearchEventsRowQuery<'c, 'a, 's, C, SearchEventsRow, 3>,
    C,
> for SearchEventsStmt {
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SearchEventsParams<T1>,
    ) -> SearchEventsRowQuery<'c, 'a, 's, C, SearchEventsRow, 3> {
        self.bind(client, &params.query_text, &params.since_ts, &params.max_results)
    }
}
