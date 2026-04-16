//! Ticket sync service — polls providers and creates workflow tasks from new tickets.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::database::pg::PgDb;
use crate::database::CreateTaskRunInput;
use crate::ticket_system::github::GitHubTicketProvider;
use crate::ticket_system::linear::LinearTicketProvider;
use crate::ticket_system::types::{
    Ticket, TicketProvider, TicketProviderConfig, TicketProviderConfigExt, TicketSource,
    TicketSourceExt, TicketState,
};

/// Marker type representing the (stateless) ticket sync service.
///
/// The actual sync runs as background tokio tasks spawned via `start_ticket_sync`.
/// This struct exists to provide a typed handle for callers who want to refer to
/// "the service" as a unit.
pub struct TicketSyncService;

impl TicketSyncService {
    pub fn start(
        pg_db: Arc<PgDb>,
        config: TicketProviderConfig,
        stop_signal: Arc<AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        start_ticket_sync(pg_db, config, stop_signal)
    }
}

/// Start the ticket sync service as a background polling task.
pub fn start_ticket_sync(
    pg_db: Arc<PgDb>,
    config: TicketProviderConfig,
    stop_signal: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let provider: Box<dyn TicketProvider> = match config.source {
            TicketSource::GitHub => match GitHubTicketProvider::new() {
                Ok(p) => Box::new(p),
                Err(e) => {
                    tracing::error!("Ticket sync: failed to create GitHub provider: {}", e);
                    return;
                }
            },
            TicketSource::Linear => match LinearTicketProvider::new() {
                Ok(p) => Box::new(p),
                Err(e) => {
                    tracing::error!("Ticket sync: failed to create Linear provider: {}", e);
                    return;
                }
            },
            TicketSource::Jira => {
                tracing::warn!(
                    "Ticket sync: {:?} provider not yet implemented",
                    config.source
                );
                return;
            }
        };

        let interval = Duration::from_secs(config.poll_interval_seconds.max(10));
        tracing::info!(
            "Ticket sync started (source: {:?}, target: {}, interval: {}s)",
            config.source,
            config.target,
            interval.as_secs()
        );

        loop {
            if stop_signal.load(Ordering::SeqCst) {
                tracing::info!("Ticket sync: stop signal received, exiting");
                break;
            }

            if let Err(e) = poll_and_sync(&pg_db, provider.as_ref(), &config).await {
                tracing::warn!("Ticket sync poll error: {}", e);
            }

            // Sleep in 5-second chunks for responsiveness
            let mut remaining = interval;
            while remaining > Duration::ZERO {
                if stop_signal.load(Ordering::SeqCst) {
                    break;
                }
                let chunk = remaining.min(Duration::from_secs(5));
                tokio::time::sleep(chunk).await;
                remaining = remaining.saturating_sub(chunk);
            }
        }

        tracing::info!("Ticket sync stopped");
    })
}

async fn poll_and_sync(
    pg_db: &Arc<PgDb>,
    provider: &dyn TicketProvider,
    config: &TicketProviderConfig,
) -> Result<(), String> {
    let tickets = provider.fetch_actionable(config).await?;
    tracing::debug!("Ticket sync: fetched {} actionable tickets", tickets.len());

    // Persist this provider config (with token) keyed by workflow_id so the
    // on-completion hook can reconstruct a provider after a restart and
    // actually close the ticket. Best-effort — don't abort polling on failure.
    match config.to_json_with_token() {
        Ok(json) => {
            if let Err(e) = pg_db
                .upsert_ticket_provider_config(&config.workflow_id, config.source.as_str(), &json)
                .await
            {
                tracing::warn!("Ticket sync: failed to persist provider config: {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("Ticket sync: failed to serialize provider config: {}", e);
        }
    }

    for ticket in tickets {
        // Dedup: check if this ticket already has a task_run
        match pg_db
            .get_ticket_task_mapping(config.source.as_str(), &ticket.external_id)
            .await
        {
            Ok(Some(_)) => {
                tracing::trace!(
                    "Ticket sync: {}/{} already mapped",
                    config.source.as_str(),
                    ticket.external_id
                );
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("Ticket sync: dedup check failed: {}", e);
                continue;
            }
        }

        // Create a new task_run from this ticket
        match create_task_from_ticket(pg_db, &ticket, config).await {
            Ok(task_run_id) => {
                tracing::info!(
                    "Ticket sync: created task {} for ticket {}/{}",
                    task_run_id,
                    config.source.as_str(),
                    ticket.external_id
                );

                // Mark ticket as in-progress so the source reflects that work has started.
                if let Err(e) = provider
                    .update_state(&ticket.external_id, TicketState::InProgress, config)
                    .await
                {
                    tracing::warn!(
                        "Ticket sync: failed to mark ticket {}/{} as in-progress: {}",
                        config.source.as_str(),
                        ticket.external_id,
                        e
                    );
                }

                // Store mapping for dedup
                if let Err(e) = pg_db
                    .insert_ticket_task_mapping(
                        config.source.as_str(),
                        &ticket.external_id,
                        &ticket.url,
                        &task_run_id,
                        &config.workflow_id,
                    )
                    .await
                {
                    tracing::warn!("Ticket sync: failed to store mapping: {}", e);
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Ticket sync: failed to create task for ticket {}/{}: {}",
                    config.source.as_str(),
                    ticket.external_id,
                    e
                );
            }
        }
    }

    Ok(())
}

async fn create_task_from_ticket(
    pg_db: &Arc<PgDb>,
    ticket: &Ticket,
    config: &TicketProviderConfig,
) -> Result<String, String> {
    // Load the configured workflow so the new task is bound to its name/type.
    // Without this, the ticket task would launch a generic workflow.
    let workflow = pg_db
        .get_unified_workflow(&config.workflow_id)
        .await
        .map_err(|e| format!("Failed to load workflow {}: {}", config.workflow_id, e))?
        .ok_or_else(|| format!("Workflow {} not found", config.workflow_id))?;

    let task_id = format!(
        "ticket-{}-{}-{}",
        config.source.as_str(),
        ticket.external_id,
        &uuid::Uuid::new_v4().to_string()[..8],
    );
    let task_name = format!("[{}] {}", config.source.as_str(), ticket.title);

    let prompt = format!(
        "You are working on ticket {}/{}: {}\n\n\
         ## Ticket Description\n\n{}\n\n\
         ## Labels\n{}\n\n\
         ## Source URL\n{}\n\n\
         Use the configured workflow to investigate and resolve this ticket.",
        config.source.as_str(),
        ticket.external_id,
        ticket.title,
        ticket.body,
        ticket.labels.join(", "),
        ticket.url,
    );

    let input = CreateTaskRunInput::new(&task_id, &task_name)
        .with_prompt(prompt)
        .with_workflow_name(&workflow.name)
        .with_workflow_type("unified")
        .with_task_type("ticket");

    let task_run = pg_db
        .create_task_run(&input)
        .await
        .map_err(|e| format!("Failed to create task run: {}", e))?;

    Ok(task_run.id)
}

/// Called when a task completes to update the ticket state (bidirectional sync).
///
/// Looks up the persisted provider config by `mapping.workflow_id`, reconstructs
/// the provider, and posts a state update + comment back to the ticket source.
pub async fn on_task_completed(
    pg_db: &Arc<PgDb>,
    task_run_id: &str,
    success: bool,
) -> Result<(), String> {
    let mapping = match pg_db.get_ticket_task_mapping_by_task(task_run_id).await? {
        Some(m) => m,
        None => return Ok(()), // Not a ticket-sourced task
    };

    tracing::info!(
        "Ticket sync: task {} completed (success: {}), updating ticket {}/{}",
        task_run_id,
        success,
        mapping.ticket_source,
        mapping.ticket_external_id
    );

    // Update local sync status first.
    let status = if success {
        "task_completed"
    } else {
        "task_failed"
    };
    pg_db
        .update_ticket_task_mapping_status(&mapping.id, status)
        .await?;

    // Look up the persisted provider config so we can reconstruct a provider
    // and actually push the state change back to the ticket source.
    let config_json = match pg_db
        .get_ticket_provider_config_by_workflow(&mapping.workflow_id)
        .await?
    {
        Some(j) => j,
        None => {
            tracing::debug!(
                "Ticket sync: no stored config for workflow {} — skipping ticket update",
                mapping.workflow_id
            );
            return Ok(());
        }
    };

    let config = TicketProviderConfig::from_json_with_token(&config_json)?;
    if !config.update_on_completion {
        tracing::debug!(
            "Ticket sync: update_on_completion=false for workflow {} — skipping",
            mapping.workflow_id
        );
        return Ok(());
    }

    let provider: Box<dyn TicketProvider> = match config.source {
        TicketSource::GitHub => match GitHubTicketProvider::new() {
            Ok(p) => Box::new(p),
            Err(e) => return Err(e),
        },
        TicketSource::Linear => match LinearTicketProvider::new() {
            Ok(p) => Box::new(p),
            Err(e) => return Err(e),
        },
        TicketSource::Jira => {
            tracing::debug!(
                "Ticket sync: provider {:?} not yet implemented for completion hook",
                config.source
            );
            return Ok(());
        }
    };

    let new_state = if success {
        TicketState::Done
    } else {
        TicketState::Open
    };
    let comment = if success {
        format!("Qontinui task {} completed successfully.", task_run_id)
    } else {
        format!(
            "Qontinui task {} failed. See logs for details.",
            task_run_id
        )
    };

    if let Err(e) = provider
        .update_state(&mapping.ticket_external_id, new_state, &config)
        .await
    {
        tracing::warn!("Failed to update ticket state: {}", e);
    }
    if let Err(e) = provider
        .add_comment(&mapping.ticket_external_id, &comment, &config)
        .await
    {
        tracing::warn!("Failed to add ticket comment: {}", e);
    }

    Ok(())
}
