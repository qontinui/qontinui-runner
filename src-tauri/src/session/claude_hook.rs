//! Bundled Claude `SessionStart` hook materializer + `--settings` delivery
//! (session-restore-redesign plan §4 `capture_hook_delivery`, Phase 2).
//!
//! ## What this delivers and why it never touches `~/.claude`
//!
//! The runner needs Claude to POST a confirmation/liveness signal to its
//! loopback control server on `SessionStart` (startup AND `--resume`). The
//! Phase-0 probe PROVED that a `SessionStart` hook supplied ONLY via
//! `claude --settings <file>` fires on both, additively — Claude MERGES the
//! `--settings` file's hooks on top of any `~/.claude` config WITHOUT writing
//! to it. So the entire delivery is two runner-owned files in the runner's OWN
//! app-data dir (`~/.qontinui/runner/session-restore/`):
//!
//!   * `claude_session_hook.sh` — the hook script (POSTs `{session_id, source,
//!     terminal_id, provider, cwd}` to `/control/session-open`).
//!   * `claude_hook_settings.json` — `{ "hooks": { "SessionStart": [...],
//!     "PreCompact": [...], "Stop": [...] } }` pointing each `command` at the
//!     corresponding materialized script. The `Stop` key is registered ONLY
//!     when the continuation flag is armed (see [`StopHookRegistration`]); a
//!     dark session gets a settings file with no `Stop` key at all, so Claude
//!     never spawns `bash` for it once per assistant turn.
//!
//! The identity shim appends `--settings <that settings file>` to the real
//! `claude` argv (alongside `--session-id`), so a HAND-STARTED `claude` gets the
//! hook out of the box. **Nothing is ever written to or read from
//! `~/.claude/settings.json`** — the out-of-box, zero-touch guarantee (plan §2
//! Principle 2). The hook is confirmation-only: identity is already pinned +
//! recorded synchronously at spawn (the §3b determinism mechanism).
//!
//! ## Materialization
//!
//! [`materialize`] is idempotent — it (re)writes both files every call (cheap,
//! a few hundred bytes) so a runner upgrade that ships a newer template
//! refreshes them, and returns the absolute settings-file path. Fail-open: any
//! IO error returns `None` (the launch then omits `--settings` — identity still
//! rides the spawn-time `--session-id` pin; only the confirmation hook is
//! absent). The settings/script live OUTSIDE any session cwd so they are never
//! committed by a user inspecting their repo.

use std::path::{Path, PathBuf};

/// Hook-script template (bundled). Substitutes nothing — it reads everything it
/// needs from env (`QONTINUI_TERMINAL_ID`, `QONTINUI_INSTALL_INTERCEPT_PORT`)
/// and stdin, so the same bytes work for every terminal.
const HOOK_SCRIPT: &str = include_str!("../../resources/session-restore/claude_session_hook.sh");
/// Stop-hook script template (bundled) — the continuation-verdict `Stop` hook
/// (plan `2026-07-17-session-autonomy-fabric.md` Phase 1). Like the
/// SessionStart hook it substitutes nothing: it reads the session key + the
/// runner API port from env (`QONTINUI_TERMINAL_ID`,
/// `QONTINUI_RUNNER_API_PORT`) and the Stop payload from stdin, so the same
/// bytes work for every terminal. Verdict policy lives entirely in the
/// runner's `POST /sessions/{id}/continuation-verdict` endpoint (D4) —
/// flag-gated `QONTINUI_STOP_HOOK_CONTINUATION` default `off`.
///
/// The script is materialized UNCONDITIONALLY (so [`hook_files`] stays
/// variant-independent), but its REGISTRATION in the delivered settings is
/// gated on the flag — see [`StopHookRegistration`]. A dark session therefore
/// gets no `Stop` key at all rather than a registered hook whose script exits
/// immediately: the script-level early exit still cost one `bash` spawn per
/// assistant turn, which is exactly what the gating removes.
const STOP_HOOK_SCRIPT: &str = include_str!("../../resources/session-restore/claude_stop_hook.sh");
/// PreCompact-hook script template (bundled) — the context-exhaustion signal
/// (plan `2026-07-17-session-autonomy-fabric.md` Phase 7). Same posture as
/// the Stop hook: a dumb curl reading its seam from env
/// (`QONTINUI_TERMINAL_ID`, `QONTINUI_RUNNER_API_PORT`) and the PreCompact
/// payload from stdin; all policy lives in the runner's
/// `POST /sessions/{id}/context-low` endpoint — flag-gated
/// `QONTINUI_CONTEXT_HANDOFF` default `off`, so shipping the hook to every
/// session is behaviorally inert until the flag is armed.
const PRECOMPACT_HOOK_SCRIPT: &str =
    include_str!("../../resources/session-restore/claude_precompact_hook.sh");
/// Policy-injection hook script template (bundled) — the SECOND `SessionStart`
/// command (plan `2026-08-08-runner-enforced-policy-pull.md` Phase 1). It rides
/// the same `SessionStart` block as [`HOOK_SCRIPT`] rather than extending it,
/// because that script is the confirmation/liveness carrier and must keep its
/// silent-stdout contract; this one exists precisely to PRINT — its stdout is
/// the `hookSpecificOutput.additionalContext` envelope Claude splices into the
/// session's context, so `policy/session-protocol` Step 0 is satisfied by
/// construction instead of by the agent volunteering. Same dumb-curl posture as
/// the Stop/PreCompact scripts: it reads its seam from env
/// (`QONTINUI_TERMINAL_ID`, `QONTINUI_RUNNER_API_PORT`) and prints the runner's
/// response verbatim; every decision lives in the runner's
/// `GET /sessions/{id}/policy-context` endpoint — flag-gated
/// `QONTINUI_POLICY_INJECTION` default `off`, which answers an EMPTY body, so
/// shipping the hook to every session is behaviorally inert until armed.
const POLICY_HOOK_SCRIPT: &str =
    include_str!("../../resources/session-restore/claude_policy_hook.sh");
/// Settings template (bundled). `@@HOOK_SCRIPT@@` → the absolute path of the
/// materialized SessionStart hook script; `@@STOP_HOOK_SCRIPT@@` → the
/// materialized Stop hook script; `@@PRECOMPACT_HOOK_SCRIPT@@` → the
/// materialized PreCompact hook script; `@@POLICY_HOOK_SCRIPT@@` → the
/// materialized SessionStart policy-injection script.
const HOOK_SETTINGS: &str =
    include_str!("../../resources/session-restore/claude_hook_settings.json");

/// File name of the materialized hook script.
const HOOK_SCRIPT_NAME: &str = "claude_session_hook.sh";
/// File name of the materialized Stop-hook script.
const STOP_HOOK_SCRIPT_NAME: &str = "claude_stop_hook.sh";
/// File name of the materialized PreCompact-hook script.
const PRECOMPACT_HOOK_SCRIPT_NAME: &str = "claude_precompact_hook.sh";
/// File name of the materialized SessionStart policy-injection script.
const POLICY_HOOK_SCRIPT_NAME: &str = "claude_policy_hook.sh";
/// File name of the materialized `--settings` file.
const HOOK_SETTINGS_NAME: &str = "claude_hook_settings.json";

/// Template placeholders, one per bundled script. These are the substitution
/// KEY — [`resolve_commands`] matches each `command` on the placeholder it
/// already carries rather than on its event, which is what lets the one
/// `SessionStart` block hold two different scripts.
const HOOK_SCRIPT_PLACEHOLDER: &str = "@@HOOK_SCRIPT@@";
const STOP_HOOK_SCRIPT_PLACEHOLDER: &str = "@@STOP_HOOK_SCRIPT@@";
const PRECOMPACT_HOOK_SCRIPT_PLACEHOLDER: &str = "@@PRECOMPACT_HOOK_SCRIPT@@";
const POLICY_HOOK_SCRIPT_PLACEHOLDER: &str = "@@POLICY_HOOK_SCRIPT@@";

/// Whether the delivered settings registers the `Stop` continuation hook.
///
/// The Stop-hook SCRIPT is always materialized; this decides only whether the
/// settings file carries a `hooks.Stop` key. Claude Code spawns `bash` once per
/// assistant turn for every registered `Stop` hook — even one that exits
/// immediately — so a dark session must not register it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopHookRegistration {
    /// The flag is armed (`observe`/`on`) — register `hooks.Stop`.
    Registered,
    /// The flag is dark (`off`, unset, empty or unknown) — omit `hooks.Stop`.
    Omitted,
}

impl StopHookRegistration {
    /// Pure — the unit-test surface. `Off` ⇒ `Omitted`; `Observe`/`On` ⇒
    /// `Registered`.
    pub fn from_mode(mode: crate::mcp::continuation_verdict::Mode) -> Self {
        use crate::mcp::continuation_verdict::Mode;
        match mode {
            Mode::Off => StopHookRegistration::Omitted,
            Mode::Observe | Mode::On => StopHookRegistration::Registered,
        }
    }

    /// Live read, mirroring `Mode::from_env` — including its fail-safe parse
    /// (`None`/empty/unknown ⇒ `Off` ⇒ `Omitted`).
    pub fn from_env() -> Self {
        Self::from_mode(crate::mcp::continuation_verdict::Mode::from_env())
    }
}

/// Env var the runner injects at spawn carrying the absolute path of the
/// materialized Claude `--settings` hook file. The identity shim's `claude`
/// wrapper reads it and appends `--settings $that` to the real argv. Empty/unset
/// ⇒ the shim appends nothing (fail-open — identity still rides `--session-id`).
pub const CLAUDE_SETTINGS_ENV: &str = "QONTINUI_CLAUDE_HOOK_SETTINGS";

/// The runner's OWN app-data dir for session-restore artifacts —
/// `~/.qontinui/runner/session-restore/`. Co-located with the lifecycle store +
/// shutdown marker (`~/.qontinui/runner/`). NEVER `~/.claude`.
pub fn session_restore_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
        .join("session-restore")
}

/// Materialize the bundled Claude SessionStart hook + its `--settings` file into
/// `base_dir` (prod: [`session_restore_dir`]; tests: a tempdir), substituting
/// the hook-script absolute path into the settings, and return the absolute path
/// of the settings file (for `claude --settings <path>` / [`DeliverySpec`]).
///
/// Idempotent + fail-open: any IO failure logs at warn and returns `None`, so a
/// launch that can't write the hook simply omits `--settings` (identity still
/// pinned via `--session-id`; only the confirmation hook is absent). The hook
/// script is marked executable on Unix.
///
/// CACHED PER (`base_dir`, [`StopHookRegistration`]) (Phase 6, B2). The four
/// SCRIPTS are byte-identical for a given `base_dir` (they are `include_str!`'d
/// constants), and the settings file is a function of `base_dir` AND the
/// registration variant. Every terminal spawn used to rewrite all of them plus
/// a `chmod` each; after the first materialize in this process a spawn with the
/// SAME variant costs one `stat` per file (proving they are still there) and
/// nothing else. An externally deleted or modified file — or a variant mismatch
/// — falls straight through to a full rewrite, so the cache can never serve a
/// path that is not on disk or whose content disagrees with the wanted variant.
///
/// Reads the live flag; see [`materialize_with`] for the explicit-variant form.
pub fn materialize(base_dir: &Path) -> Option<PathBuf> {
    materialize_with(base_dir, StopHookRegistration::from_env())
}

/// [`materialize`] with the `Stop`-hook registration variant supplied
/// explicitly (the unit-test surface; production reads the env).
pub fn materialize_with(base_dir: &Path, stop: StopHookRegistration) -> Option<PathBuf> {
    materialize_from_template(base_dir, stop, HOOK_SETTINGS)
}

/// [`materialize_with`] against an explicit settings template. Private seam:
/// `HOOK_SETTINGS` is an `include_str!` const, so this is the only way to
/// exercise the caller's half of the fail-open contract — a `None` build must
/// return `None` WITHOUT leaving a partial settings file behind.
fn materialize_from_template(
    base_dir: &Path,
    stop: StopHookRegistration,
    template: &str,
) -> Option<PathBuf> {
    if let Some(settings_path) = cached_materialization(base_dir, stop) {
        return Some(settings_path);
    }
    if let Err(e) = std::fs::create_dir_all(base_dir) {
        tracing::warn!(
            error = %e,
            dir = %base_dir.display(),
            "session-restore: claude hook dir create failed — --settings hook delivery off (identity still pinned)"
        );
        return None;
    }

    let script_path = base_dir.join(HOOK_SCRIPT_NAME);
    if let Err(e) = std::fs::write(&script_path, HOOK_SCRIPT.as_bytes()) {
        tracing::warn!(error = %e, path = %script_path.display(), "session-restore: claude hook script write failed");
        return None;
    }
    set_executable(&script_path);

    // Stop hook (continuation verdict, session-autonomy-fabric Phase 1) —
    // rides the SAME settings file, so it inherits the identical delivery +
    // fail-open posture as the SessionStart hook.
    let stop_script_path = base_dir.join(STOP_HOOK_SCRIPT_NAME);
    if let Err(e) = std::fs::write(&stop_script_path, STOP_HOOK_SCRIPT.as_bytes()) {
        tracing::warn!(error = %e, path = %stop_script_path.display(), "session-restore: claude stop-hook script write failed");
        return None;
    }
    set_executable(&stop_script_path);

    // PreCompact hook (context-exhaustion handoff, session-autonomy-fabric
    // Phase 7) — same carrier, same fail-open posture.
    let precompact_script_path = base_dir.join(PRECOMPACT_HOOK_SCRIPT_NAME);
    if let Err(e) = std::fs::write(&precompact_script_path, PRECOMPACT_HOOK_SCRIPT.as_bytes()) {
        tracing::warn!(error = %e, path = %precompact_script_path.display(), "session-restore: claude precompact-hook script write failed");
        return None;
    }
    set_executable(&precompact_script_path);

    // SessionStart policy injection (runner-enforced-policy-pull Phase 1) —
    // same carrier, same fail-open posture. Registered as a SECOND command in
    // the EXISTING `SessionStart` block, so the confirmation hook above keeps
    // its silent-stdout contract while this one carries the injected text.
    let policy_script_path = base_dir.join(POLICY_HOOK_SCRIPT_NAME);
    if let Err(e) = std::fs::write(&policy_script_path, POLICY_HOOK_SCRIPT.as_bytes()) {
        tracing::warn!(error = %e, path = %policy_script_path.display(), "session-restore: claude policy-hook script write failed");
        return None;
    }
    set_executable(&policy_script_path);

    // Build the settings by parsing the template, resolving each `command`'s
    // OWN `@@…@@` placeholder to that script's absolute path, and (when dark)
    // dropping the `Stop` key entirely. serde_json does the JSON escaping, so a
    // Windows path needs no hand-rolled backslash doubling.
    let settings = match build_settings(
        template,
        &script_path,
        &stop_script_path,
        &precompact_script_path,
        &policy_script_path,
        stop,
    ) {
        Some(s) => s,
        None => {
            tracing::warn!(
                path = %base_dir.join(HOOK_SETTINGS_NAME).display(),
                "session-restore: claude hook settings template malformed — --settings hook delivery off (identity still pinned)"
            );
            return None;
        }
    };
    let settings_path = base_dir.join(HOOK_SETTINGS_NAME);
    if let Err(e) = std::fs::write(&settings_path, settings.as_bytes()) {
        tracing::warn!(error = %e, path = %settings_path.display(), "session-restore: claude hook settings write failed");
        return None;
    }

    if let Ok(mut done) = MATERIALIZED.lock() {
        done.insert(base_dir.to_path_buf(), (stop, settings_path.clone()));
    }
    Some(settings_path)
}

/// Resolve every `command` in the template by the `@@…@@` placeholder it
/// ALREADY carries, mapping each to its materialized script's absolute path.
///
/// **Keyed on the placeholder, never on the event or the position.** One
/// `SessionStart` block holds TWO different scripts — the silent confirmation
/// hook and, since `2026-08-08-runner-enforced-policy-pull` Phase 1, the
/// policy-injection hook as a sibling command inside the same block. An
/// event-keyed substitution would give both the same path: policy injection
/// silently disabled, the confirmation hook run twice per session start, and
/// nothing anywhere to notice. That bug type-checks, so the shape of this
/// function is the thing preventing it.
///
/// Paths go in as JSON string CONTENT; serde_json escapes them on serialization,
/// which is what retired the hand-rolled `.replace('\', "\\\\")` this module
/// used to carry for Windows paths.
///
/// `None` on any unexpected shape — an event whose value is not a non-empty
/// array of matcher objects, a matcher with no non-empty `hooks` array, a hook
/// entry with no string `command`, or a `command` carrying NO known placeholder.
/// That last one is deliberate: an unrecognised placeholder would otherwise be
/// emitted verbatim, registering a hook that fails on every session start. This
/// module's contract is fail-OPEN (`None` ⇒ no `--settings`), never fail-broken.
fn resolve_commands(
    hooks: &mut serde_json::Map<String, serde_json::Value>,
    scripts: &[(&str, &Path)],
) -> Option<()> {
    for (_event, blocks) in hooks.iter_mut() {
        let matchers = blocks.as_array_mut()?;
        if matchers.is_empty() {
            return None;
        }
        for matcher in matchers.iter_mut() {
            let inner = matcher.as_object_mut()?.get_mut("hooks")?.as_array_mut()?;
            if inner.is_empty() {
                return None;
            }
            for entry in inner.iter_mut() {
                let obj = entry.as_object_mut()?;
                let command = obj.get("command")?.as_str()?;
                // EXACTLY ONE placeholder must match — not "the first that
                // matches". Order-dependent matching is a trap waiting for the
                // next placeholder to be added: today none of these is a
                // substring of another (`@@HOOK_SCRIPT@@` is not inside
                // `@@STOP_HOOK_SCRIPT@@` — the `STOP_` breaks the leading `@@`),
                // but that is a property of the current NAMES, not of the code,
                // and it would flip silently on a rename. An ambiguous or
                // unrecognised `command` fails open instead.
                let mut matched = scripts.iter().filter(|(ph, _)| command.contains(*ph));
                let (placeholder, script) = matched.next()?;
                if matched.next().is_some() {
                    return None;
                }
                let resolved = command.replace(placeholder, &script.display().to_string());
                obj.insert("command".to_string(), serde_json::Value::String(resolved));
            }
        }
    }
    Some(())
}

/// Pure: template + resolved script paths + variant → settings JSON.
///
/// `None` on a malformed or unexpected-shape template — never a panic. Every
/// shape assumption is an `Option` walk, so the caller ([`materialize_with`])
/// can log at `warn` and degrade to "no `--settings`" rather than writing a
/// partial file. Taking the template as a PARAMETER (rather than reading the
/// `include_str!` const) is what makes that fail-open path testable.
fn build_settings(
    template: &str,
    session: &Path,
    stop: &Path,
    precompact: &Path,
    policy: &Path,
    reg: StopHookRegistration,
) -> Option<String> {
    let mut root: serde_json::Value = serde_json::from_str(template).ok()?;
    {
        let hooks = root.get_mut("hooks")?.as_object_mut()?;
        if hooks.is_empty() {
            return None;
        }
        // Load-bearing in BOTH variants, so their ABSENCE is a malformed
        // template: identity pinning and policy injection ride `SessionStart`,
        // and `PreCompact` is unconditional by design. Checked by name because
        // the resolve below iterates whatever events the template HAS — it
        // cannot notice one that was never there.
        hooks.get("SessionStart")?;
        hooks.get("PreCompact")?;
        // Validate + resolve the WHOLE template first, including `Stop`, so a
        // malformed template fails open in BOTH variants rather than only when
        // armed.
        resolve_commands(
            hooks,
            &[
                (HOOK_SCRIPT_PLACEHOLDER, session),
                (STOP_HOOK_SCRIPT_PLACEHOLDER, stop),
                (PRECOMPACT_HOOK_SCRIPT_PLACEHOLDER, precompact),
                (POLICY_HOOK_SCRIPT_PLACEHOLDER, policy),
            ],
        )?;
        match reg {
            // Armed: the `Stop` registration is the whole point — a template
            // without it is malformed HERE, even though it is perfectly valid
            // for the dark arm below.
            StopHookRegistration::Registered => {
                hooks.get("Stop")?;
            }
            // Dark: drop the key so Claude never spawns `bash` per turn.
            //
            // SEMANTICS: a Stop-less template is a VALID DARK TEMPLATE. The dark
            // path must not require the very key it deletes — remove it if
            // present, otherwise carry on. (Requiring it would mean that
            // retiring the Stop hook from the template — the natural next change
            // here — silently dropped the whole `--settings` file, and with it
            // the SessionStart identity hook, the policy injection and the
            // coord-mcp pre-approval, for every session.)
            StopHookRegistration::Omitted => {
                hooks.remove("Stop");
            }
        }
    }
    serde_json::to_string_pretty(&root).ok()
}

/// Base dirs whose hook set this process has already materialized, mapped to
/// the registration variant it was built under and the settings path
/// [`materialize_with`] returned. The variant is part of the VALUE (not just
/// the key) so a mismatch is detectable and forces a rewrite.
static MATERIALIZED: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<PathBuf, (StopHookRegistration, PathBuf)>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Every file [`materialize`] is responsible for, in `base_dir`. Variant-
/// INDEPENDENT: the four scripts are written unconditionally (registration is
/// gated, materialization is not), so the same five paths exist for both
/// variants and the existence check below stays correct for each.
fn hook_files(base_dir: &Path) -> [PathBuf; 5] {
    [
        base_dir.join(HOOK_SCRIPT_NAME),
        base_dir.join(STOP_HOOK_SCRIPT_NAME),
        base_dir.join(PRECOMPACT_HOOK_SCRIPT_NAME),
        base_dir.join(POLICY_HOOK_SCRIPT_NAME),
        base_dir.join(HOOK_SETTINGS_NAME),
    ]
}

/// The settings path for `base_dir` if this process already materialized it
/// UNDER THE WANTED VARIANT and all five files are still present. `None` (⇒
/// full rewrite) otherwise, so an operator who deletes the dir gets it back on
/// the next spawn — and a variant change rewrites rather than serving a file
/// whose bytes were produced under the other flag state.
///
/// (Both production call sites share one `base_dir` — [`session_restore_dir`] —
/// so in prod this is a single cache entry.)
fn cached_materialization(base_dir: &Path, want: StopHookRegistration) -> Option<PathBuf> {
    let (cached_variant, settings_path) = MATERIALIZED.lock().ok()?.get(base_dir)?.clone();
    if cached_variant != want {
        return None;
    }
    if hook_files(base_dir).iter().all(|p| p.exists()) {
        Some(settings_path)
    } else {
        None
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::mcp::continuation_verdict::Mode;

    /// Everything that must hold for BOTH variants: all three scripts on disk,
    /// SessionStart + PreCompact registered and pointing at them, no
    /// unsubstituted placeholders, `permissions.allow` intact, and nothing
    /// outside the tempdir. Returns the parsed settings for variant-specific
    /// assertions.
    fn assert_variant_invariants(tmp: &Path, settings_path: &Path) -> serde_json::Value {
        // Settings file exists at the returned path with the expected name.
        assert!(settings_path.exists());
        assert_eq!(
            settings_path.file_name().unwrap().to_string_lossy(),
            HOOK_SETTINGS_NAME
        );

        // All three scripts exist alongside it — registration is gated,
        // MATERIALIZATION is not.
        let script_path = tmp.join(HOOK_SCRIPT_NAME);
        let stop_script_path = tmp.join(STOP_HOOK_SCRIPT_NAME);
        let precompact_script_path = tmp.join(PRECOMPACT_HOOK_SCRIPT_NAME);
        assert!(script_path.exists(), "hook script materialized");
        assert!(stop_script_path.exists(), "stop-hook script materialized");
        assert!(
            precompact_script_path.exists(),
            "precompact-hook script materialized"
        );
        let policy_script_path = tmp.join(POLICY_HOOK_SCRIPT_NAME);
        assert!(
            policy_script_path.exists(),
            "policy-hook script materialized"
        );

        // Settings is valid JSON with every placeholder substituted.
        let settings_text = std::fs::read_to_string(settings_path).unwrap();
        for placeholder in [
            HOOK_SCRIPT_PLACEHOLDER,
            STOP_HOOK_SCRIPT_PLACEHOLDER,
            PRECOMPACT_HOOK_SCRIPT_PLACEHOLDER,
            POLICY_HOOK_SCRIPT_PLACEHOLDER,
        ] {
            assert!(
                !settings_text.contains(placeholder),
                "{placeholder} substituted"
            );
        }
        let v: serde_json::Value = serde_json::from_str(&settings_text).unwrap();

        // SessionStart is load-bearing for identity pinning — never gated.
        let cmd = v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .expect("SessionStart registered");
        assert!(
            cmd.contains(HOOK_SCRIPT_NAME),
            "command runs our hook script"
        );

        // PreCompact reads no flag and fires per-compaction — never gated.
        let precompact_cmd = v["hooks"]["PreCompact"][0]["hooks"][0]["command"]
            .as_str()
            .expect("PreCompact registered");
        assert!(
            precompact_cmd.contains(PRECOMPACT_HOOK_SCRIPT_NAME),
            "PreCompact command runs our precompact-hook script"
        );

        // The precompact script POSTs to the context-low route, reads the seam
        // env, and never blocks the compaction (exit 0 everywhere).
        let precompact_text = std::fs::read_to_string(&precompact_script_path).unwrap();
        assert!(precompact_text.contains("/context-low"));
        assert!(precompact_text.contains("QONTINUI_RUNNER_API_PORT"));
        assert!(precompact_text.contains("QONTINUI_TERMINAL_ID"));

        // The SAME settings file registers the SessionStart POLICY-INJECTION
        // hook (runner-enforced-policy-pull Phase 1) as a SECOND command inside
        // the EXISTING `SessionStart` block — not a second `SessionStart` key,
        // which would be a distinct registration Claude has no obligation to
        // merge, and not an edit to the confirmation script, which must keep
        // its silent-stdout contract.
        //
        // THIS IS THE REGRESSION GUARD for the substitution key. Gating `Stop`
        // meant rebuilding the settings structurally, and the obvious structural
        // shape — one script per EVENT — gives both commands in this block the
        // same path: policy injection silently off, the confirmation hook run
        // twice. Asserting the SECOND command by name is what catches that.
        let session_start = v["hooks"]["SessionStart"]
            .as_array()
            .expect("SessionStart is an array");
        assert_eq!(
            session_start.len(),
            1,
            "exactly ONE SessionStart registration — the policy hook is a              sibling command inside it, never a second matcher block"
        );
        let session_start_cmds = session_start[0]["hooks"]
            .as_array()
            .expect("SessionStart block has a hooks array");
        assert_eq!(
            session_start_cmds.len(),
            2,
            "confirmation hook + policy hook share the one SessionStart block"
        );
        let policy_cmd = session_start_cmds[1]["command"].as_str().unwrap();
        assert!(
            policy_cmd.contains(POLICY_HOOK_SCRIPT_NAME),
            "second SessionStart command runs our POLICY-hook script, not the confirmation one"
        );
        assert!(
            !policy_cmd.contains(HOOK_SCRIPT_NAME),
            "the policy command must NOT have been overwritten with the confirmation script's path"
        );

        // The policy script GETs the policy-context route, reads the seam env,
        // and — the load-bearing difference from every other bundled hook —
        // PRINTS the runner's response, because its stdout IS the injection.
        let policy_text = std::fs::read_to_string(&policy_script_path).unwrap();
        assert!(policy_text.contains("/policy-context"));
        assert!(policy_text.contains("QONTINUI_RUNNER_API_PORT"));
        assert!(policy_text.contains("QONTINUI_TERMINAL_ID"));
        assert!(
            policy_text.contains("printf '%s' \"$resp\""),
            "the response is printed VERBATIM — the script builds no JSON"
        );

        // The confirmation hook stays silent. If this script ever grows a
        // stdout write, Claude would try to read it as a hook envelope and the
        // two SessionStart commands would fight over the same channel.
        let script_text = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            !script_text.contains("printf '%s' \"$resp\""),
            "claude_session_hook.sh must keep its silent-stdout contract"
        );

        // The SAME delivered settings file pre-approves the coord-mcp tools, so a
        // fresh user's first coord tool call isn't blocked by a per-tool prompt
        // (mcp-config-universal-provisioning Phase 2). This rides the `--settings`
        // the shim already appends — one file delivers both hook + pre-approval.
        // It is unrelated to hooks yet rides the same carrier, so it is the block
        // most likely to be lost by a settings rebuild: assert it in BOTH variants.
        let allow = v["permissions"]["allow"]
            .as_array()
            .expect("permissions.allow present");
        assert!(
            allow.iter().any(|a| a.as_str() == Some("mcp__coord-mcp")),
            "coord-mcp tools pre-approved in the delivered settings"
        );

        // The hook script POSTs to the control route and reads the seam env.
        let script_text = std::fs::read_to_string(&script_path).unwrap();
        assert!(script_text.contains("/control/session-open"));
        assert!(script_text.contains("QONTINUI_INSTALL_INTERCEPT_PORT"));
        assert!(script_text.contains("QONTINUI_TERMINAL_ID"));

        // The precompact script POSTs to the context-low route, reads the
        // seam env, and never blocks the compaction (exit 0 everywhere).
        let precompact_text = std::fs::read_to_string(&precompact_script_path).unwrap();
        assert!(precompact_text.contains("/context-low"));
        assert!(precompact_text.contains("QONTINUI_RUNNER_API_PORT"));
        assert!(precompact_text.contains("QONTINUI_TERMINAL_ID"));

        // The DELIVERY never writes to / reads from the user's config: every
        // materialized file lives under the runner's own app-data dir (the
        // tempdir here), NOT `~/.claude`. (The hook script's prose explains it
        // never touches `~/.claude`, so we assert on the materialized PATHS, the
        // load-bearing guarantee — not on the comment text.)
        let tmp_str = tmp.to_string_lossy();
        assert!(settings_path
            .to_string_lossy()
            .starts_with(tmp_str.as_ref()));
        assert!(script_path.to_string_lossy().starts_with(tmp_str.as_ref()));
        assert!(!settings_path.to_string_lossy().contains(".claude"));

        v
    }

    #[test]
    fn stop_hook_registration_maps_mode_fail_safe() {
        assert_eq!(
            StopHookRegistration::from_mode(Mode::Off),
            StopHookRegistration::Omitted
        );
        assert_eq!(
            StopHookRegistration::from_mode(Mode::Observe),
            StopHookRegistration::Registered
        );
        assert_eq!(
            StopHookRegistration::from_mode(Mode::On),
            StopHookRegistration::Registered
        );
        // `Mode`'s own fail-safe parse carries through: unset/empty/unknown ⇒
        // `Off` ⇒ `Omitted` (we never re-parse the string here).
        for raw in [None, Some(""), Some("  "), Some("banana"), Some("OFF")] {
            assert_eq!(
                StopHookRegistration::from_mode(Mode::from_flag(raw)),
                StopHookRegistration::Omitted,
                "dark for {raw:?}"
            );
        }
        assert_eq!(
            StopHookRegistration::from_mode(Mode::from_flag(Some(" ON "))),
            StopHookRegistration::Registered
        );
    }

    #[test]
    fn materialize_registered_writes_all_files_and_registers_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let settings_path =
            materialize_with(tmp.path(), StopHookRegistration::Registered).expect("materialize ok");
        let v = assert_variant_invariants(tmp.path(), &settings_path);

        // The SAME settings file registers the Stop continuation-verdict hook
        // (session-autonomy-fabric Phase 1) pointing at the materialized stop
        // script — one carrier delivers SessionStart + Stop + PreCompact +
        // pre-approval.
        let stop_cmd = v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .expect("Stop registered when armed");
        assert!(
            stop_cmd.contains(STOP_HOOK_SCRIPT_NAME),
            "Stop command runs our stop-hook script"
        );

        // The stop script POSTs to the verdict route, reads the seam env, and
        // carries the fail-open loop guard (never re-block a hook-forced
        // continuation).
        let stop_text = std::fs::read_to_string(tmp.path().join(STOP_HOOK_SCRIPT_NAME)).unwrap();
        assert!(stop_text.contains("/continuation-verdict"));
        assert!(stop_text.contains("QONTINUI_RUNNER_API_PORT"));
        assert!(stop_text.contains("QONTINUI_TERMINAL_ID"));
        assert!(stop_text.contains("stop_hook_active"));
    }

    #[test]
    fn materialize_omitted_drops_the_stop_key_and_keeps_everything_else() {
        let tmp = tempfile::tempdir().unwrap();
        let settings_path =
            materialize_with(tmp.path(), StopHookRegistration::Omitted).expect("materialize ok");
        let v = assert_variant_invariants(tmp.path(), &settings_path);

        // THE POINT: a dark session gets no `Stop` key at all, so Claude never
        // spawns `bash` for it once per assistant turn.
        assert!(
            v["hooks"]["Stop"].is_null(),
            "no Stop registration when the continuation flag is dark"
        );
        assert!(
            !v["hooks"].as_object().unwrap().contains_key("Stop"),
            "the Stop key is REMOVED, not merely nulled"
        );
    }

    /// The cache is keyed on `base_dir` alone unless the registration variant
    /// is carried too — and both variants write the SAME filename into the SAME
    /// dir with DIFFERENT content. Without the variant check this returns the
    /// `Registered` file's bytes for an `Omitted` request.
    #[test]
    fn cache_does_not_serve_the_other_variants_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let armed = materialize_with(tmp.path(), StopHookRegistration::Registered).unwrap();
        let dark = materialize_with(tmp.path(), StopHookRegistration::Omitted).unwrap();
        assert_eq!(armed, dark, "same settings filename in the same base_dir");

        // Read back from DISK — the cache must have fallen through to a rewrite.
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dark).unwrap()).unwrap();
        assert!(
            v["hooks"]["Stop"].is_null(),
            "variant change rewrote the file; the cache did not serve stale bytes"
        );

        // ...and back again, so the mismatch check is not one-directional.
        let armed_again = materialize_with(tmp.path(), StopHookRegistration::Registered).unwrap();
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&armed_again).unwrap()).unwrap();
        assert!(
            v2["hooks"]["Stop"][0]["hooks"][0]["command"].is_string(),
            "re-arming rewrites the Stop registration back in"
        );
    }

    #[test]
    fn materialize_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let a = materialize(tmp.path()).unwrap();
        let b = materialize(tmp.path()).unwrap();
        assert_eq!(a, b, "stable settings path across calls");
        assert!(a.exists());
    }

    #[test]
    fn materialize_omitted_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let a = materialize_with(tmp.path(), StopHookRegistration::Omitted).unwrap();
        let before = std::fs::read_to_string(&a).unwrap();
        let b = materialize_with(tmp.path(), StopHookRegistration::Omitted).unwrap();
        assert_eq!(a, b, "stable settings path across calls");
        assert!(a.exists());
        assert_eq!(
            before,
            std::fs::read_to_string(&b).unwrap(),
            "second call does not thrash the file"
        );
    }

    /// The manual `.replace('\\', "\\\\")` is gone — serde_json now does the
    /// escaping. Assert on the PARSED value (round-trip + basename), never on
    /// the raw escaped bytes, which differ per platform.
    #[test]
    fn emitted_settings_round_trip_with_platform_native_paths() {
        for reg in [
            StopHookRegistration::Registered,
            StopHookRegistration::Omitted,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let settings_path = materialize_with(tmp.path(), reg).unwrap();
            let text = std::fs::read_to_string(&settings_path).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&text).expect("emitted settings is valid JSON");

            let script_path = tmp.path().join(HOOK_SCRIPT_NAME);
            let expected = script_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let cmd = v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert!(
                cmd.contains(&expected),
                "{reg:?}: parsed command carries the script file_name"
            );
            // The PARSED value is the real path — no doubled separators left
            // over from a hand-rolled escape.
            assert!(
                cmd.contains(&script_path.display().to_string()),
                "{reg:?}: parsed command is the platform-native absolute path"
            );
        }
    }

    /// Fail-open is the invariant the whole module rests on: a malformed or
    /// unexpected-shape template returns `None`, never a panic, so delivery
    /// degrades to "no `--settings`" and a session can always still start.
    #[test]
    fn build_settings_fails_open_on_malformed_templates() {
        let p = Path::new("/tmp/x.sh");
        let bad = [
            ("invalid json", "{ not json"),
            ("empty", ""),
            ("not an object", "[]"),
            ("hooks absent", r#"{"permissions":{"allow":[]}}"#),
            ("hooks not an object", r#"{"hooks":[]}"#),
            ("SessionStart absent", r#"{"hooks":{"Stop":[]}}"#),
            (
                "Stop an empty array",
                r##"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash '@@HOOK_SCRIPT@@'"}]}],"PreCompact":[{"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}],"Stop":[]}}"##,
            ),
            (
                "Stop entry has no inner hooks array",
                r##"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash '@@HOOK_SCRIPT@@'"}]}],"PreCompact":[{"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}],"Stop":[{}]}}"##,
            ),
            (
                "SessionStart inner hooks empty",
                r#"{"hooks":{"SessionStart":[{"hooks":[]}]}}"#,
            ),
            // A `command` carrying a placeholder this module does not know
            // would otherwise be emitted VERBATIM, registering a hook that
            // fails on every session start. Fail open instead.
            (
                "unknown placeholder",
                r##"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash '@@WHO_KNOWS@@'"}]}],"PreCompact":[{"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}]}}"##,
            ),
            (
                "command is not a string",
                r##"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":42}]}],"PreCompact":[{"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}]}}"##,
            ),
            // PreCompact is load-bearing in both variants; a template missing
            // it entirely is malformed, and generic iteration cannot notice an
            // event that was never there.
            (
                "PreCompact absent",
                r##"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash '@@HOOK_SCRIPT@@'"}]}]}}"##,
            ),
        ];
        for (name, template) in bad {
            for reg in [
                StopHookRegistration::Registered,
                StopHookRegistration::Omitted,
            ] {
                assert!(
                    build_settings(template, p, p, p, p, reg).is_none(),
                    "{name} ({reg:?}) must fail open with None, not panic or partial output"
                );
            }
        }
    }

    /// The caller's half of the fail-open contract: a `None` build must NOT
    /// leave a partial settings file behind.
    #[test]
    fn shipped_template_builds_and_a_none_build_writes_nothing() {
        // Sanity: the real bundled template is well-formed in both variants —
        // otherwise the negative test above would pass vacuously.
        let p = Path::new("/tmp/x.sh");
        for reg in [
            StopHookRegistration::Registered,
            StopHookRegistration::Omitted,
        ] {
            assert!(
                build_settings(HOOK_SETTINGS, p, p, p, p, reg).is_some(),
                "the bundled template builds under {reg:?}"
            );
        }

        // And the caller propagates a `None` build as `None` WITHOUT writing a
        // partial settings file — while the scripts (materialization, not
        // registration) are still written, and the cache is not poisoned.
        let tmp = tempfile::tempdir().unwrap();
        let settings_path = tmp.path().join(HOOK_SETTINGS_NAME);
        for reg in [
            StopHookRegistration::Registered,
            StopHookRegistration::Omitted,
        ] {
            assert!(
                materialize_from_template(tmp.path(), reg, "{ not json").is_none(),
                "{reg:?}: a malformed template degrades to no --settings"
            );
            assert!(
                !settings_path.exists(),
                "{reg:?}: no partial settings file written"
            );
            // Materialization is NOT gated on the build succeeding.
            assert!(tmp.path().join(HOOK_SCRIPT_NAME).exists());
            assert!(tmp.path().join(STOP_HOOK_SCRIPT_NAME).exists());
            assert!(tmp.path().join(PRECOMPACT_HOOK_SCRIPT_NAME).exists());
        }

        // A later good build still works — the failed attempt cached nothing.
        let ok = materialize_with(tmp.path(), StopHookRegistration::Omitted);
        assert_eq!(ok.as_deref(), Some(settings_path.as_path()));
        assert!(settings_path.exists());
    }

    #[test]
    fn session_restore_dir_is_under_qontinui_runner_not_dot_claude() {
        let dir = session_restore_dir();
        let s = dir.to_string_lossy();
        assert!(s.contains("runner"), "lives under ~/.qontinui/runner");
        assert!(s.ends_with("session-restore"));
        assert!(!s.contains(".claude"), "NEVER under the user's ~/.claude");
    }
}
