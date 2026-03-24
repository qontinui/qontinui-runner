//! GraphQL API for qontinui-runner.
//!
//! Provides a typed GraphQL endpoint alongside the existing REST API.
//! Subscriptions use WebSocket transport for real-time event streaming.

pub mod mutation;
pub mod query;
pub mod schema;
pub mod subscription;
pub mod types;

pub use schema::{build_schema, QontinuiSchema};
