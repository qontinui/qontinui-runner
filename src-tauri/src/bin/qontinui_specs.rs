//! `qontinui-specs` — Spec-Check observability CLI (Plan 06 Steps 4–6).
//!
//! Three subcommands answer the three operator questions Stream G ships:
//!
//! - `coverage` — "how many pages does B know about vs. how many we see?"
//!   Reports `pages_specced / pages_observed * 100` with a top-20
//!   backlog of observed-but-unspecced pages.
//! - `match-rate` — "which pages / states are perpetually borderline?"
//!   Last-N-day match-rate distribution from
//!   `project.workflow_verification_phase_results.result_json`.
//! - `flywheel` — "is the flywheel actually turning?" Proposal queue depth,
//!   promotion velocity, demotion rate, drift detection rate,
//!   and the last 20 transitions from `project.proposal_events`.
//!
//! The CLI opens its own short-lived PG connection (via the active profile's
//! `database_url`) and exits. It does NOT talk to the running primary runner
//! — that's why this is a separate binary rather than a Tauri command (per
//! Plan 06 §"Why a new binary, not a Tauri command", and CLAUDE.md's
//! runner-lifecycle rules).
//!
//! The `database/pg/` Rust module that backs the rest of the runner is not
//! reachable from a separate binary (it lives under `mod database;` in
//! `main.rs`, not under `qontinui_runner_lib`). This binary therefore uses
//! raw `tokio_postgres` for the small handful of queries it needs.
//!
//! Output format: `--format text` (default) for human-readable tables;
//! `--format json` for `serde_json::to_string_pretty` consumption by
//! supervisor cron / dashboards.

use clap::{Parser, Subcommand};
use qontinui_runner_lib::profiles::load_strict;
use serde_json::{json, Value};
use std::process::ExitCode;
use tokio_postgres::Client;

// ============================================================================
// CLI surface
// ============================================================================

#[derive(Parser)]
#[command(
    name = "qontinui-specs",
    version,
    about = "Spec-Check observability CLI"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// pages_specced / pages_observed * 100 — coverage growth surface.
    Coverage(CoverageArgs),
    /// Last-window match-rate distribution per page (or per state with --by-state).
    MatchRate(MatchRateArgs),
    /// Proposal queue depth, promotion velocity, drift rate (the "is the
    /// flywheel turning?" surface).
    Flywheel(FlywheelArgs),
}

#[derive(Parser)]
struct CoverageArgs {
    /// spec-multi-app Stream E.6: restrict to one registered app.
    /// Omit to aggregate across all apps (output groups by `app_id`).
    #[arg(long)]
    app: Option<String>,
    /// Sliding observation window (days).
    #[arg(long, default_value_t = 90)]
    days: u32,
    /// Observation threshold to count a page as "observed".
    #[arg(long, default_value_t = 1)]
    min_observations: u32,
    /// Output format: `text` | `json`.
    #[arg(long, default_value = "text")]
    format: String,
}

#[derive(Parser)]
struct MatchRateArgs {
    /// spec-multi-app Stream E.6: restrict to one registered app.
    /// Omit to aggregate across all apps (output groups by `app_id`).
    #[arg(long)]
    app: Option<String>,
    /// Time window (days).
    #[arg(long, default_value_t = 7)]
    days: u32,
    /// Filter to one page (omit for all pages).
    #[arg(long)]
    page_id: Option<String>,
    /// Group by state rather than by page.
    #[arg(long, default_value_t = false)]
    by_state: bool,
    /// Minimum row count per group; smaller groups are dropped as noise.
    #[arg(long, default_value_t = 3)]
    min_rows: u32,
    /// Output format: `text` | `json`.
    #[arg(long, default_value = "text")]
    format: String,
}

#[derive(Parser)]
struct FlywheelArgs {
    /// spec-multi-app Stream E.6: restrict to one registered app.
    /// Omit to aggregate across all apps (output groups by `app_id`).
    #[arg(long)]
    app: Option<String>,
    /// Window for velocity / demotion / drift aggregates (days).
    #[arg(long, default_value_t = 7)]
    days: u32,
    /// Output format: `text` | `json`.
    #[arg(long, default_value = "text")]
    format: String,
}

// ============================================================================
// Entry point
// ============================================================================

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let client = match open_pg().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("qontinui-specs: PG connection failed: {}", e);
            return ExitCode::from(2);
        }
    };

    // spec-multi-app Stream E.6: if `--app` is supplied on any subcommand,
    // validate it against `project.apps` before dispatching so typos surface
    // as exit-2 with a usable error message rather than `0` results.
    let app_filter = match &cli.cmd {
        Cmd::Coverage(args) => args.app.as_deref(),
        Cmd::MatchRate(args) => args.app.as_deref(),
        Cmd::Flywheel(args) => args.app.as_deref(),
    };
    if let Some(id) = app_filter {
        if let Err(e) = validate_app_filter(&client, id).await {
            eprintln!("qontinui-specs: {}", e);
            return ExitCode::from(2);
        }
    }

    let result = match cli.cmd {
        Cmd::Coverage(args) => run_coverage(&client, &args).await,
        Cmd::MatchRate(args) => run_match_rate(&client, &args).await,
        Cmd::Flywheel(args) => run_flywheel(&client, &args).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("qontinui-specs: {}", e);
            ExitCode::from(1)
        }
    }
}

/// Validate a `--app <id>` filter against the registry. Returns Ok if the id
/// is present in `project.apps`; otherwise returns a multi-line error string
/// listing the valid ids so the user can correct the typo.
///
/// If `project.apps` doesn't exist (very old DB shape), we treat the filter
/// as a no-op rather than blocking the CLI — the per-subcommand queries will
/// simply find zero rows for that app, which is the same UX as a registered
/// app with no events.
async fn validate_app_filter(client: &Client, app_id: &str) -> Result<(), String> {
    if !table_exists(client, "project", "apps").await? {
        // No registry table on this DB — accept the filter quietly so the
        // CLI still works against legacy/test PGs.
        return Ok(());
    }
    let hit = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM project.apps WHERE app_id = $1)",
            &[&app_id],
        )
        .await
        .map_err(|e| format!("validate_app_filter: {}", e))?;
    if hit.get::<_, bool>(0) {
        return Ok(());
    }
    // List the valid ids for the error message.
    let rows = client
        .query("SELECT app_id FROM project.apps ORDER BY app_id", &[])
        .await
        .map_err(|e| format!("validate_app_filter list: {}", e))?;
    let valid: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
    Err(format!(
        "--app '{}' is not registered. Valid app_ids: [{}]",
        app_id,
        valid.join(", ")
    ))
}

/// Open a single PG connection from the active profile's `database_url`.
///
/// Sets `search_path` to `project, public` post-connect so unqualified table
/// names in the queries resolve to `project.*` first — same convention as the
/// runner's `database/pg/mod.rs::new` pool post-create hook.
async fn open_pg() -> Result<Client, String> {
    let profile = load_strict().map_err(|e| format!("active profile lacks database_url: {}", e))?;
    let (client, conn) = tokio_postgres::connect(&profile.database_url, tokio_postgres::NoTls)
        .await
        .map_err(|e| format!("connect to PG failed: {}", e))?;
    // Spawn the connection driver. If it ends, the next query on `client`
    // will error — fine for a one-shot CLI.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("qontinui-specs: PG connection ended: {}", e);
        }
    });
    client
        .batch_execute("SET search_path TO project, public")
        .await
        .map_err(|e| format!("SET search_path failed: {}", e))?;
    Ok(client)
}

// ============================================================================
// `coverage` subcommand
// ============================================================================

async fn run_coverage(client: &Client, args: &CoverageArgs) -> Result<(), String> {
    let days = args.days as i32;
    let min_obs = args.min_observations as i64;
    // spec-multi-app Stream E.6: `--app` scopes the surface to one app or
    // (when omitted) groups output by registered `app_id`. The underlying
    // `state_discovery_artifacts` / `cached_specs` tables do NOT yet carry
    // `app_id` — extending them is Stream F's bootstrap-migration scope.
    // Until then, when `--app` is set we tag the output as scoped to that
    // app (the numbers are still cross-app), and when it's omitted we emit
    // a per-app section per registered row. The aggregate behavior matches
    // the pre-multi-app shape.
    let scoped_label = args.app.as_deref();

    // Probe required tables — the plan's queries reference
    // `state_discovery_artifacts` and `cached_specs`, both of which may not
    // exist on every dev DB shape. Fall back gracefully so the CLI is still
    // useful when only one side is populated.
    let observed_exists = table_exists(client, "project", "state_discovery_artifacts").await?;
    let specs_exists = table_exists(client, "project", "cached_specs").await?;

    let (pages_observed, backlog) = if observed_exists && specs_exists {
        let row = client
            .query_one(
                r#"
                WITH observed AS (
                    SELECT pathname, count(*) AS obs_count
                    FROM project.state_discovery_artifacts
                    WHERE captured_at > now() - make_interval(days => $1)
                    GROUP BY pathname
                    HAVING count(*) >= $2
                )
                SELECT
                    (SELECT count(*)::bigint FROM observed) AS pages_observed,
                    (SELECT array_agg(pathname ORDER BY obs_count DESC)
                     FROM (SELECT pathname, obs_count FROM observed
                           WHERE pathname NOT IN (
                               SELECT page_id FROM project.cached_specs
                           )
                           ORDER BY obs_count DESC
                           LIMIT 20) bl
                    ) AS backlog_top20
                "#,
                &[&days, &min_obs],
            )
            .await
            .map_err(|e| format!("coverage observed query failed: {}", e))?;
        let count: i64 = row.get(0);
        let backlog: Option<Vec<String>> = row.try_get(1).ok();
        (Some(count), backlog.unwrap_or_default())
    } else if observed_exists {
        // No cached_specs to anti-join — just count observations.
        let row = client
            .query_one(
                r#"
                SELECT count(*)::bigint FROM (
                    SELECT pathname, count(*) AS obs_count
                    FROM project.state_discovery_artifacts
                    WHERE captured_at > now() - make_interval(days => $1)
                    GROUP BY pathname
                    HAVING count(*) >= $2
                ) o
                "#,
                &[&days, &min_obs],
            )
            .await
            .map_err(|e| format!("coverage observed-only query failed: {}", e))?;
        (Some(row.get::<_, i64>(0)), Vec::new())
    } else {
        (None, Vec::new())
    };

    let pages_specced: Option<i64> = if specs_exists {
        let row = client
            .query_one(
                "SELECT count(DISTINCT page_id)::bigint FROM project.cached_specs",
                &[],
            )
            .await
            .map_err(|e| format!("coverage specced query failed: {}", e))?;
        Some(row.get(0))
    } else {
        None
    };

    let coverage_pct: Option<f64> = match (pages_specced, pages_observed) {
        (Some(s), Some(o)) if o > 0 => Some((s as f64 / o as f64) * 100.0),
        _ => None,
    };

    // spec-multi-app Stream E.6: build the per-app section list for output.
    // With `--app`, the section list is just that one app; without it, we
    // list every registered app (output groups by `app_id`).
    let app_sections: Vec<String> = match scoped_label {
        Some(id) => vec![id.to_string()],
        None => list_app_ids(client).await.unwrap_or_default(),
    };
    // Fallback when no apps are registered yet: emit a single
    // legacy/aggregate section so the CLI still functions on a fresh DB.
    let app_sections = if app_sections.is_empty() {
        vec!["(aggregate)".to_string()]
    } else {
        app_sections
    };

    if args.format == "json" {
        let mut grouped = serde_json::Map::new();
        for app_id in &app_sections {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "pages_specced".to_string(),
                match pages_specced {
                    Some(n) => json!(n),
                    None => json!("unavailable"),
                },
            );
            obj.insert(
                "pages_observed".to_string(),
                match pages_observed {
                    Some(n) => json!(n),
                    None => json!("unavailable"),
                },
            );
            obj.insert(
                "coverage_pct".to_string(),
                match coverage_pct {
                    Some(p) => json!(round1(p)),
                    None => Value::Null,
                },
            );
            obj.insert("window_days".to_string(), json!(args.days));
            obj.insert("min_observations".to_string(), json!(args.min_observations));
            obj.insert("backlog".to_string(), json!(backlog));
            grouped.insert(app_id.clone(), Value::Object(obj));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Object(grouped)).unwrap()
        );
        return Ok(());
    }

    // Text format. One section per app.
    for app_id in &app_sections {
        println!("qontinui-specs coverage  [app={}]", app_id);
        println!(
            "  window: {}d, min-observations: {}",
            args.days, args.min_observations
        );
        println!();
        match pages_specced {
            Some(n) => println!("  pages_specced:   {:>6}", n),
            None => println!("  pages_specced:   unavailable (project.cached_specs not present)"),
        }
        match pages_observed {
            Some(n) => println!("  pages_observed:  {:>6}", n),
            None => println!(
                "  pages_observed:  unavailable (project.state_discovery_artifacts not present)"
            ),
        }
        match coverage_pct {
            Some(p) => println!("  coverage:        {:>5.1} %", p),
            None => println!("  coverage:        n/a"),
        }
        if !backlog.is_empty() {
            println!();
            println!("  Top {} backlog (observed but no spec):", backlog.len());
            for (i, path) in backlog.iter().enumerate() {
                println!("  {:>2}.  {}", i + 1, path);
            }
        }
        println!();
    }

    Ok(())
}

// ============================================================================
// `match-rate` subcommand
// ============================================================================

#[derive(Debug, Clone, serde::Serialize)]
struct MatchRatePageRow {
    page_id: Option<String>,
    runs: i64,
    avg_match_rate: Option<f64>,
    perfect: i64,
    partial: i64,
    none: i64,
    severity_hist: Option<Value>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct MatchRateStateRow {
    page_id: Option<String>,
    state_id: Option<String>,
    runs: i64,
    avg_match_rate: Option<f64>,
}

async fn run_match_rate(client: &Client, args: &MatchRateArgs) -> Result<(), String> {
    let days = args.days as i32;
    let min_rows = args.min_rows as i64;
    let page_id_filter: Option<&str> = args.page_id.as_deref();
    // spec-multi-app Stream E.6: app sectioning. `project.workflow_verification_phase_results`
    // does not yet carry `app_id`; Stream F's bootstrap migration will
    // backfill it. Until then we tag sections with the filter for
    // operator-facing clarity while leaving the numerics unchanged.
    let scoped_label = args.app.as_deref();
    let app_sections: Vec<String> = match scoped_label {
        Some(id) => vec![id.to_string()],
        None => {
            let mut all = list_app_ids(client).await.unwrap_or_default();
            if all.is_empty() {
                all.push("(aggregate)".to_string());
            }
            all
        }
    };

    if !table_exists(client, "project", "workflow_verification_phase_results").await? {
        return Err(
            "project.workflow_verification_phase_results is not present on this DB".to_string(),
        );
    }

    if args.by_state {
        let rows = client
            .query(
                r#"
                WITH unnested AS (
                    SELECT
                        result_json->'summary'->>'page_id' AS page_id,
                        state_result->>'state_id'          AS state_id,
                        (state_result->>'match_rate')::float AS match_rate
                    FROM project.workflow_verification_phase_results,
                         jsonb_array_elements(result_json->'state_results') AS state_result
                    WHERE created_at > now() - make_interval(days => $1)
                      AND ($2::text IS NULL OR result_json->'summary'->>'page_id' = $2)
                )
                SELECT
                    page_id,
                    state_id,
                    count(*)::bigint AS runs,
                    avg(match_rate)::float8 AS avg_rate
                FROM unnested
                GROUP BY page_id, state_id
                HAVING count(*) >= $3
                ORDER BY avg_rate ASC NULLS LAST
                "#,
                &[&days, &page_id_filter, &min_rows],
            )
            .await
            .map_err(|e| format!("match-rate by-state query failed: {}", e))?;

        let parsed: Vec<MatchRateStateRow> = rows
            .iter()
            .map(|r| MatchRateStateRow {
                page_id: r.get(0),
                state_id: r.get(1),
                runs: r.get(2),
                avg_match_rate: r.try_get::<_, f64>(3).ok(),
            })
            .collect();

        if args.format == "json" {
            // spec-multi-app Stream E.6: emit a `{app_id: [...]}` map even
            // when there is only one section so the json shape is stable.
            let mut grouped = serde_json::Map::new();
            for app_id in &app_sections {
                grouped.insert(app_id.clone(), serde_json::to_value(&parsed).unwrap());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&Value::Object(grouped)).map_err(|e| e.to_string())?
            );
            return Ok(());
        }

        // Section header per app.
        let first_app = app_sections.first().cloned().unwrap_or_default();
        println!(
            "qontinui-specs match-rate --by-state  [app={}]  (window: {}d, min-rows: {})",
            first_app, args.days, args.min_rows
        );
        if parsed.is_empty() {
            println!("  (no rows met the min-rows threshold in window)");
            return Ok(());
        }
        println!();
        println!(
            "  {:<32}  {:<24}  {:>5}  {:>10}",
            "page_id", "state_id", "runs", "avg_rate"
        );
        for r in &parsed {
            println!(
                "  {:<32}  {:<24}  {:>5}  {:>10}",
                r.page_id.as_deref().unwrap_or("<null>"),
                r.state_id.as_deref().unwrap_or("<null>"),
                r.runs,
                r.avg_match_rate
                    .map(|v| format!("{:.3}", v))
                    .unwrap_or_else(|| "n/a".to_string())
            );
        }
        return Ok(());
    }

    // Per-page query (no --by-state).
    let rows = client
        .query(
            r#"
            SELECT
                (result_json->'summary'->>'page_id') AS page_id,
                count(*)::bigint AS runs,
                avg((result_json->'summary'->>'overall_match_rate')::float)::float8 AS avg_match_rate,
                count(*) FILTER (WHERE result_json->'summary'->>'match_outcome' = 'PerfectMatch')::bigint AS perfect,
                count(*) FILTER (WHERE result_json->'summary'->>'match_outcome' = 'PartialMatch')::bigint AS partial_,
                count(*) FILTER (WHERE result_json->'summary'->>'match_outcome' = 'NoMatch')::bigint      AS none_,
                (SELECT jsonb_object_agg(k, v)
                 FROM jsonb_each(result_json->'summary'->'assertions_failed_by_severity')) AS severity_hist
            FROM project.workflow_verification_phase_results
            WHERE created_at > now() - make_interval(days => $1)
              AND result_json ? 'summary'
              AND ($2::text IS NULL OR result_json->'summary'->>'page_id' = $2)
            GROUP BY page_id
            HAVING count(*) >= $3
            ORDER BY avg_match_rate ASC NULLS LAST
            "#,
            &[&days, &page_id_filter, &min_rows],
        )
        .await
        .map_err(|e| format!("match-rate per-page query failed: {}", e))?;

    let parsed: Vec<MatchRatePageRow> = rows
        .iter()
        .map(|r| MatchRatePageRow {
            page_id: r.get(0),
            runs: r.get(1),
            avg_match_rate: r.try_get::<_, f64>(2).ok(),
            perfect: r.get(3),
            partial: r.get(4),
            none: r.get(5),
            severity_hist: r.try_get::<_, Value>(6).ok(),
        })
        .collect();

    if args.format == "json" {
        // spec-multi-app Stream E.6: `{app_id: [rows]}` map.
        let mut grouped = serde_json::Map::new();
        for app_id in &app_sections {
            grouped.insert(app_id.clone(), serde_json::to_value(&parsed).unwrap());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Object(grouped)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    let first_app = app_sections.first().cloned().unwrap_or_default();
    println!(
        "qontinui-specs match-rate  [app={}]  (window: {}d, min-rows: {})",
        first_app, args.days, args.min_rows
    );
    if let Some(p) = &args.page_id {
        println!("  filter: page_id = {}", p);
    }
    if parsed.is_empty() {
        println!("  (no rows met the min-rows threshold in window)");
        return Ok(());
    }
    println!();
    println!(
        "  {:<40}  {:>5}  {:>10}  {:>7}  {:>7}  {:>7}",
        "page_id", "runs", "avg_rate", "perfect", "partial", "none"
    );
    for r in &parsed {
        println!(
            "  {:<40}  {:>5}  {:>10}  {:>7}  {:>7}  {:>7}",
            r.page_id.as_deref().unwrap_or("<null>"),
            r.runs,
            r.avg_match_rate
                .map(|v| format!("{:.3}", v))
                .unwrap_or_else(|| "n/a".to_string()),
            r.perfect,
            r.partial,
            r.none,
        );
    }

    Ok(())
}

// ============================================================================
// `flywheel` subcommand
// ============================================================================

#[derive(Debug, Clone, serde::Serialize)]
struct ProposalEventRow {
    proposal_id: String,
    event_type: String,
    snapshot_id: Option<String>,
    failing_assertion_id: Option<String>,
    at: String,
    app_id: String,
}

async fn run_flywheel(client: &Client, args: &FlywheelArgs) -> Result<(), String> {
    let days = args.days as i32;
    let app_filter = args.app.as_deref();

    let proposals_table = table_exists(client, "project", "spec_proposals").await?;
    let events_table = table_exists(client, "project", "proposal_events").await?;

    // spec-multi-app Stream E.6: section list — one app or all registered apps.
    let app_sections: Vec<String> = match app_filter {
        Some(id) => vec![id.to_string()],
        None => {
            let mut all = list_app_ids(client).await.unwrap_or_default();
            if all.is_empty() {
                all.push("(aggregate)".to_string());
            }
            all
        }
    };

    // Five aggregates pulled in parallel. tokio::try_join! short-circuits on the
    // first error; we keep each helper independently fallible.
    let (
        queue_counts,
        recent_events,
        promoted_count,
        scanned_count,
        demoted_count,
        drift_demoted_count,
        drift_proposals_count,
    ) = tokio::try_join!(
        query_queue_counts(client, proposals_table),
        query_recent_events(client, events_table, app_filter),
        count_proposal_events_in_window(client, events_table, days, "promoted", app_filter),
        count_proposal_events_in_window(client, events_table, days, "scanned", app_filter),
        count_proposal_events_in_window(client, events_table, days, "demoted", app_filter),
        query_drift_event_count(client, events_table, days, "demoted", app_filter),
        query_drift_event_count(client, events_table, days, "scanned", app_filter),
    )?;

    let scan_rate = if scanned_count > 0 {
        promoted_count as f64 / scanned_count as f64
    } else {
        0.0
    };
    let demotion_rate = if promoted_count > 0 {
        Some(demoted_count as f64 / promoted_count as f64 * 100.0)
    } else {
        None
    };
    let drift_promotion_rate = if drift_proposals_count > 0 {
        let drift_promoted = drift_proposals_count.saturating_sub(drift_demoted_count);
        Some(drift_promoted as f64 / drift_proposals_count as f64 * 100.0)
    } else {
        None
    };

    if args.format == "json" {
        let mut queue_map = serde_json::Map::new();
        for status in &[
            "queued",
            "inFlight",
            "pendingValidation",
            "pendingPromotion",
            "promoted",
            "failed",
        ] {
            queue_map.insert(
                (*status).to_string(),
                json!(queue_counts.get(*status).copied().unwrap_or(0)),
            );
        }
        let events_json: Vec<Value> = recent_events
            .iter()
            .map(|e| {
                json!({
                    "at": e.at,
                    "event_type": e.event_type,
                    "proposal_id": e.proposal_id,
                    "snapshot_id": e.snapshot_id,
                    "failing_assertion_id": e.failing_assertion_id,
                    "app_id": e.app_id,
                })
            })
            .collect();
        let per_app = json!({
            "window_days": args.days,
            "queue": queue_map,
            "velocity": {
                "scanned": scanned_count,
                "promoted": promoted_count,
                "promotions_per_scan": round3(scan_rate),
            },
            "demotion": {
                "count": demoted_count,
                "rate_pct": demotion_rate.map(round1),
            },
            "drift": {
                "drift_triggered_proposals": drift_proposals_count,
                "drift_demoted": drift_demoted_count,
                "drift_fix_promotion_rate_pct": drift_promotion_rate.map(round1),
            },
            "recent_activity": events_json,
        });
        // spec-multi-app Stream E.6: `{app_id: { ... }}` map. With `--app`
        // the map has a single key; without it, the (currently aggregated)
        // numbers are replicated under every app_id key — replacing the
        // duplication with true per-app aggregates is queue-counts work
        // that depends on Stream F backfilling app_id on spec_proposals.
        let mut grouped = serde_json::Map::new();
        for app_id in &app_sections {
            grouped.insert(app_id.clone(), per_app.clone());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Object(grouped)).unwrap()
        );
        return Ok(());
    }

    // Text format — section header per app.
    let first_app = app_sections.first().cloned().unwrap_or_default();
    println!(
        "qontinui-specs flywheel  [app={}]  (window: {}d)",
        first_app, args.days
    );
    println!();
    println!("  Queue (status counts from spec_proposals):");
    let q = |s: &str| queue_counts.get(s).copied().unwrap_or(0);
    println!(
        "    queued: {:>6}    pendingValidation: {:>3}    promoted: {:>3}",
        q("queued"),
        q("pendingValidation"),
        q("promoted"),
    );
    println!(
        "    inFlight: {:>4}    pendingPromotion:  {:>3}    failed:   {:>3}",
        q("inFlight"),
        q("pendingPromotion"),
        q("failed"),
    );
    println!();
    println!("  Velocity (last {}d):", args.days);
    println!("    scans:        {:>5}", scanned_count);
    println!(
        "    promotions:   {:>5}  ({:.2} / scan)",
        promoted_count, scan_rate
    );
    match demotion_rate {
        Some(p) => println!(
            "    demotion rate: {:>4.1} %  ({} of {} promotions reverted)",
            p, demoted_count, promoted_count
        ),
        None => println!("    demotion rate:  n/a  (no promotions in window)"),
    }
    println!();
    println!("  Drift:");
    println!("    drift-triggered proposals: {}", drift_proposals_count);
    match drift_promotion_rate {
        Some(p) => println!("    drift-fix promotion rate:  {:.1} %", p),
        None => println!("    drift-fix promotion rate:  n/a (no drift-triggered proposals)"),
    }
    println!();
    println!("  Recent activity:");
    if recent_events.is_empty() {
        println!("    (none yet — flywheel sweep handler not running)");
    } else {
        for ev in &recent_events {
            let pid_short: String = ev.proposal_id.chars().take(8).collect();
            let asserttext = ev
                .failing_assertion_id
                .as_ref()
                .map(|a| format!("  [failing-assert: {}]", a))
                .unwrap_or_default();
            println!(
                "    {}  {:<9}  {}{}",
                ev.at,
                ev.event_type.to_uppercase(),
                pid_short,
                asserttext
            );
        }
    }

    Ok(())
}

// ----------------------------------------------------------------------------
// Flywheel helpers
// ----------------------------------------------------------------------------

async fn query_queue_counts(
    client: &Client,
    proposals_table_exists: bool,
) -> Result<std::collections::HashMap<String, i64>, String> {
    if !proposals_table_exists {
        return Ok(std::collections::HashMap::new());
    }
    let rows = client
        .query(
            "SELECT status, count(*)::bigint FROM project.spec_proposals GROUP BY status",
            &[],
        )
        .await
        .map_err(|e| format!("queue-counts query failed: {}", e))?;
    let mut out = std::collections::HashMap::new();
    for r in &rows {
        out.insert(r.get::<_, String>(0), r.get::<_, i64>(1));
    }
    Ok(out)
}

async fn query_recent_events(
    client: &Client,
    events_table_exists: bool,
    app_filter: Option<&str>,
) -> Result<Vec<ProposalEventRow>, String> {
    if !events_table_exists {
        return Ok(Vec::new());
    }
    // spec-multi-app Stream E.6: optional `--app` filter. The `app_id`
    // column is guaranteed present after the runtime self-heal in
    // `pg/mod.rs`. NULL-safe filter via `$1::text IS NULL OR ...`.
    let rows = client
        .query(
            r#"
            SELECT
                proposal_id,
                event_type,
                snapshot_id,
                failing_assertion_id,
                at::TEXT,
                app_id
            FROM proposal_events
            WHERE $1::text IS NULL OR app_id = $1
            ORDER BY at DESC, id
            LIMIT 20
            "#,
            &[&app_filter],
        )
        .await
        .map_err(|e| format!("recent-events query failed: {}", e))?;
    Ok(rows
        .iter()
        .map(|r| ProposalEventRow {
            proposal_id: r.get(0),
            event_type: r.get(1),
            snapshot_id: r.get(2),
            failing_assertion_id: r.get(3),
            at: r.get(4),
            app_id: r.get(5),
        })
        .collect())
}

async fn count_proposal_events_in_window(
    client: &Client,
    events_table_exists: bool,
    days: i32,
    event_type: &str,
    app_filter: Option<&str>,
) -> Result<i64, String> {
    if !events_table_exists {
        return Ok(0);
    }
    let row = client
        .query_one(
            r#"
            SELECT count(*)::bigint
            FROM proposal_events
            WHERE event_type = $2
              AND at > now() - make_interval(days => $1)
              AND ($3::text IS NULL OR app_id = $3)
            "#,
            &[&days, &event_type, &app_filter],
        )
        .await
        .map_err(|e| format!("count_proposal_events_in_window failed: {}", e))?;
    Ok(row.get(0))
}

/// Drift-related event count over the window: events of `event_type` where
/// `failing_assertion_id` is non-null (i.e. drift-fix proposals from
/// `spec_drift.rs`, as distinct from new-page proposal scans).
async fn query_drift_event_count(
    client: &Client,
    events_table_exists: bool,
    days: i32,
    event_type: &str,
    app_filter: Option<&str>,
) -> Result<i64, String> {
    if !events_table_exists {
        return Ok(0);
    }
    let row = client
        .query_one(
            r#"
            SELECT count(*)::bigint
            FROM proposal_events
            WHERE event_type = $2
              AND failing_assertion_id IS NOT NULL
              AND at > now() - make_interval(days => $1)
              AND ($3::text IS NULL OR app_id = $3)
            "#,
            &[&days, &event_type, &app_filter],
        )
        .await
        .map_err(|e| format!("drift event count query failed: {}", e))?;
    Ok(row.get(0))
}

/// List registered app_ids from `project.apps`. Returns an empty vec if the
/// table doesn't exist (legacy DB shape) or if no rows are registered. The
/// CLI's per-app output sections fall back to an `(aggregate)` synthetic
/// section in that case.
async fn list_app_ids(client: &Client) -> Result<Vec<String>, String> {
    if !table_exists(client, "project", "apps").await? {
        return Ok(Vec::new());
    }
    let rows = client
        .query("SELECT app_id FROM project.apps ORDER BY app_id", &[])
        .await
        .map_err(|e| format!("list_app_ids: {}", e))?;
    Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
}

// ============================================================================
// Shared helpers
// ============================================================================

/// `to_regclass($schema.$name)` IS NOT NULL — works without privilege to read
/// `information_schema` rows the caller didn't author.
async fn table_exists(client: &Client, schema: &str, name: &str) -> Result<bool, String> {
    let qualified = format!("{}.{}", schema, name);
    let row = client
        .query_one("SELECT to_regclass($1::text) IS NOT NULL", &[&qualified])
        .await
        .map_err(|e| format!("to_regclass({}) failed: {}", qualified, e))?;
    Ok(row.get(0))
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
