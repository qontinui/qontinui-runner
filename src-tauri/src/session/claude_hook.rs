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
//!   * `claude_hook_settings.json` / `claude_hook_settings-nostop.json` —
//!     `{ "hooks": { "SessionStart": [...], "PreCompact": [...], "Stop": [...] } }`
//!     pointing each `command` at the corresponding materialized script. The
//!     `Stop` key is registered ONLY when the continuation flag is armed (see
//!     [`StopHookRegistration`]); a dark session gets the `-nostop` file, which
//!     has no `Stop` key at all, so Claude never spawns `bash` for it once per
//!     assistant turn. The two variants use DISTINCT FILENAMES because the hook
//!     dir is machine-global — see [`session_restore_dir`].
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
/// `QONTINUI_POLICY_INJECTION` — which, unlike its Stop/PreCompact siblings,
/// defaults to **`on`**: this hook IS live for every session unless the flag
/// says `off` out loud. (It was `off` when the hook shipped, so that the
/// unproven SessionStart seam could land safely; the default was flipped once
/// delivering the policy became the desired baseline.)
const POLICY_HOOK_SCRIPT: &str =
    include_str!("../../resources/session-restore/claude_policy_hook.sh");
/// Settings template (bundled). [`build_settings`] parses it and resolves each
/// `command` by the `@@…@@` PLACEHOLDER that command already carries —
/// `@@HOOK_SCRIPT@@`, `@@STOP_HOOK_SCRIPT@@`, `@@PRECOMPACT_HOOK_SCRIPT@@`,
/// `@@POLICY_HOOK_SCRIPT@@` — swapping in the matching materialized script's
/// absolute path.
///
/// **The placeholders are load-bearing, not decoration.** They are the
/// substitution KEY, which is what lets ONE `SessionStart` block carry TWO
/// different scripts (the confirmation hook and the policy-injection hook).
/// Keying on the event instead would point both at the same path. Renaming one
/// here without renaming its `*_PLACEHOLDER` const makes the build fail open —
/// no `--settings` at all — rather than emitting a broken hook.
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
/// File name of the materialized `--settings` file for the ARMED variant
/// ([`StopHookRegistration::Registered`]).
const HOOK_SETTINGS_NAME: &str = "claude_hook_settings.json";
/// File name of the materialized `--settings` file for the DARK variant
/// ([`StopHookRegistration::Omitted`]).
///
/// The two variants get DISTINCT FILENAMES on purpose. [`session_restore_dir`]
/// is machine-global and deliberately unscoped — every runner instance on the
/// box (primary, secondary, supervisor-spawned temp runners) shares it. Before
/// registration gating the content was a pure function of `base_dir`, so every
/// instance wrote byte-identical bytes and the collision was benign; now it is
/// a function of each PROCESS's flag, so one filename would let a dark temp
/// runner silently rewrite the armed primary's settings (and vice versa) while
/// the other process's cache still reports a hit. Separate names make that
/// impossible by construction rather than narrowing the race — the same reason
/// `coord_mcp.rs` keys its `--mcp-config` filename per workdir+terminal.
const HOOK_SETTINGS_NAME_NOSTOP: &str = "claude_hook_settings-nostop.json";

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

    /// The `--settings` file name this variant materializes. Distinct per
    /// variant — see [`HOOK_SETTINGS_NAME_NOSTOP`] for why.
    fn settings_name(self) -> &'static str {
        match self {
            StopHookRegistration::Registered => HOOK_SETTINGS_NAME,
            StopHookRegistration::Omitted => HOOK_SETTINGS_NAME_NOSTOP,
        }
    }

    /// Stable wire string for the variant, for diagnostics that must name WHICH
    /// carrier a session would get. Distinct vocabulary from
    /// [`crate::mcp::continuation_verdict::Mode::as_str`] on purpose: two of the
    /// three modes collapse to one registration, so reporting the mode would not
    /// name the file.
    pub fn as_str(self) -> &'static str {
        match self {
            StopHookRegistration::Registered => "registered",
            StopHookRegistration::Omitted => "omitted",
        }
    }
}

/// The absolute path of the `--settings` carrier `base_dir` holds for `reg`.
///
/// THE one definition of that path. [`materialize_from_template`] writes it,
/// [`hook_files`] stats it and `config_report_cmd::claude_settings_carrier_reading`
/// REPORTS it, all through here — so a diagnostic that names the carrier cannot
/// name a different file than the spawn seam writes. A second copy of
/// `base_dir.join(reg.settings_name())` would compile, agree on the day it was
/// written, and start lying the first time either half moved, which is exactly
/// the defect class the config report exists to expose.
pub fn settings_path(base_dir: &Path, reg: StopHookRegistration) -> PathBuf {
    base_dir.join(reg.settings_name())
}

/// Env var the runner injects at spawn carrying the absolute path of the
/// materialized Claude `--settings` hook file. The identity shim's `claude`
/// wrapper reads it and appends `--settings $that` to the real argv. Empty/unset
/// ⇒ the shim appends nothing (fail-open — identity still rides `--session-id`).
pub const CLAUDE_SETTINGS_ENV: &str = "QONTINUI_CLAUDE_HOOK_SETTINGS";

/// The runner's OWN app-data dir for session-restore artifacts —
/// `~/.qontinui/runner/session-restore/`. Co-located with the lifecycle store +
/// shutdown marker (`~/.qontinui/runner/`). NEVER `~/.claude`.
///
/// MACHINE-GLOBAL AND DELIBERATELY UNSCOPED: every runner instance on the box
/// shares this one dir (same posture as `session_lifecycle_store`'s hook dir).
/// Anything written here whose CONTENT varies per process must therefore carry
/// that variation in its FILE NAME — which is why the settings file is named
/// per [`StopHookRegistration`] variant.
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
/// registration variant — the two variants write DIFFERENT filenames with
/// different content, so the cache carries the variant both to key the right
/// file names and to rewrite rather than serve a path built under the other
/// flag state. Every terminal spawn used to rewrite all of them plus a `chmod`
/// each; after the first materialize in this process a spawn with the SAME
/// variant costs one `stat` per file (proving they are still there) and nothing
/// else. An externally deleted or modified file — or a variant mismatch — falls
/// straight through to a full rewrite, so the cache can never serve a path that
/// is not on disk or whose content disagrees with the wanted variant.
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
                path = %settings_path(base_dir, stop).display(),
                "session-restore: claude hook settings template malformed — --settings hook delivery off (identity still pinned)"
            );
            return None;
        }
    };
    let settings_path = settings_path(base_dir, stop);
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
        // SETTLE `Stop` BEFORE resolving, never after. Order is load-bearing:
        // under the dark variant the key is DELETED, so a malformed `Stop` block
        // never reaches the delivered file and must not be able to veto it.
        // Validating first would trade the SessionStart identity hook, the
        // policy injection and the coord-mcp pre-approval — the whole
        // `--settings` file — for a block we were about to throw away.
        match reg {
            // Armed: the `Stop` registration is the whole point, so a template
            // without it is malformed HERE, even though it is perfectly valid
            // for the dark arm below. Its SHAPE is then validated by the resolve.
            StopHookRegistration::Registered => {
                hooks.get("Stop")?;
            }
            // Dark: drop the key so Claude never spawns `bash` per turn.
            //
            // SEMANTICS: a Stop-less template is a VALID DARK TEMPLATE. The dark
            // path must not require the very key it deletes — remove it if
            // present, otherwise carry on. (Requiring it would mean that
            // retiring the Stop hook from the template — the natural next change
            // here — silently dropped the whole `--settings` file for every
            // session.)
            StopHookRegistration::Omitted => {
                hooks.remove("Stop");
            }
        }
        resolve_commands(
            hooks,
            &[
                (HOOK_SCRIPT_PLACEHOLDER, session),
                (STOP_HOOK_SCRIPT_PLACEHOLDER, stop),
                (PRECOMPACT_HOOK_SCRIPT_PLACEHOLDER, precompact),
                (POLICY_HOOK_SCRIPT_PLACEHOLDER, policy),
            ],
        )?;
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

/// Every file a [`materialize`] under `reg` is responsible for, in `base_dir`.
///
/// The four SCRIPTS are variant-INDEPENDENT (they are written unconditionally
/// — registration is gated, materialization is not), so both variants leave all
/// four on disk. The SETTINGS entry FOLLOWS THE VARIANT, so the existence
/// check below validates the file this variant actually delivers rather than
/// the other one's.
///
/// A stale settings file from the OTHER variant may sit in the dir alongside
/// (a machine-global dir shared with other runner instances, or this process
/// after a flag flip). That is fine and harmless — every instance's spawn
/// points `--settings` at its own name — so DO NOT "clean it up": deleting the
/// other variant's file would be reaching into another live process's delivery.
fn hook_files(base_dir: &Path, reg: StopHookRegistration) -> [PathBuf; 5] {
    [
        base_dir.join(HOOK_SCRIPT_NAME),
        base_dir.join(STOP_HOOK_SCRIPT_NAME),
        base_dir.join(PRECOMPACT_HOOK_SCRIPT_NAME),
        base_dir.join(POLICY_HOOK_SCRIPT_NAME),
        settings_path(base_dir, reg),
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
///
/// The check is EXISTENCE-only, never content: this process's cache says
/// nothing about what another runner instance sharing the machine-global dir
/// may have written. What makes that safe is the per-variant FILE NAME, not
/// this cache — no other instance writes the name this variant reads.
fn cached_materialization(base_dir: &Path, want: StopHookRegistration) -> Option<PathBuf> {
    let (cached_variant, settings_path) = MATERIALIZED.lock().ok()?.get(base_dir)?.clone();
    if cached_variant != want {
        return None;
    }
    if hook_files(base_dir, want).iter().all(|p| p.exists()) {
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
    fn assert_variant_invariants(
        tmp: &Path,
        settings_path: &Path,
        reg: StopHookRegistration,
    ) -> serde_json::Value {
        // Settings file exists at the returned path with the VARIANT's name.
        assert!(settings_path.exists());
        assert_eq!(
            settings_path.file_name().unwrap().to_string_lossy(),
            reg.settings_name()
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
        let v =
            assert_variant_invariants(tmp.path(), &settings_path, StopHookRegistration::Registered);

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
        let v =
            assert_variant_invariants(tmp.path(), &settings_path, StopHookRegistration::Omitted);

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

    /// FIX 1 — the hook dir is MACHINE-GLOBAL (every runner instance on the box
    /// shares it), so the two variants must not share a filename. If they did,
    /// a dark temp runner rewriting the file would silently disarm an armed
    /// primary whose own per-process cache still reports a hit (variant matches,
    /// four files exist) and therefore never rewrites. Distinct names make the
    /// collision impossible by construction.
    #[test]
    fn the_two_variants_write_distinct_settings_files_that_coexist() {
        let tmp = tempfile::tempdir().unwrap();
        let armed = materialize_with(tmp.path(), StopHookRegistration::Registered).unwrap();
        let dark = materialize_with(tmp.path(), StopHookRegistration::Omitted).unwrap();
        assert_ne!(
            armed, dark,
            "the two variants must not share one machine-global filename"
        );

        // BOTH survive on disk with their own content — the second materialize
        // did not overwrite the first's delivery.
        let armed_v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&armed).unwrap()).unwrap();
        let dark_v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dark).unwrap()).unwrap();
        assert!(
            armed_v["hooks"]["Stop"][0]["hooks"][0]["command"].is_string(),
            "the ARMED file still registers Stop after a dark materialize ran in the same dir"
        );
        assert!(
            !dark_v["hooks"].as_object().unwrap().contains_key("Stop"),
            "the DARK file has no Stop key"
        );

        // Simulate the cross-instance case the shared dir makes possible: the
        // OTHER variant's file is clobbered by another process. The armed
        // instance's cached path is unaffected, because it is a different file.
        std::fs::write(&dark, "{}").unwrap();
        let armed_again = materialize_with(tmp.path(), StopHookRegistration::Registered).unwrap();
        assert_eq!(armed_again, armed);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&armed_again).unwrap()).unwrap();
        assert!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"].is_string(),
            "another instance's write cannot disarm this instance's settings"
        );

        // Both variants leave all three SCRIPTS on disk (materialization is not
        // gated, only registration is).
        for name in [
            HOOK_SCRIPT_NAME,
            STOP_HOOK_SCRIPT_NAME,
            PRECOMPACT_HOOK_SCRIPT_NAME,
        ] {
            assert!(tmp.path().join(name).exists(), "{name} materialized");
        }
    }

    /// The cache's real contract, not the tautology it replaced: `materialize`
    /// returns `base_dir.join(<name>)`, a pure function of its arguments, so
    /// `a == b` would hold with the cache deleted entirely. Observe the
    /// SHORT-CIRCUIT itself (a cached call does not rewrite) and the EXISTENCE
    /// check (a deleted file falls through to a full rewrite).
    #[test]
    fn a_cache_hit_skips_the_rewrite_and_a_deleted_file_forces_one() {
        let tmp = tempfile::tempdir().unwrap();
        let a = materialize_with(tmp.path(), StopHookRegistration::Registered).unwrap();
        let original = std::fs::read_to_string(&a).unwrap();

        // Cache HIT is observable: clobber the settings content, call again with
        // the same (base_dir, variant) — all four files still exist, so the call
        // short-circuits and our clobbered bytes are still there.
        std::fs::write(&a, "{\"clobbered\":true}").unwrap();
        let b = materialize_with(tmp.path(), StopHookRegistration::Registered).unwrap();
        assert_eq!(a, b, "stable settings path across calls");
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "{\"clobbered\":true}",
            "a cached call must not rewrite the four files"
        );

        // Cache MISS on a missing file: delete the settings file and the next
        // call falls through to a full rewrite that restores it.
        std::fs::remove_file(&a).unwrap();
        let c = materialize_with(tmp.path(), StopHookRegistration::Registered).unwrap();
        assert_eq!(a, c);
        assert_eq!(
            std::fs::read_to_string(&c).unwrap(),
            original,
            "an externally deleted file forces a rewrite"
        );

        // Same for a deleted SCRIPT — the existence check covers all four.
        std::fs::write(&c, "{\"clobbered\":true}").unwrap();
        std::fs::remove_file(tmp.path().join(STOP_HOOK_SCRIPT_NAME)).unwrap();
        let d = materialize_with(tmp.path(), StopHookRegistration::Registered).unwrap();
        assert_eq!(
            std::fs::read_to_string(&d).unwrap(),
            original,
            "a deleted script forces the settings rewrite too"
        );
        assert!(tmp.path().join(STOP_HOOK_SCRIPT_NAME).exists());
    }

    /// The production entry point is the env wrapper, so pin that it picks the
    /// variant — and therefore the delivered FILE — from the live flag.
    #[test]
    fn materialize_reads_the_live_flag_and_delivers_that_variants_file() {
        use crate::mcp::continuation_verdict::FLAG_ENV;
        let _lock = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[FLAG_ENV]);

        let dark_dir = tempfile::tempdir().unwrap();
        std::env::remove_var(FLAG_ENV);
        let dark = materialize(dark_dir.path()).unwrap();
        assert_eq!(
            dark.file_name().unwrap().to_string_lossy(),
            HOOK_SETTINGS_NAME_NOSTOP,
            "the DEFAULT posture delivers the no-Stop settings file"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dark).unwrap()).unwrap();
        assert!(!v["hooks"].as_object().unwrap().contains_key("Stop"));

        let armed_dir = tempfile::tempdir().unwrap();
        std::env::set_var(FLAG_ENV, "observe");
        let armed = materialize(armed_dir.path()).unwrap();
        assert_eq!(
            armed.file_name().unwrap().to_string_lossy(),
            HOOK_SETTINGS_NAME,
            "an armed flag delivers the Stop-registering settings file"
        );
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
            // A SECOND matcher block in one event, malformed. The first is
            // well-formed, so this fixture fails for the reason it NAMES rather
            // than tripping an earlier check.
            (
                "SessionStart second matcher malformed",
                r##"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash '@@HOOK_SCRIPT@@'"}]},{"hooks":[]}],"PreCompact":[{"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}]}}"##,
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

        // A malformed `Stop` BLOCK is fatal only when it must be REGISTERED.
        // When dark the key is dropped unread, so the settings still build —
        // the same rule as a template with no `Stop` key at all.
        let bad_stop_only = [
            (
                "Stop an empty array",
                r##"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash '@@HOOK_SCRIPT@@'"}]}],"PreCompact":[{"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}],"Stop":[]}}"##,
            ),
            (
                "Stop entry has no inner hooks array",
                r##"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash '@@HOOK_SCRIPT@@'"}]}],"PreCompact":[{"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}],"Stop":[{}]}}"##,
            ),
        ];
        for (name, template) in bad_stop_only {
            assert!(
                build_settings(template, p, p, p, p, StopHookRegistration::Registered).is_none(),
                "{name} must fail open when the Stop hook is being registered"
            );
            let out = build_settings(template, p, p, p, p, StopHookRegistration::Omitted)
                .unwrap_or_else(|| panic!("{name}: a dropped Stop key is not a build failure"));
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert!(!v["hooks"].as_object().unwrap().contains_key("Stop"));
        }
    }

    /// FIX 2 — the dark path must not require the very key it deletes. A
    /// template with NO `Stop` key is a valid DARK template: returning `None`
    /// there would drop the whole `--settings` file, taking the SessionStart
    /// identity hook and the coord-mcp pre-approval with it, for every session
    /// in the DEFAULT posture.
    #[test]
    fn a_stop_less_template_is_a_valid_dark_template() {
        const NO_STOP: &str = r#"{
            "hooks": {
              "SessionStart": [{"hooks":[{"type":"command","command":"bash '@@HOOK_SCRIPT@@'"}]}],
              "PreCompact": [{"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}]
            },
            "permissions": { "allow": ["mcp__coord-mcp"] }
        }"#;
        let session = Path::new("/x/claude_session_hook.sh");
        let stop = Path::new("/x/claude_stop_hook.sh");
        let precompact = Path::new("/x/claude_precompact_hook.sh");
        let policy = Path::new("/x/claude_policy_hook.sh");

        let out = build_settings(
            NO_STOP,
            session,
            stop,
            precompact,
            policy,
            StopHookRegistration::Omitted,
        )
        .expect("a Stop-less template builds in the dark variant");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert!(
            !v["hooks"].as_object().unwrap().contains_key("Stop"),
            "no Stop key appears out of nowhere"
        );
        assert_eq!(
            v["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str(),
            Some(format!("bash '{}'", session.display()).as_str()),
            "the identity-confirmation hook survives"
        );
        assert_eq!(
            v["hooks"]["PreCompact"][0]["hooks"][0]["command"].as_str(),
            Some(format!("bash '{}'", precompact.display()).as_str()),
            "the PreCompact hook survives"
        );
        assert!(
            v["permissions"]["allow"]
                .as_array()
                .expect("permissions.allow survives")
                .iter()
                .any(|a| a.as_str() == Some("mcp__coord-mcp")),
            "the coord-mcp pre-approval survives"
        );

        // The ARMED variant still fails open on it — there is nothing to
        // register, and delivering a settings file that claims to arm the hook
        // while not arming it would be worse than no `--settings`.
        assert!(
            build_settings(
                NO_STOP,
                session,
                stop,
                precompact,
                policy,
                StopHookRegistration::Registered
            )
            .is_none(),
            "no Stop key to register ⇒ the armed build fails open"
        );
    }

    /// FIX 5(c) — the substitution→parse/serialize rewrite is exactly the change
    /// a snapshot guards. Assert on PARSED values (key order and whitespace are
    /// irrelevant): the armed output registers EXACTLY the three events, each
    /// pointing at its own script, with `permissions.allow` intact.
    #[test]
    fn armed_settings_registers_exactly_the_three_events_with_the_expected_commands() {
        let session = Path::new("/x/claude_session_hook.sh");
        let stop = Path::new("/x/claude_stop_hook.sh");
        let precompact = Path::new("/x/claude_precompact_hook.sh");
        let policy = Path::new("/x/claude_policy_hook.sh");
        let out = build_settings(
            HOOK_SETTINGS,
            session,
            stop,
            precompact,
            policy,
            StopHookRegistration::Registered,
        )
        .expect("the bundled template builds when armed");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();

        let hooks = v["hooks"].as_object().expect("hooks object");
        let mut events: Vec<&str> = hooks.keys().map(String::as_str).collect();
        events.sort_unstable();
        assert_eq!(
            events,
            ["PreCompact", "SessionStart", "Stop"],
            "exactly the three events, no more and no fewer"
        );

        // Command COUNT per event, stated explicitly. `SessionStart` carries
        // TWO — the silent confirmation hook and the policy-injection hook that
        // rides beside it — while `Stop` and `PreCompact` carry one each. A
        // blanket "one command per event" would have been true before
        // `2026-08-08-runner-enforced-policy-pull` Phase 1 and is now exactly
        // the assumption this module must not make.
        for (event, commands) in [("SessionStart", 2), ("Stop", 1), ("PreCompact", 1)] {
            let blocks = hooks[event].as_array().unwrap();
            assert_eq!(blocks.len(), 1, "{event}: one matcher block");
            let inner = blocks[0]["hooks"].as_array().unwrap();
            assert_eq!(inner.len(), commands, "{event}: command count");
            for entry in inner {
                assert_eq!(entry["type"].as_str(), Some("command"), "{event}: type");
            }
        }

        // ...and each command points at its OWN script — addressed by position,
        // because the two `SessionStart` siblings are the pair a per-event
        // substitution would silently collapse onto one path.
        for (event, index, script) in [
            ("SessionStart", 0, session),
            ("SessionStart", 1, policy),
            ("Stop", 0, stop),
            ("PreCompact", 0, precompact),
        ] {
            assert_eq!(
                hooks[event][0]["hooks"][index]["command"].as_str(),
                Some(format!("bash '{}'", script.display()).as_str()),
                "{event}[{index}]: command points at its OWN script"
            );
        }

        assert_eq!(
            v["permissions"]["allow"].as_array(),
            Some(&vec![serde_json::Value::String("mcp__coord-mcp".into())]),
            "permissions.allow intact"
        );
    }

    /// FIX 3 — the old `String::replace` substituted EVERY occurrence. The
    /// parse-and-set rewrite must too: a second matcher block or a second inner
    /// hook would otherwise ship with its placeholder `command` intact, i.e. a
    /// registered hook that runs `bash '@@HOOK_SCRIPT@@'` and fails at runtime
    /// on every session start — the opposite of this module's fail-open design.
    #[test]
    fn every_matcher_and_every_inner_hook_gets_its_command_set() {
        const MULTI: &str = r#"{
            "hooks": {
              "SessionStart": [
                {"matcher":"startup","hooks":[
                   {"type":"command","command":"bash '@@HOOK_SCRIPT@@'"},
                   {"type":"command","command":"bash '@@POLICY_HOOK_SCRIPT@@'"}]},
                {"matcher":"resume","hooks":[
                   {"type":"command","command":"bash '@@HOOK_SCRIPT@@'"}]}
              ],
              "PreCompact": [
                {"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]},
                {"hooks":[{"type":"command","command":"bash '@@PRECOMPACT_HOOK_SCRIPT@@'"}]}
              ],
              "Stop": [
                {"hooks":[{"type":"command","command":"bash '@@STOP_HOOK_SCRIPT@@'"}]},
                {"hooks":[{"type":"command","command":"bash '@@STOP_HOOK_SCRIPT@@'"}]}
              ]
            }
        }"#;
        let session = Path::new("/x/claude_session_hook.sh");
        let stop = Path::new("/x/claude_stop_hook.sh");
        let precompact = Path::new("/x/claude_precompact_hook.sh");
        let policy = Path::new("/x/claude_policy_hook.sh");
        let out = build_settings(
            MULTI,
            session,
            stop,
            precompact,
            policy,
            StopHookRegistration::Registered,
        )
        .expect("a multi-block template builds");
        assert!(
            !out.contains("@@"),
            "no placeholder survives into the delivered settings"
        );

        let v: serde_json::Value = serde_json::from_str(&out).unwrap();

        // Block COUNTS first, so a template that lost a matcher cannot pass the
        // per-entry checks below by simply not having the entry.
        for (event, blocks) in [("SessionStart", 2), ("PreCompact", 2), ("Stop", 2)] {
            assert_eq!(
                v["hooks"][event].as_array().unwrap().len(),
                blocks,
                "{event}: every matcher block survives"
            );
        }

        // EVERY entry, addressed individually — including the two SIBLINGS in
        // one block that carry DIFFERENT placeholders. A loop that asserted one
        // script per EVENT would pass while those two were collapsed onto the
        // same path, which is precisely the bug this module has to not have.
        let want = |script: &Path| format!("bash '{}'", script.display());
        for (event, block, entry, script) in [
            ("SessionStart", 0, 0, session),
            ("SessionStart", 0, 1, policy),
            ("SessionStart", 1, 0, session),
            ("PreCompact", 0, 0, precompact),
            ("PreCompact", 1, 0, precompact),
            ("Stop", 0, 0, stop),
            ("Stop", 1, 0, stop),
        ] {
            assert_eq!(
                v["hooks"][event][block]["hooks"][entry]["command"].as_str(),
                Some(want(script).as_str()),
                "{event} block {block} entry {entry} resolves to its OWN script"
            );
        }
    }

    /// FIX 5(a) — the commit's HEADLINE behaviour is "default posture ⇒
    /// omitted", and `from_env` is the exact entry point production calls
    /// ([`materialize`]). Without this, a mutant `from_env() { Registered }`
    /// passes every other test in this module.
    #[test]
    fn stop_hook_registration_from_env_is_dark_by_default() {
        use crate::mcp::continuation_verdict::FLAG_ENV;
        let _lock = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[FLAG_ENV]);

        // UNSET — the shipped default posture.
        std::env::remove_var(FLAG_ENV);
        assert_eq!(
            StopHookRegistration::from_env(),
            StopHookRegistration::Omitted,
            "unset ⇒ dark"
        );

        // Empty, explicitly off, and garbage all fail SAFE to dark.
        for raw in ["", "   ", "off", "OFF", "banana", "1", "true"] {
            std::env::set_var(FLAG_ENV, raw);
            assert_eq!(
                StopHookRegistration::from_env(),
                StopHookRegistration::Omitted,
                "{raw:?} ⇒ dark"
            );
        }

        // ...and only the two armed values register the hook.
        for raw in ["observe", " ON ", "on", "OBSERVE"] {
            std::env::set_var(FLAG_ENV, raw);
            assert_eq!(
                StopHookRegistration::from_env(),
                StopHookRegistration::Registered,
                "{raw:?} ⇒ armed"
            );
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
        for reg in [
            StopHookRegistration::Registered,
            StopHookRegistration::Omitted,
        ] {
            let settings_path = tmp.path().join(reg.settings_name());
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
        let dark_settings = tmp.path().join(HOOK_SETTINGS_NAME_NOSTOP);
        let ok = materialize_with(tmp.path(), StopHookRegistration::Omitted);
        assert_eq!(ok.as_deref(), Some(dark_settings.as_path()));
        assert!(dark_settings.exists());
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
