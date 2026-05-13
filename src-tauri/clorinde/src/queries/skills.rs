// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct CreateUserSkillParams<
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
    T19: crate::StringSql,
    T20: crate::StringSql,
> {
    pub id: T1,
    pub name: T2,
    pub slug: T3,
    pub description: Option<T4>,
    pub category: T5,
    pub tags: T6,
    pub icon: T7,
    pub color: T8,
    pub allowed_phases: T9,
    pub parameters: T10,
    pub template: T11,
    pub source: T12,
    pub version: T13,
    pub author: Option<T14>,
    pub checksum: Option<T15>,
    pub depends_on: Option<T16>,
    pub approval_status: Option<T17>,
    pub forked_from: Option<T18>,
    pub source_fix_id: Option<T19>,
    pub source_pattern_id: Option<T20>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListUserSkills {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub icon: String,
    pub color: String,
    pub allowed_phases: String,
    pub parameters: String,
    pub template: String,
    pub source: String,
    pub version: String,
    pub author: String,
    pub checksum: String,
    pub depends_on: String,
    pub usage_count: i64,
    pub approval_status: String,
    pub forked_from: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListUserSkillsBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub slug: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub icon: &'a str,
    pub color: &'a str,
    pub allowed_phases: &'a str,
    pub parameters: &'a str,
    pub template: &'a str,
    pub source: &'a str,
    pub version: &'a str,
    pub author: &'a str,
    pub checksum: &'a str,
    pub depends_on: &'a str,
    pub usage_count: i64,
    pub approval_status: &'a str,
    pub forked_from: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListUserSkillsBorrowed<'a>> for ListUserSkills {
    fn from(
        ListUserSkillsBorrowed {
            id,
            name,
            slug,
            description,
            category,
            tags,
            icon,
            color,
            allowed_phases,
            parameters,
            template,
            source,
            version,
            author,
            checksum,
            depends_on,
            usage_count,
            approval_status,
            forked_from,
            created_at,
            updated_at,
        }: ListUserSkillsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            slug: slug.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            icon: icon.into(),
            color: color.into(),
            allowed_phases: allowed_phases.into(),
            parameters: parameters.into(),
            template: template.into(),
            source: source.into(),
            version: version.into(),
            author: author.into(),
            checksum: checksum.into(),
            depends_on: depends_on.into(),
            usage_count,
            approval_status: approval_status.into(),
            forked_from: forked_from.into(),
            created_at,
            updated_at,
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetUserSkill {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub category: String,
    pub tags: String,
    pub icon: String,
    pub color: String,
    pub allowed_phases: String,
    pub parameters: String,
    pub template: String,
    pub source: String,
    pub version: String,
    pub author: String,
    pub checksum: String,
    pub depends_on: String,
    pub usage_count: i64,
    pub approval_status: String,
    pub forked_from: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct GetUserSkillBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub slug: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub tags: &'a str,
    pub icon: &'a str,
    pub color: &'a str,
    pub allowed_phases: &'a str,
    pub parameters: &'a str,
    pub template: &'a str,
    pub source: &'a str,
    pub version: &'a str,
    pub author: &'a str,
    pub checksum: &'a str,
    pub depends_on: &'a str,
    pub usage_count: i64,
    pub approval_status: &'a str,
    pub forked_from: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<GetUserSkillBorrowed<'a>> for GetUserSkill {
    fn from(
        GetUserSkillBorrowed {
            id,
            name,
            slug,
            description,
            category,
            tags,
            icon,
            color,
            allowed_phases,
            parameters,
            template,
            source,
            version,
            author,
            checksum,
            depends_on,
            usage_count,
            approval_status,
            forked_from,
            created_at,
            updated_at,
        }: GetUserSkillBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            slug: slug.into(),
            description: description.into(),
            category: category.into(),
            tags: tags.into(),
            icon: icon.into(),
            color: color.into(),
            allowed_phases: allowed_phases.into(),
            parameters: parameters.into(),
            template: template.into(),
            source: source.into(),
            version: version.into(),
            author: author.into(),
            checksum: checksum.into(),
            depends_on: depends_on.into(),
            usage_count,
            approval_status: approval_status.into(),
            forked_from: forked_from.into(),
            created_at,
            updated_at,
        }
    }
}
use crate::client::async_::GenericClient;
use futures::{self, StreamExt, TryStreamExt};
pub struct ListUserSkillsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<ListUserSkillsBorrowed, tokio_postgres::Error>,
    mapper: fn(ListUserSkillsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListUserSkillsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListUserSkillsBorrowed) -> R,
    ) -> ListUserSkillsQuery<'c, 'a, 's, C, R, N> {
        ListUserSkillsQuery {
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
pub struct GetUserSkillQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetUserSkillBorrowed, tokio_postgres::Error>,
    mapper: fn(GetUserSkillBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetUserSkillQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetUserSkillBorrowed) -> R,
    ) -> GetUserSkillQuery<'c, 'a, 's, C, R, N> {
        GetUserSkillQuery {
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
pub struct ListUserSkillsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_user_skills() -> ListUserSkillsStmt {
    ListUserSkillsStmt(
        "SELECT id, name, slug, COALESCE(description, '') as description, category, tags, icon, color, allowed_phases, parameters, template, source, version, COALESCE(author, '') as author, COALESCE(checksum, '') as checksum, COALESCE(depends_on, '[]') as depends_on, usage_count, COALESCE(approval_status, 'approved') as approval_status, COALESCE(forked_from, '') as forked_from, created_at, updated_at FROM user_skills ORDER BY updated_at DESC",
        None,
    )
}
impl ListUserSkillsStmt {
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
    ) -> ListUserSkillsQuery<'c, 'a, 's, C, ListUserSkills, 0> {
        ListUserSkillsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<ListUserSkillsBorrowed, tokio_postgres::Error> {
                Ok(ListUserSkillsBorrowed {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    slug: row.try_get(2)?,
                    description: row.try_get(3)?,
                    category: row.try_get(4)?,
                    tags: row.try_get(5)?,
                    icon: row.try_get(6)?,
                    color: row.try_get(7)?,
                    allowed_phases: row.try_get(8)?,
                    parameters: row.try_get(9)?,
                    template: row.try_get(10)?,
                    source: row.try_get(11)?,
                    version: row.try_get(12)?,
                    author: row.try_get(13)?,
                    checksum: row.try_get(14)?,
                    depends_on: row.try_get(15)?,
                    usage_count: row.try_get(16)?,
                    approval_status: row.try_get(17)?,
                    forked_from: row.try_get(18)?,
                    created_at: row.try_get(19)?,
                    updated_at: row.try_get(20)?,
                })
            },
            mapper: |it| ListUserSkills::from(it),
        }
    }
}
pub struct GetUserSkillStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_user_skill() -> GetUserSkillStmt {
    GetUserSkillStmt(
        "SELECT id, name, slug, COALESCE(description, '') as description, category, tags, icon, color, allowed_phases, parameters, template, source, version, COALESCE(author, '') as author, COALESCE(checksum, '') as checksum, COALESCE(depends_on, '[]') as depends_on, usage_count, COALESCE(approval_status, 'approved') as approval_status, COALESCE(forked_from, '') as forked_from, created_at, updated_at FROM user_skills WHERE id = $1",
        None,
    )
}
impl GetUserSkillStmt {
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
    ) -> GetUserSkillQuery<'c, 'a, 's, C, GetUserSkill, 1> {
        GetUserSkillQuery {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<GetUserSkillBorrowed, tokio_postgres::Error> {
                    Ok(GetUserSkillBorrowed {
                        id: row.try_get(0)?,
                        name: row.try_get(1)?,
                        slug: row.try_get(2)?,
                        description: row.try_get(3)?,
                        category: row.try_get(4)?,
                        tags: row.try_get(5)?,
                        icon: row.try_get(6)?,
                        color: row.try_get(7)?,
                        allowed_phases: row.try_get(8)?,
                        parameters: row.try_get(9)?,
                        template: row.try_get(10)?,
                        source: row.try_get(11)?,
                        version: row.try_get(12)?,
                        author: row.try_get(13)?,
                        checksum: row.try_get(14)?,
                        depends_on: row.try_get(15)?,
                        usage_count: row.try_get(16)?,
                        approval_status: row.try_get(17)?,
                        forked_from: row.try_get(18)?,
                        created_at: row.try_get(19)?,
                        updated_at: row.try_get(20)?,
                    })
                },
            mapper: |it| GetUserSkill::from(it),
        }
    }
}
pub struct CreateUserSkillStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn create_user_skill() -> CreateUserSkillStmt {
    CreateUserSkillStmt(
        "INSERT INTO user_skills ( id, name, slug, description, category, tags, icon, color, allowed_phases, parameters, template, source, version, author, checksum, depends_on, approval_status, forked_from, source_fix_id, source_pattern_id ) VALUES ( $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20 ) RETURNING id",
        None,
    )
}
impl CreateUserSkillStmt {
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
        T19: crate::StringSql,
        T20: crate::StringSql,
    >(
        &'s self,
        client: &'c C,
        id: &'a T1,
        name: &'a T2,
        slug: &'a T3,
        description: &'a Option<T4>,
        category: &'a T5,
        tags: &'a T6,
        icon: &'a T7,
        color: &'a T8,
        allowed_phases: &'a T9,
        parameters: &'a T10,
        template: &'a T11,
        source: &'a T12,
        version: &'a T13,
        author: &'a Option<T14>,
        checksum: &'a Option<T15>,
        depends_on: &'a Option<T16>,
        approval_status: &'a Option<T17>,
        forked_from: &'a Option<T18>,
        source_fix_id: &'a Option<T19>,
        source_pattern_id: &'a Option<T20>,
    ) -> StringQuery<'c, 'a, 's, C, String, 20> {
        StringQuery {
            client,
            params: [
                id,
                name,
                slug,
                description,
                category,
                tags,
                icon,
                color,
                allowed_phases,
                parameters,
                template,
                source,
                version,
                author,
                checksum,
                depends_on,
                approval_status,
                forked_from,
                source_fix_id,
                source_pattern_id,
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
    T19: crate::StringSql,
    T20: crate::StringSql,
>
    crate::client::async_::Params<
        'c,
        'a,
        's,
        CreateUserSkillParams<
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
            T19,
            T20,
        >,
        StringQuery<'c, 'a, 's, C, String, 20>,
        C,
    > for CreateUserSkillStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a CreateUserSkillParams<
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
            T19,
            T20,
        >,
    ) -> StringQuery<'c, 'a, 's, C, String, 20> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.slug,
            &params.description,
            &params.category,
            &params.tags,
            &params.icon,
            &params.color,
            &params.allowed_phases,
            &params.parameters,
            &params.template,
            &params.source,
            &params.version,
            &params.author,
            &params.checksum,
            &params.depends_on,
            &params.approval_status,
            &params.forked_from,
            &params.source_fix_id,
            &params.source_pattern_id,
        )
    }
}
pub struct DeleteUserSkillStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_user_skill() -> DeleteUserSkillStmt {
    DeleteUserSkillStmt("DELETE FROM user_skills WHERE id = $1 RETURNING id", None)
}
impl DeleteUserSkillStmt {
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
pub struct IncrementSkillUsageStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn increment_skill_usage() -> IncrementSkillUsageStmt {
    IncrementSkillUsageStmt(
        "UPDATE user_skills SET usage_count = usage_count + 1, updated_at = NOW() WHERE id = $1 RETURNING usage_count",
        None,
    )
}
impl IncrementSkillUsageStmt {
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
    ) -> I64Query<'c, 'a, 's, C, i64, 1> {
        I64Query {
            client,
            params: [id],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it,
        }
    }
}
