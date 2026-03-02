//! Schedule watcher: fires triggers on a cron schedule.
//!
//! Uses the `cron` crate to parse cron expressions and calculates
//! sleep durations between firings. Supports IANA timezone-aware scheduling
//! via `chrono-tz` (e.g., "America/New_York", "Europe/London").

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::super::types::TriggerEvent;

/// Resolved timezone for schedule calculations.
enum ResolvedTz {
    Utc,
    Local,
    Iana(chrono_tz::Tz),
}

/// Parse a timezone string into a ResolvedTz.
///
/// Accepts: "UTC", "Local", or any IANA timezone (e.g., "America/New_York").
fn parse_timezone(tz: &str) -> Result<ResolvedTz, String> {
    if tz.eq_ignore_ascii_case("utc") || tz.eq_ignore_ascii_case("utc+0") {
        return Ok(ResolvedTz::Utc);
    }
    if tz.eq_ignore_ascii_case("local") {
        return Ok(ResolvedTz::Local);
    }

    // Try parsing as IANA timezone
    tz.parse::<chrono_tz::Tz>()
        .map(ResolvedTz::Iana)
        .map_err(|_| {
            format!(
                "Unknown timezone '{}'. Use 'UTC', 'Local', or an IANA name like 'America/New_York'",
                tz
            )
        })
}

/// Calculate the next fire time in UTC for a given schedule and timezone.
fn next_fire_utc(
    schedule: &cron::Schedule,
    tz: &ResolvedTz,
) -> Option<chrono::DateTime<chrono::Utc>> {
    match tz {
        ResolvedTz::Utc => schedule.upcoming(chrono::Utc).next(),
        ResolvedTz::Local => schedule
            .upcoming(chrono::Local)
            .next()
            .map(|dt| dt.with_timezone(&chrono::Utc)),
        ResolvedTz::Iana(iana_tz) => schedule
            .upcoming(*iana_tz)
            .next()
            .map(|dt| dt.with_timezone(&chrono::Utc)),
    }
}

/// Start a schedule watcher for a trigger.
///
/// Parses the cron expression, calculates the next fire time, sleeps until then,
/// and sends a TriggerEvent. Repeats until the stop signal is set.
pub fn start_schedule(
    trigger_id: String,
    cron_expression: String,
    timezone: String,
    tx: mpsc::Sender<TriggerEvent>,
    stop_signal: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<()>, String> {
    // Validate cron expression up front
    let schedule = cron::Schedule::from_str(&cron_expression)
        .map_err(|e| format!("Invalid cron expression '{}': {}", cron_expression, e))?;

    // Parse timezone
    let resolved_tz = parse_timezone(&timezone)?;

    let tz_display = match &resolved_tz {
        ResolvedTz::Utc => "UTC".to_string(),
        ResolvedTz::Local => "Local".to_string(),
        ResolvedTz::Iana(tz) => tz.to_string(),
    };

    info!(
        "Starting schedule watcher for trigger '{}': cron='{}', tz={}",
        trigger_id, cron_expression, tz_display
    );

    let handle = tokio::spawn(async move {
        loop {
            if stop_signal.load(Ordering::SeqCst) {
                debug!("Schedule watcher '{}' received stop signal", trigger_id);
                break;
            }

            // Calculate next fire time
            let next_utc = match next_fire_utc(&schedule, &resolved_tz) {
                Some(dt) => dt,
                None => {
                    warn!(
                        "Schedule watcher '{}': no upcoming fire time, stopping",
                        trigger_id
                    );
                    break;
                }
            };

            let now = chrono::Utc::now();
            let duration = match (next_utc - now).to_std() {
                Ok(d) => d,
                Err(_) => {
                    // Next fire time is in the past — skip to the following occurrence
                    // to avoid firing multiple times within a single cron interval
                    debug!(
                        "Schedule watcher '{}': fire time already passed, skipping to next",
                        trigger_id
                    );
                    continue;
                }
            };

            debug!(
                "Schedule watcher '{}': next fire at {} (sleeping {:.1}s)",
                trigger_id,
                next_utc.format("%Y-%m-%d %H:%M:%S UTC"),
                duration.as_secs_f64()
            );

            // Sleep until next fire time, checking stop signal periodically
            // Break the sleep into 5-second chunks to remain responsive to stop signals
            let mut remaining = duration;
            let check_interval = std::time::Duration::from_secs(5);

            while !remaining.is_zero() {
                if stop_signal.load(Ordering::SeqCst) {
                    debug!(
                        "Schedule watcher '{}' received stop signal during sleep",
                        trigger_id
                    );
                    return;
                }

                let sleep_for = remaining.min(check_interval);
                tokio::time::sleep(sleep_for).await;
                remaining = remaining.saturating_sub(sleep_for);
            }

            // Fire the trigger
            if stop_signal.load(Ordering::SeqCst) {
                break;
            }

            let fire_time = chrono::Utc::now();
            info!(
                "Schedule trigger '{}' fired at {}",
                trigger_id,
                fire_time.format("%Y-%m-%d %H:%M:%S UTC")
            );

            let mut variables = HashMap::new();
            variables.insert("fire_time".to_string(), fire_time.to_rfc3339());
            variables.insert("cron_expression".to_string(), cron_expression.clone());

            let event = TriggerEvent {
                trigger_id: trigger_id.clone(),
                event_type: "schedule_fire".to_string(),
                event_data: serde_json::json!({
                    "fire_time": fire_time.to_rfc3339(),
                    "cron_expression": cron_expression,
                    "timezone": timezone,
                }),
                variables,
                chain_depth: 0,
            };

            if let Err(e) = tx.send(event).await {
                warn!("Failed to send schedule event for '{}': {}", trigger_id, e);
                break;
            }
        }

        info!("Schedule watcher '{}' stopped", trigger_id);
    });

    Ok(handle)
}
