//! PostgreSQL CRUD for `project.apps` — spec-multi-app Stream B registry.
//!
//! The table is authored declaratively in `atlas/schema.hcl` AND mirrored as
//! a `CREATE TABLE IF NOT EXISTS` self-heal in `pg/mod.rs::PgDb::new` so a
//! fresh PG without Atlas applied still boots. Query style follows
//! `pg/spec_proposals.rs` / `pg/proposal_events.rs` — direct `tokio_postgres`,
//! no Clorinde codegen.
//!
//! All public methods take a slug-style `app_id` validated by
//! `qontinui_types::apps::validate_app_id`. `insert_app` is transactional —
//! the slug + repo-root checks and the INSERT commit atomically.
//!
//! `touch_app` is best-effort: failures are logged at `warn!` but never
//! returned to callers, because the calling code path is a hot
//! `/apps/<app_id>/spec/*` read and a flaky `UPDATE` must not poison it.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use qontinui_types::apps::{
    validate_app_id, validate_update_strategy, App, AppError, RegisterAppRequest, UpdateAppRequest,
};

use crate::spec_api::storage::RUNNER_APP_ID;

use super::PgDb;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn row_to_app(r: &tokio_postgres::Row) -> App {
    let auth_required: Option<bool> = r.try_get(6).ok();
    let red_threshold: Option<f64> = r.try_get::<_, Option<f64>>(7).ok().flatten();
    let yellow_threshold: Option<f64> = r.try_get::<_, Option<f64>>(8).ok().flatten();

    App {
        app_id: r.get(0),
        repo_root: r.get(1),
        ui_bridge_url: r.get(2),
        display_name: r.get(3),
        created_at_ms: r.get(4),
        last_seen_at_ms: r.get(5),
        auth_required: auth_required.unwrap_or(false),
        red_threshold: red_threshold.unwrap_or(0.5),
        yellow_threshold: yellow_threshold.unwrap_or(0.8),
        // Columns added by P1a (update_strategy/build_command/start_command).
        // Option guards keep reads tolerant of a not-yet-healed DB mid-rollout.
        update_strategy: r
            .get::<_, Option<String>>(9)
            .unwrap_or_else(|| "pull_only".to_string()),
        build_command: r.get(10),
        start_command: r.get(11),
    }
}

impl PgDb {
    // -------------------------------------------------------------------------
    // Reads
    // -------------------------------------------------------------------------

    /// List all registered apps, ordered by most-recently-seen.
    pub async fn list_apps(&self) -> Result<Vec<App>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT app_id, repo_root, ui_bridge_url, display_name, \
                        created_at_ms, last_seen_at_ms, auth_required, red_threshold, \
                        yellow_threshold, update_strategy, build_command, start_command \
                 FROM project.apps \
                 ORDER BY last_seen_at_ms DESC, app_id",
                &[],
            )
            .await
            .map_err(|e| format!("PG list_apps: {}", e))?;
        Ok(rows.iter().map(row_to_app).collect())
    }

    /// Look up a single app by id. Returns `Ok(None)` if no row matches —
    /// callers translate that to `AppError::NotRegistered` so the user-facing
    /// reason is a kebab-case enum variant rather than a raw missing-row.
    pub async fn get_app(&self, app_id: &str) -> Result<Option<App>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT app_id, repo_root, ui_bridge_url, display_name, \
                        created_at_ms, last_seen_at_ms, auth_required, red_threshold, \
                        yellow_threshold, update_strategy, build_command, start_command \
                 FROM project.apps \
                 WHERE app_id = $1",
                &[&app_id],
            )
            .await
            .map_err(|e| format!("PG get_app: {}", e))?;
        Ok(rows.first().map(row_to_app))
    }

    // -------------------------------------------------------------------------
    // Writes
    // -------------------------------------------------------------------------

    /// Register a new app. Wraps slug validation + repo-root existence check
    /// + INSERT in a single transaction so a racing duplicate INSERT cannot
    /// land between the SELECT and the INSERT.
    ///
    /// Rejections:
    /// - `AppError::InvalidAppId` if `req.app_id` is not a valid slug.
    /// - `AppError::InvalidRepoRoot` if `req.repo_root` is missing or is not
    ///   a directory at the time of registration.
    /// - `AppError::AlreadyRegistered` if a row with `req.app_id` exists.
    pub async fn insert_app(&self, req: &RegisterAppRequest) -> Result<App, AppError> {
        validate_app_id(&req.app_id)?;
        validate_update_strategy(&req.update_strategy)?;

        if !Path::new(&req.repo_root).is_dir() {
            return Err(AppError::InvalidRepoRoot {
                repo_root: req.repo_root.clone(),
            });
        }

        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::InvalidRepoRoot {
                // Surface PG pool failures through InvalidRepoRoot so the HTTP
                // layer's `IntoResponse` mapping reports a 500. There is no
                // "internal error" variant in `AppError` (kept narrow on purpose).
                // The error message is captured in the `repo_root` field so the
                // detail is visible in logs without expanding the enum surface.
                repo_root: format!("(pg pool error) {}", e),
            })?;

        let txn = conn
            .transaction()
            .await
            .map_err(|e| AppError::InvalidRepoRoot {
                repo_root: format!("(pg transaction error) {}", e),
            })?;

        // Re-check inside the transaction so a racing INSERT loses to the
        // serializable PRIMARY KEY constraint below rather than slipping
        // through a stale read.
        let existing = txn
            .query_opt(
                "SELECT 1 FROM project.apps WHERE app_id = $1",
                &[&req.app_id],
            )
            .await
            .map_err(|e| AppError::InvalidRepoRoot {
                repo_root: format!("(pg pre-insert select error) {}", e),
            })?;
        if existing.is_some() {
            return Err(AppError::AlreadyRegistered {
                app_id: req.app_id.clone(),
            });
        }

        let now = now_ms();
        let rows = txn
            .query(
                "INSERT INTO project.apps \
                     (app_id, repo_root, ui_bridge_url, display_name, \
                      created_at_ms, last_seen_at_ms, auth_required, red_threshold, \
                      yellow_threshold, update_strategy, build_command, start_command) \
                 VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8, $9, $10, $11) \
                 RETURNING app_id, repo_root, ui_bridge_url, display_name, \
                           created_at_ms, last_seen_at_ms, auth_required, red_threshold, \
                           yellow_threshold, update_strategy, build_command, start_command",
                &[
                    &req.app_id,
                    &req.repo_root,
                    &req.ui_bridge_url,
                    &req.display_name,
                    &now,
                    &req.auth_required,
                    &req.red_threshold,
                    &req.yellow_threshold,
                    &req.update_strategy,
                    &req.build_command,
                    &req.start_command,
                ],
            )
            .await
            .map_err(|e| {
                // Translate the unique-violation SQLSTATE 23505 into the
                // structured `AlreadyRegistered` variant; everything else
                // surfaces as InvalidRepoRoot (the catch-all in this enum).
                if let Some(db_err) = e.as_db_error() {
                    if db_err.code().code() == "23505" {
                        return AppError::AlreadyRegistered {
                            app_id: req.app_id.clone(),
                        };
                    }
                }
                AppError::InvalidRepoRoot {
                    repo_root: format!("(pg insert error) {}", e),
                }
            })?;

        txn.commit().await.map_err(|e| AppError::InvalidRepoRoot {
            repo_root: format!("(pg commit error) {}", e),
        })?;

        let row = rows.first().ok_or_else(|| AppError::InvalidRepoRoot {
            repo_root: "(insert returned no rows)".into(),
        })?;
        Ok(row_to_app(row))
    }

    /// Patch mutable fields on an existing app. Fields on `UpdateAppRequest`
    /// are optional — `None` means "leave unchanged". Returns the updated row.
    /// Errors with `AppError::NotRegistered` if no row exists for `app_id`.
    pub async fn update_app(&self, app_id: &str, req: &UpdateAppRequest) -> Result<App, AppError> {
        if let Some(ref strategy) = req.update_strategy {
            validate_update_strategy(strategy)?;
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| AppError::InvalidRepoRoot {
                repo_root: format!("(pg pool error) {}", e),
            })?;

        // COALESCE($n, column) → leave the column untouched when the param is
        // NULL. Single round-trip, no SELECT-then-UPDATE race.
        let rows = conn
            .query(
                "UPDATE project.apps \
                 SET ui_bridge_url = COALESCE($2, ui_bridge_url), \
                     display_name  = COALESCE($3, display_name), \
                     auth_required  = COALESCE($4, auth_required), \
                     red_threshold  = COALESCE($5, red_threshold), \
                     yellow_threshold = COALESCE($6, yellow_threshold), \
                     update_strategy  = COALESCE($7, update_strategy), \
                     build_command    = COALESCE($8, build_command), \
                     start_command    = COALESCE($9, start_command) \
                 WHERE app_id = $1 \
                 RETURNING app_id, repo_root, ui_bridge_url, display_name, \
                           created_at_ms, last_seen_at_ms, auth_required, red_threshold, \
                           yellow_threshold, update_strategy, build_command, start_command",
                &[
                    &app_id,
                    &req.ui_bridge_url,
                    &req.display_name,
                    &req.auth_required,
                    &req.red_threshold,
                    &req.yellow_threshold,
                    &req.update_strategy,
                    &req.build_command,
                    &req.start_command,
                ],
            )
            .await
            .map_err(|e| AppError::InvalidRepoRoot {
                repo_root: format!("(pg update error) {}", e),
            })?;

        match rows.first() {
            Some(r) => Ok(row_to_app(r)),
            None => Err(AppError::NotRegistered {
                app_id: app_id.into(),
            }),
        }
    }

    /// Remove an app row. Returns `Ok(true)` if a row was deleted,
    /// `Ok(false)` if no row matched. Files on disk under the app's
    /// `repo_root` are NEVER touched — the registry is the only persisted
    /// state owned by the runner.
    pub async fn delete_app(&self, app_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let n = conn
            .execute("DELETE FROM project.apps WHERE app_id = $1", &[&app_id])
            .await
            .map_err(|e| format!("PG delete_app: {}", e))?;
        Ok(n > 0)
    }

    /// Bump `last_seen_at_ms` to `now()`. Best-effort: failures are logged
    /// at `warn!` and swallowed — the storage hot path must not fail because
    /// of a missed bookkeeping write. No-op (zero rows updated) when
    /// `app_id` is unknown.
    pub async fn touch_app(&self, app_id: &str) {
        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                warn!(%app_id, ?e, "touch_app: PG pool error (swallowed)");
                return;
            }
        };
        let now = now_ms();
        if let Err(e) = conn
            .execute(
                "UPDATE project.apps SET last_seen_at_ms = $2 WHERE app_id = $1",
                &[&app_id, &now],
            )
            .await
        {
            warn!(%app_id, ?e, "touch_app: UPDATE failed (swallowed)");
        }
    }
}

// -----------------------------------------------------------------------------
// Self-registration — the runner's OWN identity row (unconditional)
// -----------------------------------------------------------------------------

/// The `repo_root` this runner should register for itself.
///
/// `repo_root` is `NOT NULL` and [`PgDb::insert_app`] additionally requires it
/// to be an existing directory, so this must return a real path — but it must
/// **never fabricate a developer's directory layout** (that is exactly why
/// [`bootstrap_dev_apps`] is env-gated: it hardcodes *sibling*-repo paths for
/// apps other than this one). Two candidates, both verified on disk before use:
///
/// 1. **The checkout this binary was built from** (`CARGO_MANIFEST_DIR/..`,
///    i.e. the `qontinui-runner` repo root), used **only when it still exists
///    on this machine** — true on a developer box, false on any production
///    install, where the build path does not exist. It is checked, not assumed.
///    Preferring it matters for correctness, not cosmetics: `bootstrap_dev_apps`
///    registers this same `app_id` with exactly this value, and whichever writer
///    lands first owns the row forever (the loser's `AlreadyRegistered` is
///    swallowed). Self-registration runs first (PG pool init), so returning a
///    *different* root here would pin a weaker `repo_root` on a fresh dev
///    database and break `resolve_specs_root` for the runner's own specs.
/// 2. Otherwise **the directory this executable runs from** — the only root a
///    production runner genuinely knows about itself. `<that>/specs` will not
///    normally exist, so spec reads for this app stay in exactly the state they
///    are in today on such a box (an error/embedded-fallback), which is no worse
///    than the un-registered status quo. The row's *existence* is the load-
///    bearing part: without it every observation's `app_id` subquery resolves to
///    NULL and attribution is silently lost.
///
/// Returns `None` when neither candidate resolves, in which case
/// [`ensure_self_registered`] does nothing rather than inventing a path.
fn self_repo_root() -> Option<PathBuf> {
    // 1. The runner checkout this binary was compiled from, IF it is present
    //    on this machine. `canonicalize` is the existence check.
    if let Ok(built_from) = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
    {
        return Some(built_from);
    }
    // 2. The install directory of the running executable.
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .filter(|dir| dir.is_dir())
}

/// Ensure `project.apps` carries **this runner's own identity row**, on every
/// startup, on every database, independent of `QONTINUI_DEV_BOOTSTRAP`.
///
/// Why this is not optional. `state_discovery::capture` validates an
/// observation's `app_id` inside its INSERT with
/// `(SELECT a.app_id FROM project.apps a WHERE a.app_id = $6)`. If no row for
/// `qontinui-runner` exists, that subquery yields NULL and **every observation
/// is written un-attributed, permanently** — there is no FK and no error, so
/// nothing else catches it. Before this, the only writer of that row was
/// [`bootstrap_dev_apps`], which returns early unless `QONTINUI_DEV_BOOTSTRAP=1`
/// — a variable only `dev-start.ps1` sets. Production installs,
/// supervisor-spawned temp runners that do not inherit the env, and CI runners
/// therefore had no row at all, and the whole app-scoping feature was a silent
/// no-op there.
///
/// This is the same class of self-heal the runner already performs for the table
/// itself (`CREATE TABLE IF NOT EXISTS project.apps` at PG pool init): the
/// runner is the canonical author of `project.apps`, so registering its own
/// identity in it belongs to the same owner.
///
/// **Not a widening of [`bootstrap_dev_apps`]**, which stays exactly as it is:
/// that function registers *three* apps — including two siblings whose paths are
/// a developer-layout guess — and is correctly dev-only. This registers one app,
/// the one whose values this process actually knows: its own.
///
/// Idempotent by construction: `insert_app`'s `AlreadyRegistered` is the
/// expected outcome from the second boot onward, and it is answered with
/// [`PgDb::touch_app`] so `last_seen_at_ms` still advances. Nothing on an
/// existing row is overwritten — a row authored by the dev bootstrap, or edited
/// by an operator through `PATCH /apps/<id>`, survives untouched.
///
/// Failure never breaks startup: every arm logs and returns, matching the
/// surrounding self-heal error handling.
pub async fn ensure_self_registered(pg: &PgDb) {
    let repo_root = match self_repo_root() {
        Some(p) => p,
        None => {
            warn!(
                app_id = RUNNER_APP_ID,
                "self-registration skipped: could not resolve any real directory for repo_root. \
                 Observations will be recorded with app_id NULL (un-attributed) until this app \
                 is registered."
            );
            return;
        }
    };

    // The port this runner is *configured* to serve its UI Bridge on
    // (`QONTINUI_PORT`, default 9876). Read here rather than from
    // `AppState.api_port` because self-registration runs at PG pool init, before
    // the HTTP server binds; on the rare fallback bind (configured port already
    // taken) the stored URL is the configured one, which is why an existing row
    // is never overwritten and an operator can correct it via `PATCH /apps/<id>`.
    // `127.0.0.1`, not `localhost`: the runner binds the IPv4 loopback only, and
    // Windows resolves `localhost` to `::1` first, paying a doomed connect.
    let ui_bridge_url = format!("http://127.0.0.1:{}", crate::mcp::types::get_mcp_api_port());

    // Same name `/health` already reports as `uiBridge.appName`.
    let req = RegisterAppRequest::new(
        RUNNER_APP_ID,
        repo_root.to_string_lossy(),
        ui_bridge_url,
        "Qontinui Runner",
    );

    match pg.insert_app(&req).await {
        Ok(_) => info!(
            app_id = %req.app_id,
            repo_root = %req.repo_root,
            ui_bridge_url = %req.ui_bridge_url,
            "self-registration: registered this runner in project.apps"
        ),
        Err(AppError::AlreadyRegistered { .. }) => {
            // Steady state from the second boot onward. Bump `last_seen_at_ms`
            // (best-effort, swallows its own errors) and leave every other
            // column exactly as its author wrote it.
            debug!(app_id = %req.app_id, "self-registration: already registered (idempotent)");
            pg.touch_app(&req.app_id).await;
        }
        Err(e) => warn!(
            app_id = %req.app_id,
            repo_root = %req.repo_root,
            ?e,
            "self-registration FAILED (non-fatal) — observations from this runner will be \
             recorded with app_id NULL (un-attributed) until it succeeds"
        ),
    }
}

// -----------------------------------------------------------------------------
// Dev bootstrap (spec-multi-app Stream F.1)
// -----------------------------------------------------------------------------

/// Register the three known dev apps (`qontinui-runner`, `qontinui-web`,
/// `qontinui-supervisor`) at runner startup so the multi-tenant Spec API has
/// something to serve out of the box.
///
/// **No-op unless `QONTINUI_DEV_BOOTSTRAP=1`** — production runners reach this
/// path too, and we do not want them auto-registering the developer's
/// hard-coded sibling-repo layout.
///
/// This gate is deliberately kept: it is no longer the only writer of the
/// `qontinui-runner` row. [`ensure_self_registered`] registers *that one app*
/// unconditionally at PG pool init, so app attribution for observations works on
/// production/CI/temp runners; this function still owns the two sibling apps,
/// whose paths only a developer checkout can supply.
///
/// Idempotent: an `AlreadyRegistered` error is swallowed at debug level so
/// the call is safe to invoke on every boot.
///
/// Paths are resolved at compile time via `CARGO_MANIFEST_DIR` (the runner's
/// `src-tauri/` dir), then walked up to the qontinui-root layout. The
/// `qontinui-web` and `qontinui-supervisor` siblings are registered ONLY when
/// their `frontend/` directories actually exist on disk — a partial checkout
/// is not an error.
pub async fn bootstrap_dev_apps(pg: &PgDb) -> Result<(), AppError> {
    if std::env::var("QONTINUI_DEV_BOOTSTRAP").unwrap_or_default() != "1" {
        return Ok(());
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // <manifest_dir> = .../qontinui-runner/src-tauri
    // runner_root    = .../qontinui-runner
    let runner_root = PathBuf::from(manifest_dir)
        .join("..")
        .canonicalize()
        .map_err(|_| AppError::InvalidRepoRoot {
            repo_root: manifest_dir.into(),
        })?;

    let qontinui_root = runner_root.join("..");
    let web_root = qontinui_root
        .join("qontinui-web")
        .join("frontend")
        .canonicalize()
        .ok();
    let supervisor_root = qontinui_root
        .join("qontinui-supervisor")
        .join("frontend")
        .canonicalize()
        .ok();

    let mut registrations = vec![RegisterAppRequest::new(
        // Same constant `ensure_self_registered` uses — one source of truth for
        // this id, never a second literal.
        RUNNER_APP_ID,
        runner_root.to_string_lossy(),
        "http://localhost:9876",
        "Qontinui Runner (self)",
    )];

    if let Some(root) = web_root {
        registrations.push(RegisterAppRequest::new(
            "qontinui-web",
            root.to_string_lossy(),
            "http://localhost:3001",
            "Qontinui Web (Next.js)",
        ));
    }

    if let Some(root) = supervisor_root {
        registrations.push(RegisterAppRequest::new(
            "qontinui-supervisor",
            root.to_string_lossy(),
            "http://localhost:9875",
            "Qontinui Supervisor (dashboard)",
        ));
    }

    for req in registrations {
        match pg.insert_app(&req).await {
            Ok(_) => {
                info!(app_id = %req.app_id, repo_root = %req.repo_root, "bootstrap: registered app")
            }
            Err(AppError::AlreadyRegistered { .. }) => {
                debug!(app_id = %req.app_id, "bootstrap: app already registered (idempotent)");
            }
            Err(e) => warn!(app_id = %req.app_id, ?e, "bootstrap: registration failed (non-fatal)"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Live-PG integration tests for spec-multi-app Stream B.
    //!
    //! Tests sit inside the bin crate (rather than `tests/app_registry.rs`)
    //! because the `database::pg` module is bin-private — the lib crate
    //! exposes only `accessibility`, `auth`, `secure_storage`, etc. The
    //! `#[cfg(test)] mod tests {}` pattern matches every other PG CRUD
    //! module (e.g. `pg/session_touched_files.rs`).
    //!
    //! Run locally with a running PG:
    //!   DATABASE_URL=postgres://qontinui_user:PASSWORD@localhost:5433/qontinui_db \
    //!     cargo test --bin qontinui-runner -- --ignored apps:: --test-threads=1
    //!
    //! `--test-threads=1` keeps the registry mutations serial. Per-test
    //! unique slugs (helper `unique_app_id`) prevent rows from colliding
    //! when reruns leave stale state; each test calls `cleanup_app` at the
    //! end to keep the shared dev DB tidy.

    use super::*;
    use crate::spec_api::storage;

    /// Async PgDb constructor for tests. The sibling `PgDb::new_blocking_for_test()`
    /// helper used by other `pg/*` test modules calls `rt.block_on()` which
    /// panics under `#[tokio::test]`'s active runtime — known pre-existing
    /// bug across the codebase. This async-native helper sidesteps the
    /// double-runtime conflict.
    async fn test_pg() -> PgDb {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://qontinui_user:qontinui_password@localhost:5433/qontinui_db".to_string()
        });
        PgDb::new(&url)
            .await
            .expect("PgDb::new for app_registry tests")
    }

    fn unique_app_id(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tid = format!("{:?}", std::thread::current().id())
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        let raw = format!("ar-{}-{}-{}", label, nanos, tid);
        raw.chars().take(64).collect()
    }

    fn register_request(app_id: &str, tmp: &tempfile::TempDir) -> RegisterAppRequest {
        RegisterAppRequest::new(
            app_id,
            tmp.path().to_string_lossy(),
            format!("http://localhost:3001/{}", app_id),
            format!("Test App {}", app_id),
        )
    }

    async fn cleanup_app(pg: &PgDb, app_id: &str) {
        let _ = pg.delete_app(app_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn list_apps_empty_returns_empty_vec() {
        // Shared dev DB: assert shape (Vec<App>, no error) rather than zero
        // rows — other tests may have rows in flight on the same DB.
        let pg = test_pg().await;
        let apps = pg.list_apps().await.expect("list_apps must not error");
        assert!(apps.iter().all(|a| !a.app_id.is_empty()));
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn register_app_with_invalid_slug_returns_invalid_app_id() {
        let pg = test_pg().await;
        let tmp = tempfile::tempdir().unwrap();
        let mut req = register_request("ignored", &tmp);
        req.app_id = "Has_Underscore_And_Caps".into();
        let err = pg
            .insert_app(&req)
            .await
            .expect_err("invalid slug must reject");
        assert!(
            matches!(err, AppError::InvalidAppId { .. }),
            "expected InvalidAppId; got {:?}",
            err
        );
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn register_app_with_missing_repo_root_returns_invalid_repo_root() {
        let pg = test_pg().await;
        let app_id = unique_app_id("missing-root");
        let bogus_root = format!(
            "{}/does/not/exist/{}",
            std::env::temp_dir().display(),
            app_id
        );
        let req =
            RegisterAppRequest::new(app_id.clone(), bogus_root, "http://localhost:3001", "Test");
        let err = pg
            .insert_app(&req)
            .await
            .expect_err("missing repo_root must reject");
        assert!(
            matches!(err, AppError::InvalidRepoRoot { .. }),
            "expected InvalidRepoRoot; got {:?}",
            err
        );
        cleanup_app(&pg, &app_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn register_app_then_get_app_round_trips() {
        let pg = test_pg().await;
        let app_id = unique_app_id("round-trip");
        let tmp = tempfile::tempdir().unwrap();
        let req = register_request(&app_id, &tmp);

        let inserted = pg.insert_app(&req).await.expect("insert_app");
        assert_eq!(inserted.app_id, app_id);
        assert_eq!(inserted.repo_root, req.repo_root);
        assert_eq!(inserted.ui_bridge_url, req.ui_bridge_url);
        assert_eq!(inserted.display_name, req.display_name);
        assert!(inserted.created_at_ms > 0);
        assert_eq!(
            inserted.created_at_ms, inserted.last_seen_at_ms,
            "fresh insert should have last_seen == created"
        );

        let read = pg
            .get_app(&app_id)
            .await
            .expect("get_app")
            .expect("row must exist");
        assert_eq!(read.app_id, app_id);
        assert_eq!(read.repo_root, req.repo_root);

        cleanup_app(&pg, &app_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn register_app_duplicate_returns_already_registered() {
        let pg = test_pg().await;
        let app_id = unique_app_id("dup");
        let tmp = tempfile::tempdir().unwrap();
        let req = register_request(&app_id, &tmp);

        pg.insert_app(&req).await.expect("first insert succeeds");
        let err = pg
            .insert_app(&req)
            .await
            .expect_err("second insert must fail");
        assert!(
            matches!(err, AppError::AlreadyRegistered { .. }),
            "expected AlreadyRegistered; got {:?}",
            err
        );

        cleanup_app(&pg, &app_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn update_app_changes_ui_bridge_url() {
        let pg = test_pg().await;
        let app_id = unique_app_id("update");
        let tmp = tempfile::tempdir().unwrap();
        let req = register_request(&app_id, &tmp);
        pg.insert_app(&req).await.expect("insert");

        let new_url = "http://localhost:9999/updated".to_string();
        let patch = UpdateAppRequest {
            ui_bridge_url: Some(new_url.clone()),
            display_name: None,
            auth_required: None,
            red_threshold: None,
            yellow_threshold: None,
            update_strategy: None,
            build_command: None,
            start_command: None,
        };
        let updated = pg.update_app(&app_id, &patch).await.expect("update_app");
        assert_eq!(updated.ui_bridge_url, new_url);
        // display_name must be untouched (COALESCE behavior).
        assert_eq!(updated.display_name, req.display_name);

        // Re-read to confirm persistence.
        let read = pg
            .get_app(&app_id)
            .await
            .expect("get_app")
            .expect("present");
        assert_eq!(read.ui_bridge_url, new_url);

        cleanup_app(&pg, &app_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn update_app_changes_auth_required_and_thresholds() {
        let pg = test_pg().await;
        let app_id = unique_app_id("update-auth");
        let tmp = tempfile::tempdir().unwrap();
        let req = register_request(&app_id, &tmp);
        let inserted = pg.insert_app(&req).await.expect("insert");
        assert!(!inserted.auth_required);
        assert_eq!(inserted.red_threshold, 0.5);
        assert_eq!(inserted.yellow_threshold, 0.8);

        let patch = UpdateAppRequest {
            ui_bridge_url: None,
            display_name: None,
            auth_required: Some(true),
            red_threshold: Some(0.55),
            yellow_threshold: Some(0.85),
            update_strategy: None,
            build_command: None,
            start_command: None,
        };
        let updated = pg.update_app(&app_id, &patch).await.expect("update_app");
        assert!(updated.auth_required);
        assert_eq!(updated.red_threshold, 0.55);
        assert_eq!(updated.yellow_threshold, 0.85);

        cleanup_app(&pg, &app_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn delete_app_removes_row_does_not_touch_disk() {
        let pg = test_pg().await;
        let app_id = unique_app_id("delete");
        let tmp = tempfile::tempdir().unwrap();
        let req = register_request(&app_id, &tmp);
        pg.insert_app(&req).await.expect("insert");

        // Sentinel file under the repo root — delete_app must NOT remove it.
        let sentinel = tmp.path().join("sentinel.txt");
        std::fs::write(&sentinel, b"do-not-touch").unwrap();

        let deleted = pg.delete_app(&app_id).await.expect("delete_app");
        assert!(deleted, "delete_app should return true on a hit");

        // Row gone from the registry.
        let read = pg.get_app(&app_id).await.expect("get_app");
        assert!(read.is_none(), "row must be absent after delete_app");

        // Sentinel file untouched — delete_app NEVER mutates repo_root contents.
        assert!(sentinel.exists(), "delete_app must not touch on-disk files");

        // Second delete returns false (no-op idempotent).
        let again = pg.delete_app(&app_id).await.expect("delete_app idempotent");
        assert!(!again, "delete_app on absent row should return false");
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn touch_app_bumps_last_seen_at_ms() {
        let pg = test_pg().await;
        let app_id = unique_app_id("touch");
        let tmp = tempfile::tempdir().unwrap();
        let req = register_request(&app_id, &tmp);
        let inserted = pg.insert_app(&req).await.expect("insert");

        // Sleep so the millisecond clock advances past `created_at_ms`.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        pg.touch_app(&app_id).await; // best-effort, no error surface.

        let read = pg
            .get_app(&app_id)
            .await
            .expect("get_app")
            .expect("present");
        assert!(
            read.last_seen_at_ms > inserted.last_seen_at_ms,
            "touch_app must bump last_seen_at_ms; before={}, after={}",
            inserted.last_seen_at_ms,
            read.last_seen_at_ms
        );

        cleanup_app(&pg, &app_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn resolve_specs_root_returns_repo_root_plus_specs() {
        let pg = test_pg().await;
        let app_id = unique_app_id("resolve-ok");
        let tmp = tempfile::tempdir().unwrap();

        // The resolver insists that `<repo_root>/specs` is a directory.
        let specs_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&specs_dir).unwrap();

        let req = register_request(&app_id, &tmp);
        pg.insert_app(&req).await.expect("insert");

        let resolved = storage::resolve_specs_root(&pg, &app_id)
            .await
            .expect("resolve_specs_root must succeed for registered app");
        // Canonicalize both sides — tempfile may hand back symlinked /var/folders/
        // paths on macOS that fail a naive `==`.
        let expected = specs_dir.canonicalize().unwrap_or(specs_dir);
        let got = resolved.canonicalize().unwrap_or(resolved);
        assert_eq!(got, expected);

        cleanup_app(&pg, &app_id).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn resolve_specs_root_for_unknown_app_returns_not_registered() {
        let pg = test_pg().await;
        let ghost_id = unique_app_id("ghost");
        let err = storage::resolve_specs_root(&pg, &ghost_id)
            .await
            .expect_err("unknown app must error");
        match err {
            AppError::NotRegistered { app_id } => assert_eq!(app_id, ghost_id),
            other => panic!("expected NotRegistered; got {:?}", other),
        }
    }
}
