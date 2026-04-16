//! Ticket system integration — pluggable abstraction over GitHub Issues, Linear, Jira.
//!
//! Provides a trait-based interface for fetching actionable tickets, adding comments,
//! and updating state. The `TicketSyncService` polls configured providers and auto-creates
//! workflow tasks from new tickets.

pub mod github;
pub mod linear;
pub mod service;
pub mod types;

pub use service::TicketSyncService;
pub use types::{
    Ticket, TicketComment, TicketProvider, TicketProviderConfig, TicketProviderConfigExt,
    TicketSource, TicketSourceExt, TicketState,
};
