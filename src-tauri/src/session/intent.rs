//! Session intent — typed declaration of what a session is for, where it
//! runs, and what it expects to touch. Plan
//! [`2026-05-22-coord-native-session-coordination`] §D6.
//!
//! Every call to [`crate::session::Session::start`] requires an `Intent`. The
//! wire shape mirrors `coord.sessions.intent` (JSONB) so the runner-side
//! struct serializes verbatim into the row coord stores.
//!
//! Validation is intentionally light. The plan favors explicit-over-implicit:
//! we reject obvious mistakes (`purpose` blank, paths that don't exist) but
//! we do **not** sanitize free text — the dashboard is the readable surface
//! and it can render what coord stores.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::SessionKind;

/// Minimum length of a session purpose. Matches the dashboard's
/// "what is this session for?" copy — anything shorter usually means
/// the caller forgot to fill it in.
const MIN_PURPOSE_CHARS: usize = 3;

/// Typed declaration of session intent. Plan §D6 spec; the field set is the
/// stable wire contract between runner and coord (`coord.sessions.intent`).
///
/// The `Intent` lives in the `qontinui-runner` binary crate (not the
/// `qontinui_runner_lib` crate that other binaries import) — doctests are
/// therefore plain examples rather than executable docs. See the
/// `tests` module below for the executable round-trip + validation
/// coverage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Intent {
    /// Discriminator. Selects which transport handles the session.
    pub kind: SessionKind,
    /// Human-readable purpose. Surfaces in the dashboard Live Sessions
    /// panel; required, min [`MIN_PURPOSE_CHARS`] chars after trim.
    pub purpose: String,
    /// Optional repo slug. When set together with [`Intent::branch`], the
    /// session auto-acquires a `ClaimKind::RepoBranch` on
    /// `<repo>:<branch>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Optional branch name. See [`Intent::repo`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Optional work-unit slug this session is working on. Forwarded into
    /// `coord.sessions.intent` so coord can group sessions by work unit and
    /// drive unit-scoped multi-repo dispatch.
    ///
    /// Canonical since the wire-key retirement (plan
    /// `2026-07-30-coord-web-plan-slug-wire-key-retirement`, Phase 1 step 6):
    /// this is the ONLY slug key the runner emits. Coord's accept side binds
    /// it and its own `Intent` keeps a deprecated `plan_slug` field for an
    /// un-upgraded runner, so the emit rename is safe against a deployed
    /// coord. Read it through [`Intent::resolved_work_unit_slug`], never off
    /// this field directly, so an inbound legacy body still resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit_slug: Option<String>,
    /// DEPRECATED accept-side alias for the pre-retirement wire key.
    ///
    /// **A real second field, deliberately NOT `#[serde(alias = "plan_slug")]`.**
    /// Serde treats an alias and the canonical name as the SAME field, so a
    /// body carrying BOTH spellings fails outright with
    /// ``duplicate field `work_unit_slug` `` — an alias is a rename, not a
    /// fallback. Coord dual-emits both keys today, so a both-keys body is a
    /// real input, and this struct is `Serialize` **and** `Deserialize` (its
    /// own round-trip tests below feed it its own output). This fleet already
    /// shipped and reverted exactly that bug — see the same two-field shape
    /// and its reasoning on `LaunchPayload` (`agent_runtime.rs:92-103`,
    /// regression test `launch_payload_accepts_coords_dual_emitted_both_keys`).
    ///
    /// ACCEPT-ONLY: every constructor leaves this `None`, so a runner-built
    /// intent emits exactly one slug key — the canonical one. (An intent
    /// *deserialized* from a legacy or both-keys body does carry it, and
    /// re-serializes it; that is the point of keeping the value rather than
    /// folding it away on parse.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_slug: Option<String>,
    /// Optional correlation topic for cross-machine peer discovery /
    /// rendezvous. Forwarded into `coord.sessions.intent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_topic: Option<String>,
    /// Optional page tab id the terminal pane was placed on. Set ONLY on
    /// the gate-continuation create path; a present `page_id` is the
    /// coord-side marker that the session is a gate continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
    /// Paths the session intends to touch. The plan calls this out as the
    /// hook for fine-grained claim narrowing in later phases; for now the
    /// list is persisted verbatim in the intent JSON.
    #[serde(default)]
    pub declared_paths: Vec<PathBuf>,
    /// Plan §D10 — opt-in PTY output streaming. Off by default; sensitive
    /// sessions stay local. Phase 8 consumes this when wiring PTY bytes
    /// through JetStream.
    #[serde(default)]
    pub share_output: bool,
    /// Plan §D11 — when `Some(true)`, run a regex sweep against PTY output
    /// before fan-out. When `None`, defaults to the value of
    /// [`Intent::share_output`] (see [`Intent::effective_redact_secrets`]).
    #[serde(default)]
    pub redact_secrets: Option<bool>,
    /// Phase 8b (plan `2026-07-02-session-scoped-multi-tenant-device-binding`
    /// §D4/§D7) — the tenant binding this session runs under, recorded at
    /// creation: the SPAWN INPUT when the caller supplied one, else the
    /// device's default (`machine.json::active_tenant_id`,
    /// default-for-new-sessions), stamped by `SessionRegistry::start`/
    /// `register_external`. Immutable for the session's life. Drives BOTH
    /// the `POST /sessions` body's `tenant_id` and which device-JWT slot the
    /// coord-sync loop presents for this session's writes. `None` only on
    /// single-tenant installs with no configured default (coord then
    /// resolves sole-binding server-side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<uuid::Uuid>,
}

impl Intent {
    /// The work-unit slug under either wire spelling, the canonical key
    /// winning.
    ///
    /// The read door for both fields — never read either field directly. An
    /// intent deserialized from a pre-retirement body (or from a coord that
    /// still dual-emits) carries the slug under the deprecated key only; this
    /// is what makes that body resolve.
    ///
    /// **Deliberately NOT named `work_unit_slug`**, which would collide with
    /// the field. `intent.work_unit_slug.as_deref()` compiles, returns the
    /// same `Option<&str>`, and silently skips the legacy fallback — one pair
    /// of parens between correct and quietly wrong, with no type-level tell.
    /// The name matches coord's counterpart on the same dual-field struct
    /// (`qontinui-coord/src/sessions.rs` `Intent::resolved_work_unit_slug`),
    /// so the pattern greps cleanly across both repos. The runner's
    /// `LaunchPayload` solves the same collision the other way round (field
    /// `work_unit_slug_new` + `#[serde(rename)]`, `agent_runtime.rs:85`).
    ///
    /// Currently only exercised by this module's tests: nothing in the runner
    /// reads the slug back off a typed `Intent` (the handoff read path parses
    /// the persisted JSONB raw — see `session::handoff::build_child_intent`).
    /// It exists so that the first consumer that does cannot accidentally read
    /// one spelling and silently miss the other.
    #[allow(dead_code)]
    pub fn resolved_work_unit_slug(&self) -> Option<&str> {
        self.work_unit_slug.as_deref().or(self.plan_slug.as_deref())
    }

    /// Resolve the effective `redact_secrets` flag. Plan §D11 — defaults to
    /// `share_output` so callers who opt into streaming get redaction
    /// without having to remember the second flag.
    pub fn effective_redact_secrets(&self) -> bool {
        self.redact_secrets.unwrap_or(self.share_output)
    }

    /// Validate the intent. Returns `Ok(())` on success; otherwise the
    /// first failed invariant. Plan §D6 — required at every session
    /// start.
    pub fn validate(&self) -> Result<(), IntentError> {
        let trimmed = self.purpose.trim();
        if trimmed.len() < MIN_PURPOSE_CHARS {
            return Err(IntentError::PurposeTooShort {
                got: trimmed.len(),
                min: MIN_PURPOSE_CHARS,
            });
        }

        // Branch without repo is a structural mismatch — a branch name only
        // makes sense scoped to a repo. Repo without branch is fine (whole
        // repo work).
        if self.branch.is_some() && self.repo.is_none() {
            return Err(IntentError::BranchWithoutRepo);
        }

        for path in &self.declared_paths {
            if path.as_os_str().is_empty() {
                return Err(IntentError::EmptyDeclaredPath);
            }
        }

        Ok(())
    }
}

/// Errors surfaced by [`Intent::validate`]. Stable enum — the Tauri command
/// handler maps each variant to a structured error code so the frontend can
/// branch without parsing strings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntentError {
    #[error("session purpose too short ({got} < {min} chars after trim)")]
    PurposeTooShort { got: usize, min: usize },
    #[error(
        "session intent has branch set but no repo — branch only makes sense scoped to a repo"
    )]
    BranchWithoutRepo,
    #[error("declared_paths contains an empty path")]
    EmptyDeclaredPath,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_intent() -> Intent {
        Intent {
            kind: SessionKind::TerminalShell,
            purpose: "fix the thing".into(),
            repo: None,
            branch: None,
            work_unit_slug: None,
            plan_slug: None,
            correlation_topic: None,
            page_id: None,
            declared_paths: vec![],
            share_output: false,
            redact_secrets: None,
            tenant_id: None,
        }
    }

    #[test]
    fn accepts_minimal_intent() {
        good_intent().validate().unwrap();
    }

    #[test]
    fn rejects_short_purpose() {
        let mut i = good_intent();
        i.purpose = "ab".into();
        assert!(matches!(
            i.validate(),
            Err(IntentError::PurposeTooShort { .. })
        ));
    }

    #[test]
    fn rejects_whitespace_only_purpose() {
        let mut i = good_intent();
        i.purpose = "   ".into();
        assert!(matches!(
            i.validate(),
            Err(IntentError::PurposeTooShort { .. })
        ));
    }

    #[test]
    fn rejects_branch_without_repo() {
        let mut i = good_intent();
        i.branch = Some("main".into());
        assert_eq!(i.validate(), Err(IntentError::BranchWithoutRepo));
    }

    #[test]
    fn accepts_repo_without_branch() {
        let mut i = good_intent();
        i.repo = Some("qontinui-runner".into());
        i.validate().unwrap();
    }

    #[test]
    fn rejects_empty_declared_path() {
        let mut i = good_intent();
        i.declared_paths.push(PathBuf::new());
        assert_eq!(i.validate(), Err(IntentError::EmptyDeclaredPath));
    }

    #[test]
    fn redact_defaults_to_share_output() {
        let mut i = good_intent();
        i.share_output = true;
        assert!(i.effective_redact_secrets());
        i.share_output = false;
        assert!(!i.effective_redact_secrets());
    }

    #[test]
    fn redact_explicit_overrides_default() {
        let mut i = good_intent();
        i.share_output = true;
        i.redact_secrets = Some(false);
        assert!(!i.effective_redact_secrets());
    }

    #[test]
    fn intent_round_trips_through_json() {
        let i = Intent {
            kind: SessionKind::TerminalClaude,
            purpose: "drive the agentic loop".into(),
            repo: Some("qontinui-web".into()),
            branch: Some("main".into()),
            work_unit_slug: Some("2026-05-22-coord-native-session-coordination".into()),
            plan_slug: None,
            correlation_topic: Some("by-correlation-topic/demo".into()),
            page_id: Some("page-2".into()),
            declared_paths: vec![PathBuf::from("/repo/path")],
            share_output: true,
            redact_secrets: Some(false),
            tenant_id: Some(uuid::Uuid::from_bytes([7; 16])),
        };
        let serialized = serde_json::to_string(&i).unwrap();
        let back: Intent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back, i);
    }

    #[test]
    fn tenant_id_none_omits_key_and_some_round_trips() {
        // `None` serializes WITHOUT the key (skip_serializing_if) so
        // existing coord rows / consumers see no shape change; a legacy
        // intent JSON with no tenant_id deserializes to None.
        let serialized = serde_json::to_string(&good_intent()).unwrap();
        assert!(!serialized.contains("tenant_id"));

        let t = uuid::Uuid::from_bytes([9; 16]);
        let mut with_tenant = good_intent();
        with_tenant.tenant_id = Some(t);
        let json = serde_json::to_string(&with_tenant).unwrap();
        assert!(json.contains(&format!("\"tenant_id\":\"{t}\"")));
        let back: Intent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, with_tenant);
    }

    #[test]
    fn page_id_none_omits_key_and_some_round_trips() {
        // `None` must serialize WITHOUT the key (skip_serializing_if) so
        // existing coord rows / consumers see no shape change.
        let serialized = serde_json::to_string(&good_intent()).unwrap();
        assert!(!serialized.contains("page_id"));

        // A JSON intent carrying `"page_id":"page-2"` round-trips.
        let mut with_page = good_intent();
        with_page.page_id = Some("page-2".into());
        let json = serde_json::to_string(&with_page).unwrap();
        assert!(json.contains("\"page_id\":\"page-2\""));
        let back: Intent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, with_page);
    }

    // ── Wire-key retirement (plan
    // `2026-07-30-coord-web-plan-slug-wire-key-retirement`, Phase 1 step 6 /
    // Phase 6 step 17). The runner EMITS only `work_unit_slug` and still
    // ACCEPTS the deprecated `plan_slug`, via two real fields rather than a
    // `#[serde(alias)]`.

    /// Emit side, serde attributes only: a constructed intent puts the
    /// canonical key on the wire and the `None` deprecated field is skipped.
    ///
    /// This pins the ATTRIBUTES, not the constructors — `good_intent()` sets
    /// `plan_slug: None` itself, so it cannot catch a constructor that was
    /// missed in the rename. The production-constructor coverage is
    /// `commands::session`'s `start_session_args_*` tests and
    /// `handoff::build_child_intent_reads_legacy_plan_slug_key`.
    #[test]
    fn emits_work_unit_slug_and_never_the_legacy_plan_slug_key() {
        let mut i = good_intent();
        i.work_unit_slug = Some("2026-07-30-coord-web-plan-slug-wire-key-retirement".into());
        let json = serde_json::to_value(&i).unwrap();
        assert_eq!(
            json.get("work_unit_slug").and_then(|v| v.as_str()),
            Some("2026-07-30-coord-web-plan-slug-wire-key-retirement")
        );
        assert!(
            json.get("plan_slug").is_none(),
            "the runner must emit the canonical key ALONE — coord's accept side \
             already binds `work_unit_slug`, so dual-emitting only re-seeds the \
             key this plan is retiring: {json}"
        );
    }

    /// Accept side: a body from a pre-retirement writer carries only the
    /// deprecated key, and the accessor still recovers the slug.
    #[test]
    fn accepts_legacy_plan_slug_only_body() {
        let json = serde_json::json!({
            "kind": "terminal_shell",
            "purpose": "fix the thing",
            "plan_slug": "2026-07-28-some-unit",
        });
        let i: Intent = serde_json::from_value(json).unwrap();
        assert_eq!(i.work_unit_slug, None);
        assert_eq!(i.plan_slug.as_deref(), Some("2026-07-28-some-unit"));
        assert_eq!(i.resolved_work_unit_slug(), Some("2026-07-28-some-unit"));
    }

    /// THE HAZARD THIS SHAPE EXISTS FOR. Coord dual-emits both spellings, so
    /// a body carrying BOTH is a real input. With `#[serde(alias =
    /// "plan_slug")]` this would fail outright with
    /// ``duplicate field `work_unit_slug` `` — an alias is a rename, not a
    /// fallback. Mirrors `agent_runtime`'s
    /// `launch_payload_accepts_coords_dual_emitted_both_keys`, which guards
    /// the identical trap on `LaunchPayload` after the fleet shipped and
    /// reverted it once.
    #[test]
    fn intent_accepts_coords_dual_emitted_both_keys() {
        let json = serde_json::json!({
            "kind": "terminal_shell",
            "purpose": "fix the thing",
            "plan_slug": "old-name",
            "work_unit_slug": "new-name",
        });
        let i: Intent = serde_json::from_value(json)
            .expect("a both-keys body must parse, not `duplicate field`");
        assert_eq!(i.work_unit_slug.as_deref(), Some("new-name"));
        assert_eq!(i.plan_slug.as_deref(), Some("old-name"));
        assert_eq!(
            i.resolved_work_unit_slug(),
            Some("new-name"),
            "the canonical key wins when both are present"
        );
    }

    /// The inverse of the above: `Intent` must round-trip its OWN serialized
    /// output — the shape its round-trip tests have always assumed. (Under an
    /// alias there is no second field, so this would not compile at all; the
    /// test that catches an alias at RUNTIME is
    /// `intent_accepts_coords_dual_emitted_both_keys` above.)
    #[test]
    fn intent_round_trips_its_own_output_including_a_both_keys_instance() {
        let canonical_only = Intent {
            work_unit_slug: Some("new-name".into()),
            ..good_intent()
        };
        let back: Intent = serde_json::from_value(serde_json::to_value(&canonical_only).unwrap())
            .expect("canonical-only intent must round-trip");
        assert_eq!(back, canonical_only);

        // A both-keys instance can only arrive by deserializing a legacy /
        // dual-emitted body, but it must survive a re-serialize + re-parse
        // unchanged — that is the write-back path for a handed-off intent.
        let both_keys = Intent {
            work_unit_slug: Some("new-name".into()),
            plan_slug: Some("old-name".into()),
            ..good_intent()
        };
        let json = serde_json::to_value(&both_keys).unwrap();
        assert_eq!(
            json.get("plan_slug").and_then(|v| v.as_str()),
            Some("old-name")
        );
        let back: Intent = serde_json::from_value(json)
            .expect("a both-keys intent must round-trip its own output");
        assert_eq!(back, both_keys);
        assert_eq!(back.resolved_work_unit_slug(), Some("new-name"));
    }

    /// Neither key is emitted for a slug-less session, exactly as before the
    /// rename — `skip_serializing_if` on both fields.
    #[test]
    fn slugless_intent_emits_neither_key() {
        let json = serde_json::to_string(&good_intent()).unwrap();
        assert!(!json.contains("work_unit_slug"), "{json}");
        assert!(!json.contains("plan_slug"), "{json}");
    }

    // ── Phase 8 (many-sessions plan): the terminal create paths build
    // `share_output` / `redact_secrets` from `settings.performance` instead
    // of hardcoded literals. These pin that the STOCK settings reproduce the
    // old literals exactly, and that the knob actually moves the intent.

    /// Stock settings → the pre-Phase-8 literals: `share_output: true`,
    /// `redact_secrets: None`, and therefore redaction ON.
    #[test]
    fn stock_performance_settings_reproduce_the_old_terminal_intent() {
        let perf = crate::settings::PerformanceSettings::default();
        let intent = Intent {
            share_output: perf.share_terminal_output,
            redact_secrets: perf.redact_terminal_secrets,
            ..good_intent()
        };
        assert!(intent.share_output);
        assert_eq!(intent.redact_secrets, None);
        assert!(
            intent.effective_redact_secrets(),
            "redaction followed share_output before Phase 8 and must still"
        );
    }

    /// Turning sharing off carries redaction with it (the documented
    /// `effective_redact_secrets` default), and the intent still validates —
    /// the knob changes what is declared to coord, never whether a terminal
    /// may start.
    #[test]
    fn disabling_share_output_flips_the_redaction_default() {
        let perf = crate::settings::PerformanceSettings {
            share_terminal_output: false,
            ..crate::settings::PerformanceSettings::default()
        };
        let intent = Intent {
            share_output: perf.share_terminal_output,
            redact_secrets: perf.redact_terminal_secrets,
            ..good_intent()
        };
        assert!(!intent.share_output);
        assert!(!intent.effective_redact_secrets());
        intent.validate().expect("still a valid intent");
    }

    /// An explicit redaction setting pins the flag independently of sharing,
    /// in both directions.
    #[test]
    fn explicit_redaction_setting_overrides_the_share_output_default() {
        let force_on = crate::settings::PerformanceSettings {
            share_terminal_output: false,
            redact_terminal_secrets: Some(true),
            ..crate::settings::PerformanceSettings::default()
        };
        let intent = Intent {
            share_output: force_on.share_terminal_output,
            redact_secrets: force_on.redact_terminal_secrets,
            ..good_intent()
        };
        assert!(intent.effective_redact_secrets());

        let force_off = crate::settings::PerformanceSettings {
            share_terminal_output: true,
            redact_terminal_secrets: Some(false),
            ..crate::settings::PerformanceSettings::default()
        };
        let intent = Intent {
            share_output: force_off.share_terminal_output,
            redact_secrets: force_off.redact_terminal_secrets,
            ..good_intent()
        };
        assert!(!intent.effective_redact_secrets());
    }
}
