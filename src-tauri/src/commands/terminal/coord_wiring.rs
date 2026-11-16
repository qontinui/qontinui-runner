//! The post-create coord wiring every terminal create door shares.
//!
//! Plan `2026-11-05-remote-session-end-reaches-the-target-but-cannot-end-a-real-session`,
//! Phase 4. A pane is only addressable by coord — and so only ENDable from
//! another device — once its `terminal_shell` row exists and its coord session
//! id is bound onto the [`crate::terminal::session::TerminalSession`]
//! (`GET /terminals/{id}/coord-session` reads exactly that binding; a remote
//! end's attach-grant mint is keyed by it). That wiring used to live inline in
//! the `terminal_create` Tauri command, and the two HTTP create doors — the
//! invoke proxy's `terminal_create` arm (`mcp/tauri_proxy.rs`) and
//! `POST /terminals` (`mcp/terminals.rs`) — called `TerminalManager::create`
//! and returned, so every pane they made had no coord row and a remote end
//! of it answered `not_found`. There was never a gate behind that: it was a
//! copy that stopped short. Every create door now calls
//! [`finish_terminal_create`] (or, for the backend gate-continuation spawn,
//! its binding half [`bind_registered_coord_session`]), so they cannot drift apart again.
//!
//! # What is shared, and what stays per door
//!
//! [`wire_coord_session`] — the part that needs no webview and no
//! `AppHandle`, which is why it takes the managers directly and is unit
//! testable: the pane-store resume or the fresh `register_external*`, the
//! pane-store put, [`TerminalManager::bind_coord_session`], the on-exit coord
//! close hook, and the output pipe.
//!
//! [`finish_terminal_create`] — the above, plus parking the isolated edit
//! context, the repo-detection emit and the git credential helper. Repo
//! detection is the only step that needs an `AppHandle` (it emits an event to
//! the webview); every HTTP door holds one (`ApiState.app_handle`), so all of
//! them get it. With no handle (a unit test) it is skipped. Nothing here needs
//! a webview WINDOW — an emit with no window listening is a no-op.
//!
//! Webview-only, and deliberately NOT here: the pane-key identity
//! ([`CoordRegistration::PaneKeyed`]). Its key is the create-time triple the
//! frontend round-trips on restore (`page_id`, `title`, the original
//! `working_dir`, the window label), so only the Tauri command can honestly
//! claim one. An HTTP door has no window and no restore round-trip, and two
//! headless creates with the same title and cwd would collapse onto one coord
//! row; those doors register fresh ([`CoordRegistration::FreshPinned`]).
//!
//! # Every create door, and how it stands
//!
//! | Door | Calls |
//! |---|---|
//! | `terminal_create` Tauri command | [`finish_terminal_create`], `PaneKeyed` |
//! | invoke proxy `terminal_create` arm | [`finish_terminal_create`], `FreshPinned` |
//! | `POST /terminals` | [`finish_terminal_create`], `FreshPinned` |
//! | `POST /steward/{kind}/start` (`mcp/steward.rs`) | [`finish_terminal_create`], `FreshPinned` |
//! | `create_terminal_session_backend` (gate continuations) | its own lineage register, then [`bind_registered_coord_session`] |
//!
//! The steward create path is NOT exempt, and never was on purpose: a steward
//! pane is a terminal-hosted agent session like any other, nothing in
//! `mcp/steward.rs` argued for leaving it without a coord row, and without one
//! it could not be ended remotely either. It registers like the HTTP doors.
//!
//! The backend gate-continuation spawn shares only the binding half (it is
//! synchronous, and registers with its own lineage). It already
//! carries its own lineage (`CoordSessionLineage` off the capture hint) and its
//! own durable lifecycle record, and it has never run repo detection or the
//! credential helper: a coord-spawned continuation has no operator to answer an
//! "unregistered repo" prompt, and widening its git credential surface is a
//! separate decision from this plan's.
//!
//! The remote-CREATE path (`mcp/backend_relay.rs`) registers through
//! `register_external_confirmed`, a different contract (the id is reported
//! only once coord has confirmed the row), and is out of this module's scope.

use std::sync::Arc;

use tracing::{info, warn};
use uuid::Uuid;

use crate::session::pane_store::{PaneKey, PaneSessionStore};
use crate::session::{Intent, SessionError, SessionKind, SessionRegistry};
use crate::terminal::TerminalManager;

/// How a freshly created pane gets its coord session row.
pub(crate) enum CoordRegistration {
    /// The Tauri command's pane identity: RESUME the coord row persisted for
    /// `pane_key` if there is one and no other pane is bound to it (falling
    /// back to a fresh register when coord has GC'd it), otherwise register
    /// fresh — and persist the resulting id over `pane_key` so the next runner
    /// start resumes the same row. Keyed by the pane's pinned harness session
    /// id either way.
    PaneKeyed {
        pane_store: Arc<PaneSessionStore>,
        pane_key: PaneKey,
    },
    /// Register a fresh row keyed by the pane's pinned harness session id, no
    /// coord parent. The HTTP doors: they have no restore round-trip, so no
    /// pane key (see the module doc).
    FreshPinned,
}

/// The `terminal_shell` intent a create door declares to coord. `title` is the
/// purpose when it is at least 3 characters, else `default_purpose`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn terminal_shell_intent(
    title: Option<&str>,
    default_purpose: &str,
    repo: Option<String>,
    work_unit_slug: Option<String>,
    correlation_topic: Option<String>,
    working_dir: Option<&str>,
    tenant_id: Option<Uuid>,
) -> Intent {
    let purpose = title
        .filter(|t| t.trim().len() >= 3)
        .map(str::to_string)
        .unwrap_or_else(|| default_purpose.to_string());
    let (share_output, redact_secrets) = super::intent_sharing_from_settings();
    Intent {
        kind: SessionKind::TerminalShell,
        purpose,
        repo,
        branch: None,
        work_unit_slug,
        // Accept-only on `Intent`; the runner never emits the legacy key.
        plan_slug: None,
        correlation_topic,
        // Deliberately None on these create paths — a PRESENT `page_id` is the
        // coord-side "this is a gate continuation" marker.
        page_id: None,
        declared_paths: working_dir
            .map(std::path::PathBuf::from)
            .into_iter()
            .collect(),
        share_output,
        redact_secrets,
        // `None` → the registry stamps the device default.
        tenant_id,
    }
}

/// Register (or resume) the coord session row for terminal `terminal_id` and
/// wire it onto the pane: pane-store put, `bind_coord_session`, the on-exit
/// coord close hook, and the output pipe. Returns the coord session id, or
/// `None` when registration failed (logged; the terminal is unaffected — a
/// coord hiccup must never fail a spawn).
///
/// Takes the managers rather than an `AppHandle` so it runs from any door and
/// from a unit test. See the module doc for which door calls what.
///
/// DELIBERATELY ON THE CRITICAL PATH of every create (Phase 6, §4 Q2 of the
/// spawn-latency plan — do not move it into a `tokio::spawn`):
/// `attach_output_pipe` subscribes to a live broadcast of PTY output, so
/// deferring it drops the session's first bytes, and the binding plus the
/// `set_on_exit` close hook must move together WITH registration or a PTY that
/// exits first leaves a coord ghost. `register_external*` is synchronous and
/// does no HTTP (it queues an outbox row); `resume_external` additionally does
/// a live `PATCH /sessions/:id` probe.
pub(crate) async fn wire_coord_session(
    terminal_manager: &TerminalManager,
    registry: &Arc<SessionRegistry>,
    terminal_id: &str,
    intent: Intent,
    registration: CoordRegistration,
    door: &'static str,
) -> Option<Uuid> {
    // Key the row by the harness session id the identity seam pinned this PTY
    // child to (inside `create`, which runs before this on every door).
    // Without it the registry falls back to the RUNNER's own ambient id, which
    // no child ever runs under, so coord's scoped `coord_report_status` could
    // never resolve the row. `None` only when the terminal vanished between
    // spawn and here.
    let pinned_session_id = terminal_manager
        .get(terminal_id)
        .map(|session| session.pinned_session_id().to_string());

    let mut resumed = false;
    let mut persist_to: Option<(Arc<PaneSessionStore>, PaneKey)> = None;
    // Held from before the bound-row check until the pane is bound, so a
    // concurrent same-key create cannot pass the check across the
    // `resume_external` await. Released right after the bind below.
    let mut resume_claim: Option<crate::session::pane_store::ResumeClaim> = None;
    let registration: Result<Uuid, SessionError> = match registration {
        CoordRegistration::PaneKeyed {
            pane_store,
            pane_key,
        } => {
            // A persisted row another pane is still bound to is not this pane's
            // to resume: two panes with one `(page_id, title, working_dir,
            // window)` triple open at once would share one coord row — the
            // first pane's exit hook closes the second's row, and a remote end
            // of that row resolves to nothing (`resolve_coord_terminal_id`
            // requires exactly one live pane). Register fresh instead. An unreadable
            // terminal map is UNKNOWN and takes the same fresh arm: one extra
            // row is recoverable, a shared one is not. Finding 77046422.
            //
            // The check is taken under a resume claim on the row: another
            // create in this process mid-resume of the same row holds it,
            // and that is treated as a live holder (it will be bound the
            // moment its await returns).
            let persisted = match pane_store.get(&pane_key) {
                Some(prior_id) => match pane_store.claim_resume(prior_id) {
                    None => {
                        warn!(
                            terminal_id = %terminal_id,
                            coord_session = %prior_id,
                            door,
                            "terminal create: another create is resuming the pane key's \
                             persisted coord row right now (or the claim set is unreadable) \
                             — registering a fresh row instead of resuming it"
                        );
                        None
                    }
                    Some(claim) => match terminal_manager.coord_row_bound_at(prior_id) {
                        Some(None) => {
                            resume_claim = Some(claim);
                            persist_to = Some((pane_store, pane_key));
                            Some(prior_id)
                        }
                        held_or_unknown => {
                            // An EXITED holder lingers in the map, still bound,
                            // until its tab closes — and closing it closes the row
                            // again, so even a dead holder's row is not safe to
                            // share. But it will never be resumed by that pane, so
                            // the key moves to this pane's fresh row; a live holder
                            // (or an unreadable map) keeps the key.
                            let holder_alive = held_or_unknown.is_none()
                                || live_pane_holds(terminal_manager, prior_id);
                            if !holder_alive {
                                persist_to = Some((pane_store, pane_key));
                            }
                            warn!(
                                terminal_id = %terminal_id,
                                coord_session = %prior_id,
                                door,
                                held = held_or_unknown.is_some(),
                                holder_alive,
                                "terminal create: the pane key's persisted coord row is still bound \
                                 to another pane (or the terminal map is unreadable) — registering \
                                 a fresh row instead of resuming it"
                            );
                            None
                        }
                    },
                },
                None => {
                    persist_to = Some((pane_store, pane_key));
                    None
                }
            };
            match persisted {
                // R2 — RESUME the pane's prior coord row: PATCH the EXISTING
                // row (state=active + heartbeat, re-keyed to the current pin)
                // so a runner restart no longer orphans it and mints a
                // duplicate. A persisted-but-GC'd row falls back to a fresh
                // register inside `resume_external`.
                Some(prior_id) => {
                    use tracing::Instrument;
                    resumed = true;
                    registry
                        .resume_external(prior_id, intent, pinned_session_id)
                        .instrument(tracing::debug_span!(
                            "terminal_spawn.coord_register",
                            terminal_id = %terminal_id,
                            resume = true
                        ))
                        .await
                }
                None => {
                    let _span = tracing::debug_span!(
                        "terminal_spawn.coord_register",
                        terminal_id = %terminal_id,
                        resume = false
                    )
                    .entered();
                    registry.register_external_with_lineage(intent, None, pinned_session_id)
                }
            }
        }
        CoordRegistration::FreshPinned => {
            let _span = tracing::debug_span!(
                "terminal_spawn.coord_register",
                terminal_id = %terminal_id,
                resume = false
            )
            .entered();
            registry.register_external_with_lineage(intent, None, pinned_session_id)
        }
    };

    let coord_id = match registration {
        Ok(coord_id) => coord_id,
        Err(e) => {
            warn!(
                terminal_id = %terminal_id,
                door,
                error = %e,
                "terminal create: coord session registration failed — terminal unaffected"
            );
            return None;
        }
    };

    bind_registered_coord_session(terminal_manager, registry, terminal_id, coord_id, door);
    // The row is now visible as bound in the terminal map, so the map check
    // alone protects it from here on.
    drop(resume_claim);

    // Persist (or refresh) the pane → coord-id mapping so the NEXT restart
    // resumes this same row. On the GC'd-fallback path `coord_id` is a fresh
    // id replacing the stale one. AFTER the bind: a same-key create that
    // reads the key from here on finds the row already bound and registers
    // fresh. Written before the bind, a fresh row would be visible under the
    // key with no pane bound to it and no claim on it, and a racing create
    // could resume it.
    if let Some((pane_store, pane_key)) = persist_to {
        pane_store.put(&pane_key, coord_id);
    }
    info!(
        terminal_id = %terminal_id,
        coord_session = %coord_id,
        door,
        resumed,
        "terminal create: coord session ready"
    );
    Some(coord_id)
}

/// Whether a terminal whose process is still running is bound to coord row
/// `row`. An exited pane lingers in the map, bound, until its tab closes; it
/// will never resume the row, so the pane key may move off it.
fn live_pane_holds(terminal_manager: &TerminalManager, row: Uuid) -> bool {
    terminal_manager
        .sessions_snapshot()
        .iter()
        .any(|(_, s)| s.is_alive() && s.coord_session_id() == Some(row))
}

/// Wire an ALREADY-REGISTERED coord row `coord_id` onto terminal
/// `terminal_id`: [`TerminalManager::bind_coord_session`], the on-exit coord
/// close hook, and the output pipe. A no-op when the terminal is gone.
///
/// The half of [`wire_coord_session`] the synchronous backend spawn
/// (`create_terminal_session_backend`) shares: it registers with its own
/// capture-hint lineage, then calls this. Must run under a tokio runtime (the
/// exit hook captures its handle; the PTY waiter thread that fires the hook
/// has none).
pub(crate) fn bind_registered_coord_session(
    terminal_manager: &TerminalManager,
    registry: &Arc<SessionRegistry>,
    terminal_id: &str,
    coord_id: Uuid,
    door: &'static str,
) {
    if let Some(session) = terminal_manager.get(terminal_id) {
        terminal_manager.bind_coord_session(&session, coord_id);

        // R1 — the on-exit hook: the PTY waiter thread closes the coord
        // session mirror the instant the process exits, instead of leaving a
        // ghost until coord's own stale→closed watcher reaps it. Shares the
        // idempotent `close_by_id` path with `terminal_close`, so a
        // double-close (exit + explicit close) is a no-op.
        let close_registry = registry.clone();
        // Fixed at spawn — safe to capture as an owned String.
        let exit_pinned_session_id = session.pinned_session_id().to_string();
        // A gate continuation (or a boot-restored one, plan 2026-10-03 D3) is
        // registered in the continuation registry against THIS terminal, so
        // its death must reach the same producer; a no-op for any terminal the
        // registry does not hold — every ordinary tab.
        let exited_terminal_id = terminal_id.to_string();
        // The waiter thread that fires the hook has no tokio runtime, so
        // capture this one here; every caller runs under tokio.
        let exit_rt_handle = tokio::runtime::Handle::try_current().ok();
        session.set_on_exit(Box::new(move |coord_id, exit| {
            if let Err(e) = close_registry.close_by_id(coord_id) {
                warn!(
                    coord_session = %coord_id,
                    door,
                    error = %e,
                    "terminal exit hook: coord session close failed"
                );
            }
            crate::agent_runtime::notify_continuation_terminal_exit(
                &exited_terminal_id,
                exit_rt_handle.as_ref(),
                Some(exit),
            );
            // Trigger 4 (session_exit) — plan
            // 2026-08-27-operator-touch-observation-runner-emitter, Phase B2.
            crate::session::operator_touch::emit_session_exit_if_nonzero(
                &close_registry,
                coord_id,
                Some(&exit_pinned_session_id),
                exit.agent_exit_code(),
            );
        }));

        // The output pipe so PTY output streams to coord.
        let rx = session.subscribe_output();
        registry.attach_output_pipe(coord_id, rx, true);
    }
}

/// Everything a create door does after `TerminalManager::create` returned
/// `terminal_id`: park the isolated edit context, [`wire_coord_session`], then
/// the repo-detection emit (only with an `app_handle`) and the git credential
/// helper (only with a `working_dir` and a coord session id). Returns the
/// coord session id.
///
/// `working_dir` is the directory the door asked for (after worktree
/// substitution), not the default `create` fell back to — the same value the
/// Tauri command always handed these two steps.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn finish_terminal_create(
    terminal_manager: &TerminalManager,
    registry: Option<&Arc<SessionRegistry>>,
    app_handle: Option<&tauri::AppHandle>,
    terminal_id: &str,
    isolated_ctx: Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
    intent: Intent,
    registration: CoordRegistration,
    working_dir: Option<String>,
    door: &'static str,
) -> Option<Uuid> {
    // Park the isolated edit context on the terminal session so its heartbeat
    // + claim live as long as the PTY. Cleared in `close()`.
    if let Some(ctx) = isolated_ctx {
        if let Some(session) = terminal_manager.get(terminal_id) {
            session.set_isolated_edit_ctx(ctx);
        }
    }

    let coord_session_id = match registry {
        Some(registry) => {
            wire_coord_session(
                terminal_manager,
                registry,
                terminal_id,
                intent,
                registration,
                door,
            )
            .await
        }
        None => {
            // The registry is `.manage()`d in `setup` before the API server
            // starts, so this is a boot-ordering fault, not a policy.
            warn!(
                terminal_id = %terminal_id,
                door,
                "terminal create: no SessionRegistry in app state — this pane has no coord row"
            );
            None
        }
    };

    if let Some(app_handle) = app_handle {
        let handle = app_handle.clone();
        let dir = working_dir.clone();
        tokio::spawn(async move {
            crate::repo_detection::check_and_emit_unregistered(handle, dir).await;
        });
    }

    // Hard-requires coord to already know the session, so it runs after
    // registration; off the critical path.
    if let (Some(dir), Some(sid)) = (working_dir, coord_session_id) {
        let session_id_str = sid.to_string();
        tokio::spawn(async move {
            crate::credential_helper::setup_credential_helper(&dir, &session_id_str).await;
        });
    }

    coord_session_id
}

/// Test fixtures shared by the door tests (`mcp/tauri_proxy.rs`,
/// `mcp/terminals.rs`): a [`SessionRegistry`] whose coord is unreachable, so
/// a register only queues an outbox row.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::session::coord_sync::CoordSync;
    use crate::session::local_store::OutboxWriter;
    use crate::session::{DynTransport, ExternalTransport, SessionRegistry, SessionTransports};

    /// Keep alive for the test's duration (ambient pin + outbox tempdir).
    pub(crate) struct RegistryFixture {
        pub registry: Arc<SessionRegistry>,
        _amb: crate::test_env::IsolatedAmbient,
        _dir: tempfile::TempDir,
    }

    pub(crate) fn registry() -> RegistryFixture {
        let amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let coord = CoordSync::new_for_test(
            outbox,
            "http://127.0.0.1:1".into(),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let transports = SessionTransports {
            pty: Arc::new(ExternalTransport) as DynTransport,
            claude_cli: Arc::new(ExternalTransport) as DynTransport,
            workflow: Arc::new(ExternalTransport) as DynTransport,
        };
        let registry = SessionRegistry::new(uuid::Uuid::new_v4(), transports, coord.clone());
        coord.attach_registry(&registry);
        RegistryFixture {
            registry,
            _amb: amb,
            _dir: dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(tm: &TerminalManager, id: &str) -> Arc<crate::terminal::session::TerminalSession> {
        let session = Arc::new(crate::terminal::session::tests::make_test_session(
            Arc::new(std::sync::Mutex::new(Vec::new())),
        ));
        tm.insert_for_test(id, session.clone());
        session
    }

    // `make_test_session` is already dead, so dropping these fixtures joins
    // no threads and fires no exit hook.

    fn intent() -> Intent {
        terminal_shell_intent(Some("wiring test"), "x", None, None, None, None, None)
    }

    /// `FreshPinned` binds the pane to the id it registered, keyed by the pin.
    #[tokio::test]
    async fn fresh_registration_binds_the_pane_to_its_coord_row() {
        let fx = test_support::registry();
        let tm = TerminalManager::new();
        let session = pane(&tm, "t-pinned");
        let coord = wire_coord_session(
            &tm,
            &fx.registry,
            "t-pinned",
            intent(),
            CoordRegistration::FreshPinned,
            "test",
        )
        .await
        .expect("registers");
        assert_eq!(session.coord_session_id(), Some(coord));
    }

    /// The binding half the backend spawn shares binds what it is handed.
    #[tokio::test]
    async fn the_binding_half_binds_an_already_registered_row() {
        let fx = test_support::registry();
        let tm = TerminalManager::new();
        let session = pane(&tm, "t-backend");
        let coord = fx
            .registry
            .register_external_with_lineage(intent(), None, None)
            .expect("registers");
        bind_registered_coord_session(&tm, &fx.registry, "t-backend", coord, "test");
        assert_eq!(session.coord_session_id(), Some(coord));
        // A vanished terminal is a no-op, not a panic.
        bind_registered_coord_session(&tm, &fx.registry, "t-gone", coord, "test");
    }

    /// `PaneKeyed` persists the id over the pane key, so the next create of
    /// the same pane resumes that row rather than minting a second one.
    #[tokio::test]
    async fn pane_keyed_registration_persists_the_id_for_the_next_create() {
        let fx = test_support::registry();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PaneSessionStore::open(dir.path().join("panes.json")).unwrap());
        let key = PaneKey::from_create_in_window("default", "wiring test", "", "main");
        let tm = TerminalManager::new();
        let session = pane(&tm, "t-pane");
        let coord = wire_coord_session(
            &tm,
            &fx.registry,
            "t-pane",
            intent(),
            CoordRegistration::PaneKeyed {
                pane_store: store.clone(),
                pane_key: key.clone(),
            },
            "test",
        )
        .await
        .expect("registers");
        assert_eq!(store.get(&key), Some(coord));
        assert_eq!(session.coord_session_id(), Some(coord));
    }

    /// A second pane with the same pane key, created while the first is still
    /// live, gets its OWN row rather than resuming the first pane's — and the
    /// key keeps naming the first pane's row. Finding 77046422.
    #[tokio::test]
    async fn a_pane_key_held_by_a_live_pane_registers_fresh() {
        let fx = test_support::registry();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PaneSessionStore::open(dir.path().join("panes.json")).unwrap());
        let key = PaneKey::from_create_in_window("default", "wiring test", "", "main");
        let tm = TerminalManager::new();
        let registration = || CoordRegistration::PaneKeyed {
            pane_store: store.clone(),
            pane_key: key.clone(),
        };
        let first = pane(&tm, "t-first");
        first.set_alive_for_test(true);
        // Back to dead on every exit, a failed assertion included, or the
        // fixture's `Drop` runs `close()`.
        struct DeadOnDrop(Arc<crate::terminal::session::TerminalSession>);
        impl Drop for DeadOnDrop {
            fn drop(&mut self) {
                self.0.set_alive_for_test(false);
            }
        }
        let _dead_on_drop = DeadOnDrop(first.clone());
        let first_coord = wire_coord_session(
            &tm,
            &fx.registry,
            "t-first",
            intent(),
            registration(),
            "test",
        )
        .await
        .expect("registers");
        let second = pane(&tm, "t-second");
        let second_coord = wire_coord_session(
            &tm,
            &fx.registry,
            "t-second",
            intent(),
            registration(),
            "test",
        )
        .await
        .expect("registers");
        assert_ne!(
            first_coord, second_coord,
            "two live panes must not share a coord row"
        );
        assert_eq!(first.coord_session_id(), Some(first_coord));
        assert_eq!(second.coord_session_id(), Some(second_coord));
        assert_eq!(
            store.get(&key),
            Some(first_coord),
            "the key stays with its holder"
        );
    }

    /// A row another create is mid-resume of (its claim held across the
    /// `resume_external` await, before its bind) is not resumed a second time:
    /// the racing create registers fresh and leaves the key alone. Once the
    /// claim is released the row is resumable again. The #2972 residual.
    #[tokio::test]
    async fn a_row_another_create_is_resuming_registers_fresh() {
        let fx = test_support::registry();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PaneSessionStore::open(dir.path().join("panes.json")).unwrap());
        let key = PaneKey::from_create_in_window("default", "wiring test", "", "main");
        let tm = TerminalManager::new();
        let registration = || CoordRegistration::PaneKeyed {
            pane_store: store.clone(),
            pane_key: key.clone(),
        };
        let prior = Uuid::new_v4();
        store.put(&key, prior);
        let in_flight = store.claim_resume(prior).expect("claim");
        let racer = pane(&tm, "t-racer");
        let racer_coord = wire_coord_session(
            &tm,
            &fx.registry,
            "t-racer",
            intent(),
            registration(),
            "test",
        )
        .await
        .expect("registers");
        assert_ne!(racer_coord, prior, "a row mid-resume is not shared");
        assert_eq!(racer.coord_session_id(), Some(racer_coord));
        assert_eq!(store.get(&key), Some(prior), "the key stays put");
        drop(in_flight);
    }

    /// A holder whose PTY has exited still gets no shared row — closing its
    /// tab would close the row again — but the key moves to the new pane's
    /// fresh row, so the next restart resumes the pane that is still open.
    #[tokio::test]
    async fn a_pane_key_held_only_by_an_exited_pane_moves_to_the_fresh_row() {
        let fx = test_support::registry();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PaneSessionStore::open(dir.path().join("panes.json")).unwrap());
        let key = PaneKey::from_create_in_window("default", "wiring test", "", "main");
        let tm = TerminalManager::new();
        let registration = || CoordRegistration::PaneKeyed {
            pane_store: store.clone(),
            pane_key: key.clone(),
        };
        let _exited = pane(&tm, "t-exited");
        let first_coord = wire_coord_session(
            &tm,
            &fx.registry,
            "t-exited",
            intent(),
            registration(),
            "test",
        )
        .await
        .expect("registers");
        let next = pane(&tm, "t-next");
        let next_coord = wire_coord_session(
            &tm,
            &fx.registry,
            "t-next",
            intent(),
            registration(),
            "test",
        )
        .await
        .expect("registers");
        assert_ne!(
            next_coord, first_coord,
            "an exited holder's row is not shared"
        );
        assert_eq!(next.coord_session_id(), Some(next_coord));
        assert_eq!(
            store.get(&key),
            Some(next_coord),
            "the key moves to the open pane"
        );
    }

    /// No registry is a logged miss, not a panic, and binds nothing.
    #[tokio::test]
    async fn no_registry_binds_nothing() {
        // The intent reads settings, so pin the ambient like the other tests.
        let _amb = crate::test_env::isolated_ambient();
        let tm = TerminalManager::new();
        let session = pane(&tm, "t-none");
        let got = finish_terminal_create(
            &tm,
            None,
            None,
            "t-none",
            None,
            intent(),
            CoordRegistration::FreshPinned,
            None,
            "test",
        )
        .await;
        assert_eq!(got, None);
        assert_eq!(session.coord_session_id(), None);
    }

    /// Source pin for the doors no unit test can enter (plan
    /// `2026-11-05-remote-session-end-reaches-the-target-but-cannot-end-a-real-session`
    /// Phase 4): `POST /terminals`, steward start and the invoke proxy's arm all
    /// need a live `AppHandle` to spawn, so a test cannot drive them to the
    /// post-create wiring. Instead, each door's production code (before its
    /// first `#[cfg(test)]`) must still call the shared wiring — and for the two
    /// `acquire_for_terminal` doors, AFTER their `door-scan: spawn` marker, i.e.
    /// on the success path of the real spawn. Deleting a call fails this test.
    #[test]
    fn every_create_door_still_calls_the_shared_coord_wiring() {
        let doors: &[(&str, &str, Option<&str>, &str)] = &[
            (
                "mcp/tauri_proxy.rs",
                include_str!("../../mcp/tauri_proxy.rs"),
                Some("door-scan: spawn"),
                "proxy_terminal_created(&door",
            ),
            (
                "mcp/terminals.rs",
                include_str!("../../mcp/terminals.rs"),
                Some("door-scan: spawn"),
                "coord_wiring::finish_terminal_create(",
            ),
            (
                "mcp/steward.rs",
                include_str!("../../mcp/steward.rs"),
                None,
                "coord_wiring::finish_terminal_create(",
            ),
            (
                "commands/terminal.rs",
                include_str!("../terminal.rs"),
                Some("door-scan: spawn"),
                "finish_terminal_create(",
            ),
            (
                "commands/terminal.rs (create_terminal_session_backend)",
                include_str!("../terminal.rs"),
                Some("fn create_terminal_session_backend"),
                "coord_wiring::bind_registered_coord_session(",
            ),
        ];
        for (name, src, marker, call) in doors {
            let production = src.split("#[cfg(test)]").next().unwrap_or_default();
            let from = match marker {
                Some(m) => production
                    .find(m)
                    .unwrap_or_else(|| panic!("{name}: no `{m}` marker in production code")),
                None => 0,
            };
            assert!(
                production
                    .get(from..)
                    .is_some_and(|rest| rest.contains(call)),
                "{name}: production code no longer calls `{call}`{} — the pane it \
                 creates would have no terminal_shell coord row",
                marker.map(|m| format!(" after `{m}`")).unwrap_or_default()
            );
        }
    }
}
