//! Embedded terminal system — PTY-backed terminal sessions.
//!
//! Provides full terminal emulation inside the runner via `portable-pty`.
//! Each session spawns a native shell (PowerShell on Windows, $SHELL on Unix)
//! with proper environment for running Claude CLI and other dev tools.

pub mod account_migration;
pub mod auto_response;
pub mod auto_response_fleet;
pub mod claude_resume_sniff;
pub mod commit_report;
pub mod context_watcher;
pub mod coord_warn;
pub mod grid;
pub mod interceptor;
pub mod manager;
pub mod output_scan;
mod scan_gate;
pub mod scan_interval;
pub mod session;
pub mod transcript;
pub mod transcript_watcher;
pub mod types;
pub mod usage_limit;
pub mod visibility;
pub mod vt_sanitize;

pub use manager::TerminalManager;

/// Environment variables that carry a credential VALUE and must never be
/// inherited by a runner-spawned session.
///
/// **Why the runner is the chokepoint.** All three reach an agent session ONLY
/// by being inherited through the runner process:
/// `QONTINUI_OPERATOR2_PASSWORD` and `QONTINUI_TEST_LOGIN_PASSWORD` are Windows
/// USER-scope variables the runner picks up at launch;
/// `QONTINUI_TEST_AUTO_LOGIN_PASSWORD` is stamped onto the runner process by the
/// supervisor (`qontinui-supervisor/src/process/env_forwarders.rs:209-210`).
/// There is no other route in, so every fix belongs at a runner spawn seam.
///
/// **Which seams are covered — and which are NOT.** This currently scrubs the
/// two seams named by plan
/// `2026-08-07-runner-context-visibility-and-session-env-secret-hygiene`
/// Phase 1: [`session::TerminalSession::spawn`] (PTY panes) and
/// `agent_runtime::spawn_claude_child` (headless `claude -p` workers).
/// **That is not yet every Claude-CLI spawn seam in this crate.** The crate's
/// own marker for such a seam is the paired
/// `env_remove("CLAUDECODE")` + `env_remove(CLAUDE_CHILD_SESSION_ENV)` strip
/// documented at `session/transport/claude_cli.rs:23-50`; `grep -rn env_remove
/// src-tauri/src` enumerates them. Still UNSCRUBBED at the time of writing:
/// `claude_session/session.rs` (bidirectional stream-json, bypassPermissions,
/// writes an on-disk transcript), `claude_session/runner.rs`,
/// `ai_provider/process.rs::spawn_and_wait_with_doctor` (the shared choke point
/// for the `claude --print` / `gemini -p` one-shots),
/// `orchestration_loop/fix_agent.rs` and `commands/command_interpreter.rs`.
/// The first four take a `std::process::Command` and would need a third wrapper
/// beside the two below; the last two are `tokio::process::Command` and can call
/// [`scrub_credential_env_tokio`] verbatim. Extending the coverage is tracked as
/// follow-up work — do NOT read this const as "the leak is closed everywhere".
///
/// **Why plaintext in a session env is not benign.** The habitual redaction
/// idiom `env | sed 's/\(JWT\|KEY\|TOKEN\|SECRET\)=.*/\1=<redacted>/'` does NOT
/// match `PASSWORD`, so an `env` dump prints these verbatim into a transcript
/// that then travels to a model provider and into coord's session mirror. A
/// filter nobody remembers to widen is not a control; removing the value is.
///
/// **Identifiers are deliberately NOT listed.** The matching `*_EMAIL` /
/// `*_USERNAME` variables name an account, they do not authenticate one, and
/// skills legitimately read them (e.g. `commands/setup_wizard.rs:52` reads
/// `QONTINUI_TEST_AUTO_LOGIN_EMAIL`). Adding them here would break working
/// consumers for no security gain.
///
/// **Consumers that still work.** `commands/setup_wizard.rs` reads the RUNNER's
/// own process env, which is untouched — only the CHILD's env is scrubbed. And
/// `mcp/headless_browser.rs` re-supplies `QONTINUI_TEST_AUTO_LOGIN_EMAIL` /
/// `_PASSWORD` to its own launcher child from `AppCredentials`, which are
/// fetched from AWS SSM (`spec_api::auth_injection`), not from any env.
///
/// Do not put a password-bearing variable into a session env; put its name here.
pub(crate) const CREDENTIAL_VALUE_ENV_VARS: &[&str] = &[
    "QONTINUI_OPERATOR2_PASSWORD",
    "QONTINUI_TEST_LOGIN_PASSWORD",
    "QONTINUI_TEST_AUTO_LOGIN_PASSWORD",
];

/// Strip [`CREDENTIAL_VALUE_ENV_VARS`] from a PTY child's environment.
///
/// `CommandBuilder` seeds its env map at construction (`get_base_env`) from
/// `std::env::vars_os()` **plus, on Windows, a fresh read of the HKLM and HKCU
/// `Environment` registry keys, which are inserted OVER the process-env
/// entries**. So `env_remove` here genuinely drops the inherited value rather
/// than merely declining to add one — and it also drops a USER-scope value the
/// runner process itself never held, which is exactly the shape of
/// `QONTINUI_OPERATOR2_PASSWORD` and `QONTINUI_TEST_LOGIN_PASSWORD`. That
/// registry re-read is unique to this seam; the tokio seam inherits the process
/// env only.
pub(crate) fn scrub_credential_env_pty(cmd: &mut portable_pty::CommandBuilder) {
    for name in CREDENTIAL_VALUE_ENV_VARS {
        cmd.env_remove(name);
    }
}

/// Strip [`CREDENTIAL_VALUE_ENV_VARS`] from a tokio child's environment.
///
/// Twin of [`scrub_credential_env_pty`] over the other `Command` type the two
/// spawn seams use. Both read the SAME name list so the seams cannot drift.
pub(crate) fn scrub_credential_env_tokio(cmd: &mut tokio::process::Command) {
    for name in CREDENTIAL_VALUE_ENV_VARS {
        cmd.env_remove(name);
    }
}

/// The attributable source marker prefixed to [`runner_context`]'s briefing.
///
/// Shape: `[source: <package>/runner_context@<version>+<git-sha>]`. The git SHA
/// comes from `QONTINUI_GIT_SHA` (baked by `build.rs`, the same stamp `/health`
/// and the fleet drift check report) because `CARGO_PKG_VERSION` alone moves
/// only once per release — every locally-built runner on the fleet would
/// otherwise claim the same version. On a source-tarball build with no git the
/// SHA component is the literal `unknown`.
///
/// Exposed so consumers can recognize the marker without retyping the literal.
pub const RUNNER_CONTEXT_SOURCE_MARKER: &str = concat!(
    "[source: ",
    env!("CARGO_PKG_NAME"),
    "/runner_context@",
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("QONTINUI_GIT_SHA"),
    "]"
);

/// The canonical "you are inside the Qontinui Runner" briefing appended to the
/// system prompt of every `claude` session the runner hosts.
///
/// This is the SINGLE SOURCE OF TRUTH for the briefing text. Both launch paths
/// render it from here so they can never drift:
///   - Interactive panes (a human types `claude`) read it via the
///     `QONTINUI_RUNNER_CONTEXT` env var that [`session`] injects at spawn; the
///     `shell-integration.{ps1,bash,zsh}` wrapper passes it to
///     `--append-system-prompt`.
///   - Autonomous direct-exec spawns (gate continuations, fleet/batch, the
///     looping-agent supervisor) never source shell integration, so they
///     inject it into the argv directly via
///     `agent_runtime::build_continuation_claude_command`.
///
/// Pull-first lean protocol (session-autonomy-fabric Phase 5): this briefing
/// carries PROTOCOL + LINKS, never policy content. Policy/playbook bodies live
/// in coord's versioned prompt documents and are fetched on demand, so editing
/// a policy never requires a runner release. It also carries NO narration
/// instructions — transparency is structural (tool calls land in the
/// transcript).
///
/// It deliberately carries NO tenant/agent identity — that is an RCE-class
/// invariant (a prompt must never cross tenants) and is deferred to the vetted
/// session-identity fabric plan. Only tenant-agnostic protocol guidance lives
/// here.
///
/// Attributable source marker (part of the contract): the FIRST line of the
/// appended briefing is always [`RUNNER_CONTEXT_SOURCE_MARKER`]. An instruction
/// with no attributable source cannot mandate or forbid agent behavior (e.g.
/// spawns — incident coord #1242), so every consumer of this briefing can trace
/// it back to this function and the build that emitted it. Both delivery paths
/// above inherit the marker automatically because they render from this single
/// function. Note this is the first line of the text the runner *appends* — the
/// harness's own system prompt still precedes it.
///
/// # Conditional clauses
///
/// One trailing clause is fleet-gated: the plan/prompt **capture** protocol is
/// appended only when [`crate::mcp::fleet_policy_poller::effective_plan_capture_level`]
/// reads `record` (plan `2026-08-10-plan-and-prompt-library-in-web` Phase 4).
/// At `off` — which is the resting value, the value after any coord 404/401,
/// and the value on an unpaired or offline runner — the clause is ABSENT, on
/// the principle that an instruction with no live authorization must not appear
/// in a system prompt. The read is a synchronous lock read, never I/O: this
/// function runs on the spawn path.
///
/// The clause obeys the same protocol-and-links-only contract as the rest of
/// this briefing — endpoints and a pointer to the coord prompt document, with
/// the long form (what counts as which kind, how to phrase a title, the worked
/// selection example) living in that document, not here.
///
/// ## Ordering constraint the toggle does NOT enforce
///
/// The clause names `:{api_port}/plan-library/*`, which are served by the same
/// plan's Phase 3 (`mcp::plan_library`). The fleet dial is a coord-side data
/// row, so an operator CAN set `plan_capture=record` against a runner build
/// that predates those routes, and every session on it would then be told to
/// POST to a 404. Nothing in this function can detect that — the level says the
/// tenant wants capture, not that this binary can serve it. Two things bound
/// the blast radius rather than remove it: the level is `off` everywhere until
/// an operator deliberately flips it, and Phase 3's write routes are themselves
/// behind an off-by-default capability flag, so turning the instruction on and
/// opening the door are two separate operator acts. **Flip the dial only for a
/// fleet whose runners carry the routes.**
pub fn runner_context(api_port: u16) -> String {
    // Coord HTTP base for the tool-less fallback links. Resolved the same way
    // every coord proxy route resolves it (COORD_HTTP_URL env → active
    // profile → localhost fallback) so the prompt never disagrees with the
    // runner's own coord client.
    let coord_url = crate::coord_mcp::coord_base_url();
    let mut briefing = format!(
        "{RUNNER_CONTEXT_SOURCE_MARKER}
You are running inside the Qontinui Runner — an autonomous multi-agent \
development environment. Runner HTTP API: http://127.0.0.1:{api_port}.

You work autonomously. No human is watching this session; do not wait for \
human replies or pause for approval you can resolve yourself.

Policies and playbooks are pull-first documents, not baked into this prompt. \
Discover them via the coord MCP tools: coord_list_prompt_documents (names + \
descriptions, cheap) and coord_get_prompt_document (full body on demand). \
HTTP fallback if those tools are unavailable: \
GET {coord_url}/coord/prompt-documents (list, optional ?kind= filter) and \
GET {coord_url}/coord/prompt-documents/{{kind}}/{{name}}.

When you would ask the user a question: fetch the relevant policy and DECIDE, \
recording it via coord_request_policy / coord_record_decision. Only a \
question a human must answer goes to coord_ask_question — after asking, you \
will be left alone until it is answered.

When no policy clause covers a decision and you apply a category-default tier \
to proceed, report the gap so a clause can be authored: \
coord_ask_question(policy_gap={{category, proposed_clause, tier_applied}}). \
With tier_applied set it records non-blocking (pre-answered) — you do not wait.

Report status transitions via coord_report_status \
(working | blocked | waiting_human | finished). Set finished only after \
cleanup (worktrees, branches) is done.

Before starting new work, check the communal work ledger so you do not \
duplicate a peer: coord_who_is_working_on, then coord_declare_intent to \
record your own scope.

If context runs low, act BEFORE exhaustion: request a handoff \
(coord_request_handoff) or spawn a continuation session seeded with a \
summary via POST http://127.0.0.1:{api_port}/sessions/spawn.",
    );

    // Fleet-gated clause. Read synchronously from the poller's process-global
    // cache — `off` before the first successful poll, on a coord 404/401, on an
    // unpaired runner and on a poisoned lock, so the default is "no clause".
    if crate::mcp::fleet_policy_poller::effective_plan_capture_level()
        == crate::mcp::fleet_policy_poller::PLAN_CAPTURE_RECORD
    {
        briefing.push_str(&plan_capture_clause(api_port, &coord_url));
    }

    briefing
}

/// The plan/prompt capture clause — PROTOCOL + LINKS ONLY.
///
/// What it may contain: what to save, when to save it, the exact endpoints, and
/// the instruction to record the provenance edge. What it may NOT contain:
/// policy prose, rationale, kind-selection heuristics, or any tenant/agent
/// identity. The long form is a coord prompt document this clause links to, so
/// editing it never requires a runner release (the pull-first lean protocol this
/// whole briefing follows).
///
/// Returned with a LEADING blank line so it appends onto the base briefing as a
/// new paragraph and can never disturb the source marker on line 1.
fn plan_capture_clause(api_port: u16, coord_url: &str) -> String {
    format!(
        "

Plan-library capture is ON for this fleet. Save the work artifacts you author \
to the plan library: investigation prompts, plan-authoring prompts, \
implementation prompts, findings reports, and plans. Write each one when you \
author it, and write it again when its status changes. \
POST http://127.0.0.1:{api_port}/plan-library/artifacts records the artifact; \
POST http://127.0.0.1:{api_port}/plan-library/links records the provenance edge \
back to the artifact it came from (the prompt that produced a report, the \
report that fed a prompt, the prompt that authored a plan) — record the edge \
every time, not only the artifact. Protocol detail lives in the coord prompt \
document: coord_get_prompt_document(kind=\"agent_playbook\", \
name=\"plan-capture\"), or GET \
{coord_url}/coord/prompt-documents/agent_playbook/plan-capture."
    )
}

/// Strip ANSI escape sequences from text for readable scrollback previews.
///
/// Handles CSI (`ESC [ ... letter`) and OSC (`ESC ] ... BEL`) sequences plus
/// generic two-char escapes. Preserves printable chars and `\n` / `\r` / `\t`.
/// Used by both the Tauri command surface (`commands::terminal`) and the MCP
/// HTTP API (`mcp::terminals`) to produce human-readable terminal output.
pub fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip CSI sequences: ESC [ ... letter
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            // Skip OSC sequences: ESC ] ... BEL
            } else if chars.peek() == Some(&']') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c == '\x07' {
                        break;
                    }
                }
            } else {
                // Skip next char (two-char escape)
                chars.next();
            }
        } else if ch >= ' ' || ch == '\n' || ch == '\r' || ch == '\t' {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        runner_context, scrub_credential_env_pty, scrub_credential_env_tokio,
        CREDENTIAL_VALUE_ENV_VARS, RUNNER_CONTEXT_SOURCE_MARKER,
    };
    use crate::mcp::fleet_policy_poller::{pin_plan_capture_level_for_test, PLAN_CAPTURE_RECORD};

    // =======================================================================
    // Session credential-env scrub (plan
    // 2026-08-07-runner-context-visibility-and-session-env-secret-hygiene)
    // =======================================================================

    /// The list is the contract. Assert membership by name so removing an entry
    /// is a deliberate, reviewed act rather than a silent regression that only
    /// shows up as a password in someone's transcript.
    #[test]
    fn credential_env_list_covers_the_three_known_plaintext_passwords() {
        for expected in [
            "QONTINUI_OPERATOR2_PASSWORD",
            "QONTINUI_TEST_LOGIN_PASSWORD",
            "QONTINUI_TEST_AUTO_LOGIN_PASSWORD",
        ] {
            assert!(
                CREDENTIAL_VALUE_ENV_VARS.contains(&expected),
                "{expected} must be scrubbed from every session env"
            );
        }
    }

    /// Identifiers must NOT be scrubbed: `*_EMAIL` / `*_USERNAME` name an
    /// account rather than authenticating one, and in-crate + skill consumers
    /// read them (`commands/setup_wizard.rs:52`). Guards against someone
    /// "hardening" the list into a breakage.
    #[test]
    fn credential_env_list_excludes_identifier_variables() {
        for name in CREDENTIAL_VALUE_ENV_VARS {
            assert!(
                !name.ends_with("_EMAIL") && !name.ends_with("_USERNAME"),
                "{name} is an identifier, not a credential value — do not scrub it"
            );
        }
    }

    /// Every listed name must actually look like a credential VALUE. Cheap
    /// tripwire against the list drifting into a general-purpose env denylist.
    #[test]
    fn credential_env_list_entries_are_credential_values() {
        for name in CREDENTIAL_VALUE_ENV_VARS {
            assert!(
                name.contains("PASSWORD")
                    || name.contains("SECRET")
                    || name.contains("TOKEN")
                    || name.contains("_KEY"),
                "{name} does not read as a credential value"
            );
        }
    }

    /// Behavioural: on the PTY seam the scrub must remove an INHERITED value,
    /// not merely decline to set one. `CommandBuilder` seeds its env map from
    /// the process env at construction, so seeding then scrubbing exercises
    /// exactly the production case.
    #[test]
    fn scrub_removes_inherited_values_from_a_pty_command() {
        // Premise guard. Seeding below uses `cmd.env(...)` rather than mutating
        // the process env (racy, and `unsafe` on newer editions), which is only
        // equivalent to the production case because `CommandBuilder` keeps base
        // env and overrides in ONE map. If a future portable-pty stopped seeding
        // in `new()`, the child would inherit the parent env directly at spawn,
        // `env_remove` would become a no-op, and the rest of this test would
        // still pass — a silent security regression. Assert the premise.
        assert!(
            portable_pty::CommandBuilder::new("dummy")
                .get_env("PATH")
                .is_some()
                || portable_pty::CommandBuilder::new("dummy")
                    .get_env("Path")
                    .is_some(),
            "portable-pty no longer seeds the base env in new() — \
             env_remove is now a no-op at the PTY seam"
        );

        let mut cmd = portable_pty::CommandBuilder::new("dummy");
        for name in CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }
        cmd.env("QONTINUI_TEST_AUTO_LOGIN_EMAIL", "operator@example.com");

        scrub_credential_env_pty(&mut cmd);

        for name in CREDENTIAL_VALUE_ENV_VARS {
            assert!(cmd.get_env(name).is_none(), "{name} survived the PTY scrub");
        }
        assert_eq!(
            cmd.get_env("QONTINUI_TEST_AUTO_LOGIN_EMAIL")
                .and_then(|v| v.to_str()),
            Some("operator@example.com"),
            "the identifier must survive — skills read it"
        );
    }

    /// Behavioural twin on the tokio seam. `tokio::process::Command` records a
    /// removal as `(key, None)` in its env overrides, which is what suppresses
    /// the inherited value at spawn.
    #[test]
    fn scrub_records_removals_on_a_tokio_command() {
        let mut cmd = tokio::process::Command::new("dummy");
        cmd.env("QONTINUI_TEST_AUTO_LOGIN_EMAIL", "operator@example.com");

        scrub_credential_env_tokio(&mut cmd);

        let envs: Vec<(String, Option<String>)> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();

        for name in CREDENTIAL_VALUE_ENV_VARS {
            assert!(
                envs.iter().any(|(k, v)| k == *name && v.is_none()),
                "{name} is not marked for removal on the tokio seam"
            );
        }
        assert!(
            envs.iter()
                .any(|(k, v)| k == "QONTINUI_TEST_AUTO_LOGIN_EMAIL"
                    && v.as_deref() == Some("operator@example.com")),
            "the identifier must survive — skills read it"
        );
    }

    /// A distinctive fragment of the capture clause. Deliberately an ENDPOINT,
    /// not a prose phrase: prose gets reworded, the route is the contract.
    const CLAUSE_MARKER: &str = "/plan-library/artifacts";

    /// The attributability contract: the briefing's FIRST line is always the
    /// source marker naming this package, its version and the build's git SHA,
    /// so any consumer of the injected system prompt can trace the instruction
    /// back to its origin (incident coord #1242).
    #[test]
    fn runner_context_starts_with_attributable_source_marker() {
        // Pinned even though the assertion holds at either level: this test
        // renders the same process-global-dependent string as the clause tests,
        // and running it in an undefined level state is the kind of latent race
        // that only shows up once someone strengthens the assertion.
        let _pin = pin_plan_capture_level_for_test("off");
        let briefing = runner_context(9876);
        assert_eq!(
            briefing.lines().next(),
            Some(RUNNER_CONTEXT_SOURCE_MARKER),
            "the source marker must be the first line of the briefing"
        );
    }

    /// The marker must actually discriminate builds — a bare package/version
    /// stamp moves only once per release, so every locally-built runner on the
    /// fleet would attribute to the same string. Guards the `+<git-sha>`
    /// component against being dropped back to `CARGO_PKG_VERSION` alone.
    #[test]
    fn source_marker_carries_build_identity() {
        let expected = format!(
            "[source: {}/runner_context@{}+{}]",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            env!("QONTINUI_GIT_SHA"),
        );
        assert_eq!(RUNNER_CONTEXT_SOURCE_MARKER, expected);
    }

    // =======================================================================
    // The fleet-gated plan-capture clause (plan
    // 2026-08-10-plan-and-prompt-library-in-web, Phase 4)
    // =======================================================================

    /// At `off` the clause must be ABSENT. `off` is the resting value, the
    /// value after a coord 404/401, and the value on an unpaired or offline
    /// runner — so this is the arm that runs on every runner until an operator
    /// deliberately turns the fleet dial. An instruction with no live
    /// authorization must not appear in a system prompt.
    #[test]
    fn plan_capture_clause_is_absent_at_level_off() {
        let _pin = pin_plan_capture_level_for_test("off");

        let briefing = runner_context(9876);
        assert!(
            !briefing.contains(CLAUSE_MARKER),
            "the capture clause must not appear at level off"
        );
        assert!(!briefing.contains("/plan-library/links"));
        assert!(!briefing
            .to_lowercase()
            .contains("plan-library capture is on"));
        // The rest of the briefing is unaffected — this is an APPENDED clause,
        // not a replacement.
        assert!(briefing.contains("Qontinui Runner"));
        // …and the attributability contract still holds with the clause off.
        assert_eq!(
            briefing.lines().next(),
            Some(RUNNER_CONTEXT_SOURCE_MARKER),
            "the source marker must remain the first line at level off"
        );
    }

    /// At `record` the clause is present, and carries the four things it is
    /// contracted to carry: what to save, when, the exact endpoints, and the
    /// provenance edge. Everything else stays in the linked coord document.
    #[test]
    fn plan_capture_clause_is_present_at_level_record() {
        let _pin = pin_plan_capture_level_for_test(PLAN_CAPTURE_RECORD);

        let briefing = runner_context(9876);

        // The exact endpoints, on the loopback API port the caller passed.
        assert!(briefing.contains("http://127.0.0.1:9876/plan-library/artifacts"));
        assert!(briefing.contains("http://127.0.0.1:9876/plan-library/links"));
        // WHAT to save — all five kinds.
        for kind in [
            "investigation prompts",
            "plan-authoring prompts",
            "implementation prompts",
            "findings reports",
            "plans",
        ] {
            assert!(briefing.contains(kind), "clause must name `{kind}`");
        }
        // WHEN — on authoring and on status change.
        assert!(briefing.contains("when you author it"));
        assert!(briefing.contains("status changes"));
        // The provenance edge back to the source artifact.
        assert!(briefing.contains("provenance edge"));
        // LINKS, not policy prose: the long form is a coord prompt document.
        assert!(briefing.contains("coord_get_prompt_document"));
        assert!(briefing.contains("/coord/prompt-documents/agent_playbook/plan-capture"));

        // The marker contract survives an appended clause.
        assert_eq!(
            briefing.lines().next(),
            Some(RUNNER_CONTEXT_SOURCE_MARKER),
            "the source marker must remain the first line at level record"
        );
    }

    /// A level that is neither `off` nor `record` is not an authorization.
    /// Guards against the other fleet-policy domain's vocabulary (`observe` /
    /// `gate`) or a typo being read as "on-ish, close enough".
    #[test]
    fn an_unrecognised_level_does_not_inject_the_clause() {
        let pin = pin_plan_capture_level_for_test("off");
        for level in ["observe", "gate", "recording", "RECORD ", "", "on"] {
            pin.set(level);
            let briefing = runner_context(9876);
            assert!(
                !briefing.contains(CLAUSE_MARKER),
                "level `{level}` must not inject the clause"
            );
        }
    }

    /// The clause carries NO tenant or agent identity — an RCE-class invariant
    /// of this briefing (a prompt must never cross tenants). The clause names
    /// routes and a document, and the org comes from the device JWT the runner
    /// attaches to the write, never from the prompt.
    ///
    /// Scope note: this is a NAMED-FIELD scan, not a proof of tenant-agnosticism.
    /// It catches the realistic regression — someone interpolating an id into
    /// the clause under one of these names — but a bare UUID pasted in as a
    /// literal would pass. The structural guarantee is upstream: the clause is
    /// a static format string whose only inputs are `api_port` and the coord
    /// base URL.
    #[test]
    fn the_clause_carries_no_tenant_or_agent_identity() {
        let _pin = pin_plan_capture_level_for_test(PLAN_CAPTURE_RECORD);

        let briefing = runner_context(9876);
        for forbidden in [
            "tenant_id",
            "organization_id",
            "agent_id",
            "device_id",
            "scope_key",
        ] {
            assert!(
                !briefing.contains(forbidden),
                "the briefing must not carry `{forbidden}`"
            );
        }
    }
}
