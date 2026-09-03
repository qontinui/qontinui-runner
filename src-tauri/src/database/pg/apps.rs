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

use std::path::Path;

use tracing::{debug, info, warn};

use qontinui_types::apps::{
    validate_app_id, validate_update_strategy, App, AppError, RegisterAppRequest, UpdateAppRequest,
};

use super::PgDb;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Resolve an incoming optional command into the `(touch, value)` pair the
/// UPDATE binds, applying the fleet-wide **blank-means-clear** contract.
///
/// Three states, which is one more than a bare `Option` can carry:
///
/// | Input | Returns | Meaning |
/// |---|---|---|
/// | `None` | `(false, None)` | absent — leave the column exactly as it is |
/// | `Some("")` / `Some("   ")` | `(true, None)` | **clear** the column |
/// | `Some("npm run build")` | `(true, Some(..))` | set it, trimmed |
///
/// Why blank means clear rather than being rejected: `qontinui-web` has shipped
/// that semantic since the fleet UI landed (it writes `project.apps` directly
/// via SQLAlchemy, `fleet_targets.py`), so this makes the runner's own API agree
/// with the contract already in production instead of adding a third one.
///
/// Why a blank must never be *stored*: the auto-fresh engine runs
/// `if let Some(cmd)` and checks only the exit status, and an empty shell
/// command exits 0 on every platform — so a stored `""` marks the app freshly
/// built having built nothing, and the `fresh_only` dispatcher then routes
/// tests to a host serving the previous artifact. Normalizing here is what
/// keeps that value out of the column.
fn normalize_command(value: Option<&str>) -> (bool, Option<String>) {
    match value {
        None => (false, None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                (true, None)
            } else {
                (true, Some(trimmed.to_string()))
            }
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
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
        // Normalize on the registration door too, so a blank command cannot
        // enter the table by either route (see `normalize_command`).
        let (_, insert_build_command) = normalize_command(req.build_command.as_deref());
        let (_, insert_start_command) = normalize_command(req.start_command.as_deref());
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
                    &insert_build_command,
                    &insert_start_command,
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
        //
        // The two COMMAND columns cannot use COALESCE, because it collapses the
        // three states this API has to express down to two. COALESCE falls
        // through only on SQL NULL, so `Some("")` would be *stored* rather than
        // clearing (verified: `COALESCE('', 'STORED')` → `''`) — and a stored
        // empty command is executed by the auto-fresh engine, exits 0, and marks
        // the app freshly built having built nothing. Each command therefore
        // binds a (touch, value) PAIR and a CASE: `$8`/`$10` say whether to
        // write at all, `$9`/`$11` carry the value. See `normalize_command`.
        let (touch_build, build_command) = normalize_command(req.build_command.as_deref());
        let (touch_start, start_command) = normalize_command(req.start_command.as_deref());
        let rows = conn
            .query(
                "UPDATE project.apps \
                 SET ui_bridge_url = COALESCE($2, ui_bridge_url), \
                     display_name  = COALESCE($3, display_name), \
                     auth_required  = COALESCE($4, auth_required), \
                     red_threshold  = COALESCE($5, red_threshold), \
                     yellow_threshold = COALESCE($6, yellow_threshold), \
                     update_strategy  = COALESCE($7, update_strategy), \
                     build_command    = CASE WHEN $8 THEN $9 ELSE build_command END, \
                     start_command    = CASE WHEN $10 THEN $11 ELSE start_command END \
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
                    &touch_build,
                    &build_command,
                    &touch_start,
                    &start_command,
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
/// Idempotent: an `AlreadyRegistered` error is swallowed at debug level so
/// the call is safe to invoke on every boot.
///
/// Paths come from the workspace root resolved by
/// [`crate::workspace_paths::workspace_root`]. They used to be resolved at
/// **compile time** from `CARGO_MANIFEST_DIR` and walked up two levels — which
/// baked the *build* machine's sibling-repo layout into the shipped binary, the
/// exact thing the "no-op unless `QONTINUI_DEV_BOOTSTRAP=1`" note above says we
/// do not want registered (plan
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 5
/// Phase 7 — class 2).
///
/// The `qontinui-web` and `qontinui-supervisor` siblings are registered ONLY
/// when their `frontend/` directories actually exist on disk — a partial
/// checkout is not an error.
pub async fn bootstrap_dev_apps(pg: &PgDb) -> Result<(), AppError> {
    if std::env::var("QONTINUI_DEV_BOOTSTRAP").unwrap_or_default() != "1" {
        return Ok(());
    }

    // Opt-in dev convenience, so an unresolved root is a skip, not an error: a
    // developer who set the flag on a machine with no discoverable workspace
    // gets a line saying why, and boot continues.
    let Some(qontinui_root) = crate::workspace_paths::workspace_root() else {
        info!(
            "bootstrap: QONTINUI_DEV_BOOTSTRAP=1 but no Qontinui workspace root \
             resolved, so no dev apps were registered. Set $QONTINUI_ROOT (or the \
             `paths.workspace_root` setting) to the directory holding the repo \
             checkouts."
        );
        return Ok(());
    };

    let runner_dir = qontinui_root.join("qontinui-runner");
    let runner_root = runner_dir
        .canonicalize()
        .map_err(|_| AppError::InvalidRepoRoot {
            repo_root: runner_dir.to_string_lossy().into_owned(),
        })?;

    for req in dev_app_registrations(&qontinui_root, &runner_root) {
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

/// The sibling apps registered alongside the runner itself, as
/// `(app_id + directory name, UI Bridge URL, display name)`. Each is registered
/// only when `<workspace-root>/<dir>/frontend/` exists.
const DEV_SIBLING_APPS: [(&str, &str, &str); 2] = [
    (
        "qontinui-web",
        "http://localhost:3001",
        "Qontinui Web (Next.js)",
    ),
    (
        "qontinui-supervisor",
        "http://localhost:9875",
        "Qontinui Supervisor (dashboard)",
    ),
];

/// Pure core of [`bootstrap_dev_apps`]'s path layout: which apps get registered
/// for a given workspace root and an already-canonicalized runner checkout.
///
/// The workspace root is injected rather than resolved here, so the layout rule
/// (`<workspace-root>/<app>/frontend`, and "absent sibling ⇒ omitted, not an
/// error") is unit-testable against a synthetic tree instead of the operator's
/// own sibling-repo layout — which is what the `CARGO_MANIFEST_DIR` version
/// baked in. Same wrapper/core split as
/// `agent_worktree::canonical_paths::default_canonical_path_in`.
///
/// The runner registers itself unconditionally: its checkout is the one that is
/// guaranteed to exist, having just been canonicalized by the caller.
fn dev_app_registrations(qontinui_root: &Path, runner_root: &Path) -> Vec<RegisterAppRequest> {
    let mut registrations = vec![RegisterAppRequest::new(
        "qontinui-runner",
        runner_root.to_string_lossy(),
        "http://localhost:9876",
        "Qontinui Runner (self)",
    )];

    for (app_id, ui_bridge_url, display_name) in DEV_SIBLING_APPS {
        if let Ok(root) = qontinui_root.join(app_id).join("frontend").canonicalize() {
            registrations.push(RegisterAppRequest::new(
                app_id,
                root.to_string_lossy(),
                ui_bridge_url,
                display_name,
            ));
        }
    }

    registrations
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

    /// An all-`None` `UpdateAppRequest` to mutate per test.
    ///
    /// `UpdateAppRequest` has no `Default` derive, and adding one to
    /// `qontinui-schemas` would pull the cross-repo consumer gate into a change
    /// that alters no wire format — so the builder lives here.
    fn empty_update() -> UpdateAppRequest {
        UpdateAppRequest {
            ui_bridge_url: None,
            display_name: None,
            auth_required: None,
            red_threshold: None,
            yellow_threshold: None,
            update_strategy: None,
            build_command: None,
            start_command: None,
        }
    }

    // ---- normalize_command (pure — runs in CI, unlike the DB-gated tests) ----

    #[test]
    fn normalize_command_absent_leaves_column_alone() {
        assert_eq!(normalize_command(None), (false, None));
    }

    #[test]
    fn normalize_command_blank_clears() {
        // The fleet-wide contract: blank means clear. qontinui-web has shipped
        // this semantic since the fleet UI landed.
        for blank in [
            "", "   ", "	", "
", " 	
 ",
        ] {
            assert_eq!(
                normalize_command(Some(blank)),
                (true, None),
                "{blank:?} must clear the column, not be stored"
            );
        }
    }

    #[test]
    fn normalize_command_sets_trimmed() {
        assert_eq!(
            normalize_command(Some("  npm run build  ")),
            (true, Some("npm run build".to_string()))
        );
    }

    #[test]
    fn normalize_command_never_yields_a_blank_value() {
        // The property that matters: no input can produce a STORED blank. A
        // stored blank is executed by the auto-fresh engine, exits 0, and marks
        // the app freshly built having built nothing.
        for input in [None, Some(""), Some("   "), Some(" x "), Some("npm start")] {
            let (_, value) = normalize_command(input);
            assert!(
                value.as_deref().map(str::trim).map(str::is_empty) != Some(true),
                "{input:?} produced a blank stored value"
            );
        }
    }

    /// DB-gated: prove all THREE update states against real SQL.
    ///
    /// The `CASE WHEN $n THEN ... ELSE column END` rewrite is exactly what a
    /// struct-level test cannot cover — and the previous `COALESCE` shape gets
    /// the "clear" case wrong while looking correct, because `COALESCE` falls
    /// through only on NULL and `''` is not NULL.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn update_app_command_leave_clear_and_set() {
        let pg = test_pg().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_id = unique_app_id("cmd");
        pg.insert_app(&register_request(&app_id, &tmp))
            .await
            .expect("register");

        // Set both commands.
        let set = UpdateAppRequest {
            update_strategy: Some("pull_build".to_string()),
            build_command: Some("npm run build".to_string()),
            start_command: Some("npm start".to_string()),
            ..empty_update()
        };
        let app = pg.update_app(&app_id, &set).await.expect("set");
        assert_eq!(app.build_command.as_deref(), Some("npm run build"));
        assert_eq!(app.start_command.as_deref(), Some("npm start"));

        // ABSENT → leave both alone.
        let leave = UpdateAppRequest {
            display_name: Some("Renamed".to_string()),
            ..empty_update()
        };
        let app = pg.update_app(&app_id, &leave).await.expect("leave");
        assert_eq!(app.display_name, "Renamed");
        assert_eq!(
            app.build_command.as_deref(),
            Some("npm run build"),
            "an absent field must not disturb the column"
        );

        // BLANK → clear, and clear INDEPENDENTLY of the sibling column.
        let clear_build = UpdateAppRequest {
            build_command: Some("   ".to_string()),
            ..empty_update()
        };
        let app = pg.update_app(&app_id, &clear_build).await.expect("clear");
        assert_eq!(
            app.build_command, None,
            "a blank must clear the column, never be stored"
        );
        assert_eq!(
            app.start_command.as_deref(),
            Some("npm start"),
            "clearing one command must not touch the other"
        );

        // And a set value round-trips trimmed.
        let retrim = UpdateAppRequest {
            build_command: Some("  make  ".to_string()),
            ..empty_update()
        };
        let app = pg.update_app(&app_id, &retrim).await.expect("re-set");
        assert_eq!(app.build_command.as_deref(), Some("make"));

        cleanup_app(&pg, &app_id).await;
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

    // -------------------------------------------------------------------------
    // Dev-bootstrap path layout (slice 5 Phase 7). No PG involved — these assert
    // the LAYOUT rule against a synthetic workspace root, so they hold on a
    // fresh checkout and on a non-operator machine.
    // -------------------------------------------------------------------------

    /// A synthetic workspace root with a runner checkout and, optionally, the
    /// sibling `frontend/` dirs. Never this machine's layout; pid + counter
    /// scoped because several worktrees run `cargo test` here concurrently, and
    /// cleaned up by `Drop` even when an assertion fails. Same shape as
    /// `workspace_paths::tests::Fixture`.
    struct BootstrapFixture {
        root: std::path::PathBuf,
    }

    impl Drop for BootstrapFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn bootstrap_fixture(siblings: &[&str]) -> BootstrapFixture {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "qontinui_dev_bootstrap_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("qontinui-runner")).unwrap();
        for sibling in siblings {
            std::fs::create_dir_all(root.join(sibling).join("frontend")).unwrap();
        }
        BootstrapFixture { root }
    }

    fn app_ids(reqs: &[RegisterAppRequest]) -> Vec<&str> {
        reqs.iter().map(|r| r.app_id.as_str()).collect()
    }

    /// The full layout: every sibling's `frontend/` exists under the workspace
    /// root, so all three apps register, and each repo root sits under the
    /// injected root — never under a compile-time path.
    #[test]
    fn dev_bootstrap_registers_every_sibling_whose_frontend_exists() {
        let f = bootstrap_fixture(&["qontinui-web", "qontinui-supervisor"]);
        let runner_root = f.root.join("qontinui-runner").canonicalize().unwrap();

        let reqs = dev_app_registrations(&f.root, &runner_root);

        assert_eq!(
            app_ids(&reqs),
            vec!["qontinui-runner", "qontinui-web", "qontinui-supervisor"]
        );
        let canonical_root = f.root.canonicalize().unwrap();
        for req in &reqs {
            assert!(
                Path::new(&req.repo_root).starts_with(&canonical_root),
                "every registered repo root must sit under the injected workspace root: {}",
                req.repo_root
            );
        }
    }

    /// A partial checkout is not an error: a sibling without a `frontend/` dir
    /// is simply omitted, and the runner still registers itself.
    #[test]
    fn dev_bootstrap_omits_siblings_without_a_frontend_dir() {
        let f = bootstrap_fixture(&["qontinui-web"]);
        let runner_root = f.root.join("qontinui-runner").canonicalize().unwrap();

        let reqs = dev_app_registrations(&f.root, &runner_root);

        assert_eq!(app_ids(&reqs), vec!["qontinui-runner", "qontinui-web"]);
    }

    /// The minimum: no siblings at all still registers the runner, whose
    /// checkout the caller has already canonicalized.
    #[test]
    fn dev_bootstrap_registers_the_runner_alone_when_no_siblings_exist() {
        let f = bootstrap_fixture(&[]);
        let runner_root = f.root.join("qontinui-runner").canonicalize().unwrap();

        let reqs = dev_app_registrations(&f.root, &runner_root);

        assert_eq!(app_ids(&reqs), vec!["qontinui-runner"]);
        assert_eq!(reqs[0].repo_root, runner_root.to_string_lossy());
        assert_eq!(reqs[0].ui_bridge_url, "http://localhost:9876");
    }
}
