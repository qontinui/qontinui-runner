// This file was generated with `clorinde`. Do not modify.

#[derive(Debug)]
pub struct SetSettingParams<T1: crate::StringSql, T2: crate::StringSql> {
    pub key: T1,
    pub value: T2,
}
#[derive(Debug)]
pub struct SaveConfigParams<
    T1: crate::StringSql,
    T2: crate::StringSql,
    T3: crate::StringSql,
    T4: crate::StringSql,
    T5: crate::StringSql,
> {
    pub id: T1,
    pub name: T2,
    pub config_json: T3,
    pub source_type: T4,
    pub source_path: Option<T5>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAllSettings {
    pub key: String,
    pub value: String,
}
pub struct GetAllSettingsBorrowed<'a> {
    pub key: &'a str,
    pub value: &'a str,
}
impl<'a> From<GetAllSettingsBorrowed<'a>> for GetAllSettings {
    fn from(GetAllSettingsBorrowed { key, value }: GetAllSettingsBorrowed<'a>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListConfigs {
    pub id: String,
    pub name: String,
    pub source_type: String,
    pub source_path: String,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
pub struct ListConfigsBorrowed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub source_type: &'a str,
    pub source_path: &'a str,
    pub created_at: chrono::DateTime<chrono::FixedOffset>,
    pub updated_at: chrono::DateTime<chrono::FixedOffset>,
}
impl<'a> From<ListConfigsBorrowed<'a>> for ListConfigs {
    fn from(
        ListConfigsBorrowed {
            id,
            name,
            source_type,
            source_path,
            created_at,
            updated_at,
        }: ListConfigsBorrowed<'a>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            source_type: source_type.into(),
            source_path: source_path.into(),
            created_at,
            updated_at,
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
pub struct GetAllSettingsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<GetAllSettingsBorrowed, tokio_postgres::Error>,
    mapper: fn(GetAllSettingsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> GetAllSettingsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(GetAllSettingsBorrowed) -> R,
    ) -> GetAllSettingsQuery<'c, 'a, 's, C, R, N> {
        GetAllSettingsQuery {
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
pub struct ListConfigsQuery<'c, 'a, 's, C: GenericClient, T, const N: usize> {
    client: &'c C,
    params: [&'a (dyn postgres_types::ToSql + Sync); N],
    query: &'static str,
    cached: Option<&'s tokio_postgres::Statement>,
    extractor: fn(&tokio_postgres::Row) -> Result<ListConfigsBorrowed, tokio_postgres::Error>,
    mapper: fn(ListConfigsBorrowed) -> T,
}
impl<'c, 'a, 's, C, T: 'c, const N: usize> ListConfigsQuery<'c, 'a, 's, C, T, N>
where
    C: GenericClient,
{
    pub fn map<R>(
        self,
        mapper: fn(ListConfigsBorrowed) -> R,
    ) -> ListConfigsQuery<'c, 'a, 's, C, R, N> {
        ListConfigsQuery {
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
pub struct GetSettingStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_setting() -> GetSettingStmt {
    GetSettingStmt("SELECT value FROM settings WHERE key = $1", None)
}
impl GetSettingStmt {
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
        key: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [key],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct SetSettingStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn set_setting() -> SetSettingStmt {
    SetSettingStmt(
        "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, NOW()) ON CONFLICT(key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW() RETURNING key",
        None,
    )
}
impl SetSettingStmt {
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
        key: &'a T1,
        value: &'a T2,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        StringQuery {
            client,
            params: [key, value],
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
        SetSettingParams<T1, T2>,
        StringQuery<'c, 'a, 's, C, String, 2>,
        C,
    > for SetSettingStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SetSettingParams<T1, T2>,
    ) -> StringQuery<'c, 'a, 's, C, String, 2> {
        self.bind(client, &params.key, &params.value)
    }
}
pub struct DeleteSettingStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_setting() -> DeleteSettingStmt {
    DeleteSettingStmt("DELETE FROM settings WHERE key = $1 RETURNING key", None)
}
impl DeleteSettingStmt {
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
        key: &'a T1,
    ) -> StringQuery<'c, 'a, 's, C, String, 1> {
        StringQuery {
            client,
            params: [key],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |row| Ok(row.try_get(0)?),
            mapper: |it| it.into(),
        }
    }
}
pub struct GetAllSettingsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_all_settings() -> GetAllSettingsStmt {
    GetAllSettingsStmt("SELECT key, value FROM settings ORDER BY key", None)
}
impl GetAllSettingsStmt {
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
    ) -> GetAllSettingsQuery<'c, 'a, 's, C, GetAllSettings, 0> {
        GetAllSettingsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor: |
                row: &tokio_postgres::Row,
            | -> Result<GetAllSettingsBorrowed, tokio_postgres::Error> {
                Ok(GetAllSettingsBorrowed {
                    key: row.try_get(0)?,
                    value: row.try_get(1)?,
                })
            },
            mapper: |it| GetAllSettings::from(it),
        }
    }
}
pub struct SaveConfigStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn save_config() -> SaveConfigStmt {
    SaveConfigStmt(
        "INSERT INTO configs (id, name, config_json, source_type, source_path, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, NOW(), NOW()) ON CONFLICT(id) DO UPDATE SET name = EXCLUDED.name, config_json = EXCLUDED.config_json, source_type = EXCLUDED.source_type, source_path = EXCLUDED.source_path, updated_at = NOW() RETURNING id",
        None,
    )
}
impl SaveConfigStmt {
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
        id: &'a T1,
        name: &'a T2,
        config_json: &'a T3,
        source_type: &'a T4,
        source_path: &'a Option<T5>,
    ) -> StringQuery<'c, 'a, 's, C, String, 5> {
        StringQuery {
            client,
            params: [id, name, config_json, source_type, source_path],
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
        SaveConfigParams<T1, T2, T3, T4, T5>,
        StringQuery<'c, 'a, 's, C, String, 5>,
        C,
    > for SaveConfigStmt
{
    fn params(
        &'s self,
        client: &'c C,
        params: &'a SaveConfigParams<T1, T2, T3, T4, T5>,
    ) -> StringQuery<'c, 'a, 's, C, String, 5> {
        self.bind(
            client,
            &params.id,
            &params.name,
            &params.config_json,
            &params.source_type,
            &params.source_path,
        )
    }
}
pub struct GetConfigStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn get_config() -> GetConfigStmt {
    GetConfigStmt("SELECT config_json FROM configs WHERE id = $1", None)
}
impl GetConfigStmt {
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
pub struct ListConfigsStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn list_configs() -> ListConfigsStmt {
    ListConfigsStmt(
        "SELECT id, name, source_type, COALESCE(source_path, '') as source_path, created_at, updated_at FROM configs ORDER BY updated_at DESC",
        None,
    )
}
impl ListConfigsStmt {
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
    ) -> ListConfigsQuery<'c, 'a, 's, C, ListConfigs, 0> {
        ListConfigsQuery {
            client,
            params: [],
            query: self.0,
            cached: self.1.as_ref(),
            extractor:
                |row: &tokio_postgres::Row| -> Result<ListConfigsBorrowed, tokio_postgres::Error> {
                    Ok(ListConfigsBorrowed {
                        id: row.try_get(0)?,
                        name: row.try_get(1)?,
                        source_type: row.try_get(2)?,
                        source_path: row.try_get(3)?,
                        created_at: row.try_get(4)?,
                        updated_at: row.try_get(5)?,
                    })
                },
            mapper: |it| ListConfigs::from(it),
        }
    }
}
pub struct DeleteConfigStmt(&'static str, Option<tokio_postgres::Statement>);
pub fn delete_config() -> DeleteConfigStmt {
    DeleteConfigStmt("DELETE FROM configs WHERE id = $1 RETURNING id", None)
}
impl DeleteConfigStmt {
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
