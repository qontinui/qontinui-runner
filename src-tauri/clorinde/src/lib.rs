// This file was generated with `clorinde`. Do not modify.

#[allow(clippy::all, clippy::pedantic)]
#[allow(unused_variables)]
#[allow(unused_imports)]
#[allow(dead_code)]
pub mod types;
#[allow(clippy::all, clippy::pedantic)]
#[allow(unused_variables)]
#[allow(unused_imports)]
#[allow(dead_code)]
pub mod queries;
pub mod client;
mod array_iterator;
mod domain;
mod type_traits;
mod utils;
pub(crate) use utils::slice_iter;
pub use array_iterator::ArrayIterator;
pub use domain::{Domain, DomainArray};
pub use type_traits::{ArraySql, BytesSql, IterSql, StringSql};
#[cfg(feature = "deadpool")]
pub use deadpool_postgres;
#[cfg(any(feature = "deadpool", feature = "wasm-async"))]
pub use tokio_postgres;
#[cfg(any(feature = "deadpool", feature = "wasm-async"))]
pub use tokio_postgres::fallible_iterator;
#[cfg(not(any(feature = "deadpool", feature = "wasm-async")))]
pub use postgres;
#[cfg(not(any(feature = "deadpool", feature = "wasm-async")))]
pub use postgres::fallible_iterator;
pub use type_traits::JsonSql;
