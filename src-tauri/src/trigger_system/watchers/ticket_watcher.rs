//! Ticket watcher: registers a ticket sync task as a trigger watcher.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::database::pg::PgDb;
use crate::ticket_system::service::start_ticket_sync;
use crate::ticket_system::types::TicketProviderConfig;

/// Start a ticket watcher that runs the ticket sync as a background task.
pub fn start_ticket_watcher(
    pg_db: Arc<PgDb>,
    config: TicketProviderConfig,
    stop_signal: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    start_ticket_sync(pg_db, config, stop_signal)
}
