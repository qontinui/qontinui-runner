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
/// **Which seams are covered.** Every seam in this crate that starts a Claude
/// or Gemini CLI *session* — i.e. one that carries a prompt and can therefore
/// put an `env` dump into a model context or an on-disk transcript. The crate's
/// own marker for such a seam is the paired
/// `env_remove("CLAUDECODE")` + `env_remove(CLAUDE_CHILD_SESSION_ENV)` strip
/// documented at `session/transport/claude_cli.rs:23-50`; `grep -rn env_remove
/// src-tauri/src` enumerates them. The eight covered seams, each scrubbing as
/// its LAST env mutation before spawn:
///
/// | seam | `Command` type | wrapper |
/// |---|---|---|
/// | `session::TerminalSession::finalize_child_env` (PTY panes) | `portable_pty::CommandBuilder` | [`scrub_credential_env_pty`] |
/// | `agent_runtime::finalize_headless_child_env` (headless `claude -p`) | `tokio::process::Command` | [`scrub_credential_env_tokio`] |
/// | `claude_session::session::finalize_child_env` (bidirectional stream-json, bypassPermissions, on-disk transcript) | `std::process::Command` | [`scrub_credential_env_std`] |
/// | `claude_session::runner::build_inline_child_command` | `std::process::Command` | [`scrub_credential_env_std`] |
/// | `ai_provider::process::prepare_ai_child_env` (choke point for the `claude --print` **and** `gemini -p` one-shots) | `std::process::Command` | [`scrub_credential_env_std`] |
/// | `ai_provider::claude_cli::build_scorer_command` (auto-response option scorer) | `std::process::Command` | [`scrub_credential_env_std`] |
/// | `orchestration_loop::fix_agent::build_fix_agent_command` | `tokio::process::Command` | [`scrub_credential_env_tokio`] |
/// | `commands::command_interpreter::build_interpret_command` | `tokio::process::Command` | [`scrub_credential_env_tokio`] |
///
/// Every one of those is an extracted, unit-tested env-construction function
/// (plan `2026-08-07-runner-context-visibility-and-session-env-secret-hygiene`
/// Phase 1 + follow-up), so deleting a `scrub_*` call reddens a test rather
/// than silently reopening the leak.
///
/// **Which spawn sites deliberately do NOT scrub, and why.** Three `claude`
/// spawn sites carry the `CLAUDECODE` marker or a claude/gemini program name
/// but are not session seams, so a scrub there would buy nothing:
///
/// - `instance_manager.rs` launches a SECONDARY qontinui-runner, not a CLI
///   session. That runner is a legitimate holder of these values (see
///   "Consumers that still work" below) and every session IT spawns comes back
///   through the seams above. Scrubbing here would break `setup_wizard` /
///   `headless_browser` on secondaries for no gain.
/// - `commands/ai_settings.rs` (`claude --version` / `gemini --version`
///   connection tests) and `fleet.rs::detect_claude_code_now`
///   (`claude --version` availability probe) pass no prompt, load no model and
///   write no transcript. There is no context for an `env` dump to land in.
///
/// If you add a spawn site that passes a prompt to a CLI, it belongs in the
/// table above, not in this list.
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
/// Twin of [`scrub_credential_env_pty`] over one of the other two `Command`
/// types the spawn seams use. All three wrappers read the SAME name list so the
/// seams cannot drift.
pub(crate) fn scrub_credential_env_tokio(cmd: &mut tokio::process::Command) {
    for name in CREDENTIAL_VALUE_ENV_VARS {
        cmd.env_remove(name);
    }
}

/// Strip [`CREDENTIAL_VALUE_ENV_VARS`] from a `std::process::Command` child's
/// environment.
///
/// Third and last of the wrappers, for the blocking seams
/// (`claude_session::{session, runner}`, `ai_provider::{process, claude_cli}`).
/// Takes `&mut` rather than a builder-style `Command` so it also fits the
/// `&mut std::process::Command` call site in
/// `ai_provider::process::spawn_and_wait_with_doctor`, which never owns the
/// command it prepares.
///
/// `std::process::Command::env_remove` records the name as *cleared* in the
/// child's env override map, which suppresses the value the child would
/// otherwise inherit from this process — it is not merely "decline to set".
/// `tokio::process::Command` is a wrapper over this same type and inherits the
/// behaviour, which is why the two functions are one-liners over one const.
pub(crate) fn scrub_credential_env_std(cmd: &mut std::process::Command) {
    for name in CREDENTIAL_VALUE_ENV_VARS {
        cmd.env_remove(name);
    }
}

/// Test-only assertion: every [`CREDENTIAL_VALUE_ENV_VARS`] name is marked for
/// removal on a `std::process::Command` built by a production seam.
///
/// Shared by the per-seam call-site tests so each seam asserts the SAME
/// property against the SAME name list — a name added to the const is
/// immediately required at every seam rather than at whichever ones someone
/// remembered to update.
#[cfg(test)]
pub(crate) fn assert_credentials_scrubbed_std(cmd: &std::process::Command, seam: &str) {
    let envs: Vec<(String, bool)> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_string_lossy().to_string(), v.is_none()))
        .collect();
    for name in CREDENTIAL_VALUE_ENV_VARS {
        assert!(
            envs.iter().any(|(k, cleared)| k == *name && *cleared),
            "{seam}: {name} is not marked for removal — the credential scrub \
             is missing from this spawn seam's env construction"
        );
    }
}

/// Test-only twin of [`assert_credentials_scrubbed_std`] for the tokio seams.
#[cfg(test)]
pub(crate) fn assert_credentials_scrubbed_tokio(cmd: &tokio::process::Command, seam: &str) {
    assert_credentials_scrubbed_std(cmd.as_std(), seam);
}

/// Test-only twin of [`assert_credentials_scrubbed_std`] for the PTY seam.
///
/// `CommandBuilder` keeps base env and overrides in ONE map, so a scrubbed name
/// is simply ABSENT rather than present-and-cleared. Seed the values before
/// calling the seam under test, or this assertion passes vacuously.
#[cfg(test)]
pub(crate) fn assert_credentials_scrubbed_pty(cmd: &portable_pty::CommandBuilder, seam: &str) {
    for name in CREDENTIAL_VALUE_ENV_VARS {
        assert!(
            cmd.get_env(name).is_none(),
            "{seam}: {name} survived — the credential scrub is missing from \
             this spawn seam's env construction"
        );
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

/// Render the "you are inside the Qontinui Runner" briefing appended to the
/// system prompt of every `claude` session the runner hosts.
///
/// # This function is a RENDERER, not the text
///
/// The briefing's source of truth is a coord prompt document —
/// `session_briefing/runner-session` — which an operator edits at
/// `/admin/coord/prompt-documents` with a version log, a diff and a
/// restore-to-default (plan
/// `2026-08-20-runner-session-briefing-versioned-and-operator-editable`). The
/// compiled-in text in [`builtin_briefing_body`] is a labelled **FALLBACK**,
/// deliberately allowed to age and drift; it is NOT a mirror of the document
/// and must never be read as the authority on what a session was told.
///
/// What this function does, synchronously and with **zero I/O** (it runs on the
/// spawn path):
///
/// 1. line 1 — [`RUNNER_CONTEXT_SOURCE_MARKER`], byte-identical, always;
/// 2. line 2 — the provenance label, e.g.
///    `[briefing: coord session_briefing/runner-session v7]` or
///    `[briefing: builtin-fallback]`;
/// 3. the base body: the cached coord document with the closed placeholder
///    vocabulary substituted, or the builtin;
/// 4. the fleet-gated plan-capture clause, on the same condition as before.
///
/// Every degradation — coord unreachable, unpaired runner, absent document, a
/// body that fails the render-time guard — falls back to the builtin and SAYS
/// so on line 2. The one thing that must never happen is claiming a coord
/// version while serving the builtin.
///
/// Both launch paths render from here so they can never drift:
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
/// transcript). Making the briefing itself an editable document is NOT licence
/// to grow it: the 16 KiB render-time ceiling is a bound on the damage, never a
/// target.
///
/// It carries NO tenant/agent identity — an RCE-class invariant (a prompt must
/// never cross tenants). **Be precise about what kind of guarantee that now
/// is.** While the text was a static format string the invariant was
/// STRUCTURAL: there was no way to interpolate an identity. With an editable
/// body it is ENFORCED — by coord's write-time validator and again by this
/// crate's render-time guard, which refuses a body carrying a named identity
/// key or a UUID-shaped literal. That is a weaker class of guarantee,
/// deliberately accepted, and it is the reason enforcement lives at BOTH ends
/// rather than one. Tenant scoping comes from the device JWT the fetch is made
/// with and from `(tenant_id, kind, name)` — never from the text.
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
/// The marker line is **equality**, not a prefix, and the provenance label is
/// therefore its own SECOND line rather than a same-line suffix: several tests
/// in this crate assert line-1 equality, and `/whereami` parses line 1 for the
/// spawn SHA with no hex shape-guard, so a suffix would silently corrupt an
/// external parser instead of failing loudly. The marker is also runner-owned:
/// an editable body that opens with a `[source: …]` line is refused by the
/// render-time guard, so it can never be forged from a document.
///
/// # Conditional clauses
///
/// TWO trailing clauses are gated, on different things, and neither gate is
/// the document that supplies its text:
///
/// 1. **plan capture** — a FLEET dial (below). The dial is the authorization;
///    the coord document is only the content, so creating a row must never by
///    itself turn an instruction on for a whole tenant.
/// 2. **coord memory** — a PROVISIONING fact, [`crate::coord_mcp::coord_mcp_deliverable`].
///    It fails to FALSE with no Tauri runtime, so a session that cannot be
///    given the memory tools is never told to use them. Unlike everything else
///    here it is compiled-in ONLY, deliberately: see the note at its call site.
///
/// The first, in full: the plan/prompt **capture** protocol is
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
    use crate::mcp::fleet_policy_poller::{BRIEFING_PLAN_CAPTURE_CLAUSE, BRIEFING_RUNNER_SESSION};
    use crate::mcp::session_briefing;

    // Coord HTTP base for the tool-less fallback links. Resolved the same way
    // every coord proxy route resolves it (COORD_HTTP_URL env → active
    // profile → localhost fallback) so the prompt never disagrees with the
    // runner's own coord client.
    let coord_url = crate::coord_mcp::coord_base_url();
    let api_base = session_briefing::runner_api_base(api_port);

    // The base body: the cached coord document, or the compiled-in fallback.
    // Pure cache read + string substitution — no I/O, no await.
    let base = session_briefing::resolve(
        BRIEFING_RUNNER_SESSION,
        &builtin_briefing_body(api_port, &coord_url),
        &api_base,
        &coord_url,
    );

    // Line 2 collects the provenance of every DOCUMENT-BACKED block in this
    // render, so `head -2` answers "which text am I running under". The
    // plan-capture clause is a separate document with its own version, and a
    // render that named only the base would leave half the injected text
    // unattributed.
    //
    // The memory clause below is deliberately absent from line 2: it is
    // compiled-in only, so a token for it would read `builtin` on every render
    // forever — noise on the one line that has to stay worth reading.
    let mut provenance = base.provenance.line();

    let mut briefing = String::new();

    // Fleet-gated clause. Read synchronously from the poller's process-global
    // cache — `off` before the first successful poll, on a coord 404/401, on an
    // unpaired runner and on a poisoned lock, so the default is "no clause".
    if crate::mcp::fleet_policy_poller::effective_plan_capture_level()
        == crate::mcp::fleet_policy_poller::PLAN_CAPTURE_RECORD
    {
        let clause = session_briefing::resolve(
            BRIEFING_PLAN_CAPTURE_CLAUSE,
            &plan_capture_clause_body(api_port, &coord_url),
            &api_base,
            &coord_url,
        );
        provenance.push_str(&format!(" [clause: {}]", clause.provenance.describe()));
        briefing.push_str("\n\n");
        briefing.push_str(&clause.text);
    }

    // Provisioning-gated clause. `coord_mcp_deliverable()` is a pure in-process
    // read (no I/O — this function runs on the spawn path) and fails to FALSE
    // when no Tauri runtime is reachable, so a session that cannot be given the
    // memory tools is never told to use them and keeps the local-file fallback.
    //
    // Deliberately NOT a `session_briefing` document, and therefore carrying no
    // provenance token: its wording is load-bearing EVIDENCE, not an editorial
    // choice. `memory_clause`'s own docs record that it is the winning arm of a
    // measured two-arm A/B, verbatim — "reword it and the evidence no longer
    // applies to what ships". Making it operator-editable would hand that
    // guarantee away for the one clause whose entire justification IS the
    // measurement. Moving it to coord is a separate decision with its own
    // evidence bar, and it is not this plan.
    if crate::coord_mcp::coord_mcp_deliverable() {
        briefing.push_str(&memory_clause());
    }

    format!(
        "{RUNNER_CONTEXT_SOURCE_MARKER}\n{provenance}\n{}{briefing}",
        base.text
    )
}

/// The compiled-in FALLBACK briefing body — everything after the marker and
/// provenance lines.
///
/// Not the source of truth (see [`runner_context`]): it is what the runner
/// injects when coord has never been reached, when the document is absent or
/// unauthorized, or when the served body fails the render-time guard. It is
/// allowed to age relative to `session_briefing/runner-session`; that drift is
/// harmless precisely because line 2 always says which one was used.
///
/// Kept as a `format!` over `api_port` + `coord_url` rather than moved to the
/// placeholder vocabulary: this text never travels over the wire, so it has no
/// reason to carry `{{tokens}}` a human would then have to mentally expand.
pub(crate) fn builtin_briefing_body(api_port: u16, coord_url: &str) -> String {
    // The HTTP fallbacks below name `/coord/agent-prompt-documents`, coord's
    // device/agent (`require_jwt`) door — NOT the sibling
    // `/coord/prompt-documents`, which is the OPERATOR door and 403s a device
    // JWT (documented at length on `mcp::continuation_verdict`'s rules URL and
    // in `prompt_library`'s module docs). The briefing named the operator door
    // until 2026-08-19, which meant the one WRITTEN escape hatch for a session
    // whose coord MCP tools are masked was dead on arrival — it 403s exactly
    // when it is needed. `policy/session-protocol` Step 0 names the agent door
    // for this reason; the briefing now agrees with it.
    // (plan `2026-08-08-runner-enforced-policy-pull.md` Phase 1.8)
    format!(
        "You are running inside the Qontinui Runner — an autonomous multi-agent \
development environment. Runner HTTP API: http://127.0.0.1:{api_port}.

You work autonomously. No human is watching this session; do not wait for \
human replies or pause for approval you can resolve yourself.

Policies and playbooks are pull-first documents, not baked into this prompt. \
Discover them via the coord MCP tools: coord_list_prompt_documents (names + \
descriptions, cheap) and coord_get_prompt_document (full body on demand). \
HTTP fallback if those tools are unavailable: \
GET {coord_url}/coord/agent-prompt-documents (list, optional ?kind= filter) and \
GET {coord_url}/coord/agent-prompt-documents/{{kind}}/{{name}}.

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
summary via POST http://127.0.0.1:{api_port}/sessions/spawn."
    )
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
/// Returned as a BARE paragraph with no leading blank line: [`runner_context`]
/// owns the paragraph separator, so the coord document and this fallback
/// compose identically and neither can disturb the source marker on line 1.
///
/// Like [`builtin_briefing_body`], this is the compiled-in FALLBACK for
/// `session_briefing/plan-capture-clause`, not its source of truth. The fleet
/// dial stays the AUTHORIZATION (see the ordering note on [`runner_context`]);
/// the document is only the content.
pub(crate) fn plan_capture_clause_body(api_port: u16, coord_url: &str) -> String {
    format!(
        "Plan-library capture is ON for this fleet. Save the work artifacts you author \
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
{coord_url}/coord/agent-prompt-documents/agent_playbook/plan-capture."
    )
}

/// The coord-memory clause — PROTOCOL + LINKS ONLY.
///
/// Plan: `2026-08-05-runner-memory-injection-and-tenant-fail-closed` Phase 4.
///
/// ## Why this exists
///
/// Coord-backed session memory is provisioned into every runner-spawned session
/// (the per-session `.mcp.json` / `QONTINUI_MCP_CONFIG` seam, with
/// `coord_memory_record` + `coord_memory_search` allowlisted) but nothing ever
/// told a session to USE it. Claude Code's built-in file-memory directive won
/// uncontested, so sessions wrote local topic files that — since the memory
/// cutover retired the runner's up-federation — now reach coord by nothing.
///
/// ## Why an appended system prompt is enough
///
/// Measured, not assumed (plan Phase 1, 2026-08-18): a two-arm A/B differing
/// ONLY in the presence of this text. With it, the session recorded to coord
/// (`memory_id 889da044-…`) and wrote **zero** local `.md` files; without it,
/// the same prompt in the same shape wrote a topic file plus the `MEMORY.md`
/// index line and never reached coord. The wording below is the arm that won,
/// verbatim — reword it and the evidence no longer applies to what ships.
///
/// ## Contracts inherited from [`runner_context`]
///
/// - **Protocol + links only.** No policy prose; bodies live in coord.
/// - **No tenant/agent identity.** Tenant rides the CREDENTIAL (the proxy injects
///   the session's device JWT and coord binds `tenant_id` from the token claim,
///   never from a request argument), so naming a tenant here would be both
///   inert and an invitation for the model to try to set one. That omission is
///   deliberate — do not "helpfully" add it.
/// - **Leading blank line** so it appends as a new paragraph and can never
///   disturb [`RUNNER_CONTEXT_SOURCE_MARKER`] on line 1.
fn memory_clause() -> String {
    "

Session memory is coord-backed in this session, and the tools are live. Author \
memories with the coord_memory_record MCP tool INSTEAD of writing local memory \
files, and recall with coord_memory_search INSTEAD of reading a local memory \
directory. Search is full-text: query it with the target record's own literal \
words rather than a paraphrase, and note the argument is query_text."
        .to_string()
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
        memory_clause, runner_context, scrub_credential_env_pty, scrub_credential_env_std,
        scrub_credential_env_tokio, CREDENTIAL_VALUE_ENV_VARS, RUNNER_CONTEXT_SOURCE_MARKER,
    };
    use crate::mcp::fleet_policy_poller::{
        briefing_for_test, pin_plan_capture_level_for_test, BriefingProvenance,
        BRIEFING_PLAN_CAPTURE_CLAUSE, BRIEFING_RUNNER_SESSION, PLAN_CAPTURE_RECORD,
    };

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

    /// Behavioural twin on the blocking `std::process::Command` seam. A prior
    /// `env(name, …)` must be REPLACED by the cleared marker, not left standing:
    /// that is what proves the wrapper suppresses an inherited value rather than
    /// merely declining to add one.
    #[test]
    fn scrub_records_removals_on_a_std_command() {
        let mut cmd = std::process::Command::new("dummy");
        for name in CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }
        cmd.env("QONTINUI_TEST_AUTO_LOGIN_EMAIL", "operator@example.com");

        scrub_credential_env_std(&mut cmd);

        let envs: Vec<(String, Option<String>)> = cmd
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
                "{name} is not marked for removal on the std seam"
            );
            assert!(
                !envs
                    .iter()
                    .any(|(k, v)| k == *name && v.as_deref() == Some("hunter2")),
                "{name} still carries its value on the std seam"
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

    /// The briefing's advertised HTTP fallback must name coord's DEVICE/AGENT
    /// door. `/coord/prompt-documents/*` is the operator door: it resolves
    /// tenancy from a verified Cognito operator context and 403s the device JWT
    /// a runner session carries. Naming it made the one written escape hatch
    /// for a masked-tools session dead on arrival — it fails exactly when it is
    /// needed, and fails with an auth error that looks like a permissions
    /// problem rather than a wrong-URL one.
    /// (plan `2026-08-08-runner-enforced-policy-pull.md` Phase 1.8)
    #[test]
    fn briefing_http_fallback_names_the_agent_door_not_the_operator_door() {
        let _pin = pin_plan_capture_level_for_test("off");
        let briefing = runner_context(9876);
        assert!(
            briefing.contains("/coord/agent-prompt-documents (list, optional ?kind= filter)"),
            "the list fallback must be the agent door: {briefing}"
        );
        assert!(
            briefing.contains("/coord/agent-prompt-documents/{kind}/{name}"),
            "the single-document fallback must be the agent door: {briefing}"
        );
        // The negative is the load-bearing half: `agent-prompt-documents`
        // CONTAINS `prompt-documents`, so match on the full path segment.
        assert!(
            !briefing.contains("/coord/prompt-documents"),
            "the operator door 403s a device JWT and must never be advertised: {briefing}"
        );
    }

    /// The same invariant, for the arm the three assertions above can no longer
    /// reach. Once the body is a coord document those assertions only prove the
    /// BUILTIN is clean — an operator edit could re-open the bug
    /// `2026-08-08` Phase 1.8 fixed and every one of them would still pass. So
    /// the guarantee moved into the render-time guard: an edited body naming
    /// the operator door is REFUSED and the builtin renders instead.
    ///
    /// Coord rejects the same body at write time (Phase 2 step 6). Both ends,
    /// deliberately: coord has more than one write door, and a runner that
    /// renders whatever it is handed has no structural guarantee left at the
    /// point where the prompt is actually built.
    #[test]
    fn an_edited_body_naming_the_operator_door_is_refused_at_render() {
        let pin = pin_plan_capture_level_for_test("off");
        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            briefing_for_test(
                "Fetch policy over GET https://coord.example.com/coord/prompt-documents",
                12,
                BriefingProvenance::Coord,
            ),
        );

        let briefing = runner_context(9876);
        assert!(
            !briefing.contains("/coord/prompt-documents"),
            "an edited body must not be able to advertise the operator door: {briefing}"
        );
        assert!(
            briefing.contains("[briefing: builtin-fallback (rejected coord v12)]"),
            "the refusal must be visible on line 2: {briefing}"
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
    ///
    /// Scope note since the clause became editable: these eleven literal
    /// phrases now pin the BUILTIN fallback, which is what this test renders
    /// (nothing is cached under the pin). They are no longer a statement about
    /// whatever a tenant has edited `session_briefing/plan-capture-clause` to
    /// say — that body is bounded by the render-time guard, not by this list.
    /// What did NOT change is the authorization: the fleet dial still decides
    /// whether ANY clause appears, so `plan_capture_clause_is_absent_at_level_off`
    /// below remains a statement about every render.
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
        // The AGENT door — the operator door 403s a device JWT, so a link to it
        // is a link a session cannot follow.
        assert!(briefing.contains("/coord/agent-prompt-documents/agent_playbook/plan-capture"));

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
    /// Scope note, REWRITTEN now that the clause is an editable document. This
    /// test used to lean on an upstream structural guarantee — "the clause is a
    /// static format string whose only inputs are `api_port` and the coord base
    /// URL" — and moving the body into coord DESTROYS that guarantee. It is a
    /// weaker class of assurance, deliberately accepted, and it is the single
    /// strongest argument for enforcing at both ends.
    ///
    /// So this test now pins the BUILTIN (nothing is cached under the pin), and
    /// `an_edited_body_carrying_identity_is_refused_at_render` below covers the
    /// arm it can no longer reach. Coord rejects the same shapes at write time.
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

    // ---- Phase 4: the coord-memory clause ----

    #[test]
    fn memory_clause_starts_with_a_blank_line_so_it_never_touches_line_one() {
        // The source-marker contract: the clause appends as a NEW paragraph.
        // If this regresses, the marker stops being the first line of the
        // briefing and the attribution contract (incident coord #1242) breaks.
        assert!(
            memory_clause().starts_with(
                "

"
            ),
            "clause must open with a blank line, got: {:?}",
            &memory_clause()[..memory_clause().len().min(20)]
        );
    }

    #[test]
    fn memory_clause_names_both_tools_and_the_search_argument() {
        let c = memory_clause();
        assert!(
            c.contains("coord_memory_record"),
            "authoring tool must be named"
        );
        assert!(
            c.contains("coord_memory_search"),
            "recall tool must be named"
        );
        // Phase 1 found `query` silently errors; the clause must say `query_text`.
        assert!(
            c.contains("query_text"),
            "the search argument must be named"
        );
    }

    #[test]
    fn memory_clause_has_no_collapsed_whitespace_runs() {
        // Regression guard for a real defect caught in review: a multi-line Rust
        // string literal whose `\` line-continuations go missing silently keeps
        // compiling, but ships the source indentation INSIDE the string — the
        // model then receives "resolve its          tenant". Substring assertions
        // do not catch it, because the words are all still there.
        let c = memory_clause();
        let body = c.trim_start_matches('\n');
        assert!(
            !body.contains("  "),
            "clause carries a collapsed whitespace run (lost line continuation): {body:?}"
        );
        assert!(
            !body.contains('\n'),
            "clause body must be one paragraph — a raw newline means a continuation was dropped"
        );
    }

    #[test]
    fn memory_clause_carries_no_tenant_identity() {
        // RCE-class invariant inherited from `runner_context`: tenant rides the
        // credential, never the prompt.
        let c = memory_clause().to_lowercase();
        assert!(
            !c.contains("tenant"),
            "clause must not mention tenancy: {c}"
        );
        assert!(
            !c.contains("tenant_id"),
            "clause must not carry a tenant id"
        );
    }

    #[test]
    fn memory_clause_is_absent_when_coord_mcp_is_not_deliverable() {
        // No Tauri runtime in unit tests, so `coord_mcp_deliverable()` is false
        // and the briefing must NOT carry the directive — a session that cannot
        // be given the tools is never told to use them (fail to ABSENT).
        assert!(
            !crate::coord_mcp::coord_mcp_deliverable(),
            "precondition: no runtime under test, so coord-mcp is not deliverable"
        );
        // Pinned because `runner_context` now also reads the session-briefing
        // cache; the guard serializes BOTH spawn-path globals and restores them
        // on drop.
        let _pin = pin_plan_capture_level_for_test("off");
        let briefing = runner_context(9876);
        assert!(
            !briefing.contains("coord_memory_record"),
            "ungated briefing leaked the memory directive"
        );
    }

    #[test]
    fn memory_clause_appends_cleanly_onto_a_briefing() {
        // Composition check: the marker survives, and the clause lands whole.
        let _pin = pin_plan_capture_level_for_test("off");
        let composed = format!("{}{}", runner_context(9876), memory_clause());
        assert!(
            composed.starts_with(RUNNER_CONTEXT_SOURCE_MARKER),
            "source marker must still open the composed briefing"
        );
        assert!(composed.contains("coord_memory_search"));
    }

    /// The RCE-class invariant, rebuilt as an explicit render-time scan for the
    /// editable arm: a body carrying a named identity key OR a bare UUID-shaped
    /// literal is refused, and the builtin renders. The UUID half is what the
    /// old named-field scan admitted it could not see.
    #[test]
    fn an_edited_body_carrying_identity_is_refused_at_render() {
        let pin = pin_plan_capture_level_for_test(PLAN_CAPTURE_RECORD);
        for (label, bad) in [
            ("named key", "your agent_id is attached to every write"),
            (
                "bare uuid",
                "you are agent 01a01eb4-718a-7303-825a-94ec0d0ade91",
            ),
        ] {
            pin.set_briefing(
                BRIEFING_PLAN_CAPTURE_CLAUSE,
                briefing_for_test(bad, 5, BriefingProvenance::Coord),
            );

            let briefing = runner_context(9876);
            assert!(
                !briefing.contains("agent_id")
                    && !briefing.contains("01a01eb4-718a-7303-825a-94ec0d0ade91"),
                "{label}: identity reached a system prompt: {briefing}"
            );
            assert!(
                briefing.contains("[clause: builtin-fallback (rejected coord v5)]"),
                "{label}: the refusal must be visible on line 2: {briefing}"
            );
            // …and the builtin clause is still injected, so a refused edit
            // degrades to the shipped instruction rather than to silence.
            assert!(briefing.contains(CLAUSE_MARKER), "{label}: {briefing}");
        }
    }

    // =======================================================================
    // The briefing is now a RENDER of a coord document (plan
    // 2026-08-20-runner-session-briefing-versioned-and-operator-editable).
    // =======================================================================

    /// THE NO-REGRESSION ANCHOR. A verbatim copy of the briefing body this
    /// build shipped before the briefing became a coord document, with the two
    /// interpolations spelled out (`9876`, and `__COORD__` for the coord base
    /// the test resolves the same way the renderer does).
    ///
    /// Deliberately a LITERAL rather than a call to `builtin_briefing_body`: a
    /// test that renders through the same function it is checking cannot see a
    /// change to that function. This is the one assertion that proves moving
    /// the text into a document did not silently reword it.
    const TODAYS_BRIEFING_BODY: &str = r#"You are running inside the Qontinui Runner — an autonomous multi-agent development environment. Runner HTTP API: http://127.0.0.1:9876.

You work autonomously. No human is watching this session; do not wait for human replies or pause for approval you can resolve yourself.

Policies and playbooks are pull-first documents, not baked into this prompt. Discover them via the coord MCP tools: coord_list_prompt_documents (names + descriptions, cheap) and coord_get_prompt_document (full body on demand). HTTP fallback if those tools are unavailable: GET __COORD__/coord/agent-prompt-documents (list, optional ?kind= filter) and GET __COORD__/coord/agent-prompt-documents/{kind}/{name}.

When you would ask the user a question: fetch the relevant policy and DECIDE, recording it via coord_request_policy / coord_record_decision. Only a question a human must answer goes to coord_ask_question — after asking, you will be left alone until it is answered.

When no policy clause covers a decision and you apply a category-default tier to proceed, report the gap so a clause can be authored: coord_ask_question(policy_gap={category, proposed_clause, tier_applied}). With tier_applied set it records non-blocking (pre-answered) — you do not wait.

Report status transitions via coord_report_status (working | blocked | waiting_human | finished). Set finished only after cleanup (worktrees, branches) is done.

Before starting new work, check the communal work ledger so you do not duplicate a peer: coord_who_is_working_on, then coord_declare_intent to record your own scope.

If context runs low, act BEFORE exhaustion: request a handoff (coord_request_handoff) or spawn a continuation session seeded with a summary via POST http://127.0.0.1:9876/sessions/spawn."#;

    /// The same anchor for the fleet-gated clause.
    const TODAYS_CLAUSE_BODY: &str = r#"Plan-library capture is ON for this fleet. Save the work artifacts you author to the plan library: investigation prompts, plan-authoring prompts, implementation prompts, findings reports, and plans. Write each one when you author it, and write it again when its status changes. POST http://127.0.0.1:9876/plan-library/artifacts records the artifact; POST http://127.0.0.1:9876/plan-library/links records the provenance edge back to the artifact it came from (the prompt that produced a report, the report that fed a prompt, the prompt that authored a plan) — record the edge every time, not only the artifact. Protocol detail lives in the coord prompt document: coord_get_prompt_document(kind="agent_playbook", name="plan-capture"), or GET __COORD__/coord/agent-prompt-documents/agent_playbook/plan-capture."#;

    fn expected_briefing_body() -> String {
        TODAYS_BRIEFING_BODY.replace("__COORD__", &crate::coord_mcp::coord_base_url())
    }

    fn expected_clause_body() -> String {
        TODAYS_CLAUSE_BODY.replace("__COORD__", &crate::coord_mcp::coord_base_url())
    }

    /// Split a render into (line 1, line 2, everything else).
    fn split_render(briefing: &str) -> (&str, &str, &str) {
        let mut parts = briefing.splitn(3, '\n');
        (
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        )
    }

    /// With nothing cached — the arm EVERY runner runs on until coord's half of
    /// the plan ships — the builtin renders BYTE-IDENTICALLY to the briefing
    /// this build shipped before, under a marker line that is unchanged and a
    /// provenance line that says exactly where the text came from.
    #[test]
    fn builtin_renders_byte_identical_to_todays_briefing() {
        let _pin = pin_plan_capture_level_for_test("off");

        let briefing = runner_context(9876);
        let (marker, provenance, body) = split_render(&briefing);

        assert_eq!(marker, RUNNER_CONTEXT_SOURCE_MARKER);
        assert_eq!(provenance, "[briefing: builtin-fallback]");
        assert_eq!(body, expected_briefing_body());
    }

    /// The same anchor with the fleet-gated clause on: the clause is still
    /// appended as its own paragraph, byte-identically, and gets its own
    /// provenance token on line 2 rather than a line of its own.
    #[test]
    fn builtin_renders_byte_identical_to_todays_briefing_with_the_clause() {
        let _pin = pin_plan_capture_level_for_test(PLAN_CAPTURE_RECORD);

        let briefing = runner_context(9876);
        let (marker, provenance, body) = split_render(&briefing);

        assert_eq!(marker, RUNNER_CONTEXT_SOURCE_MARKER);
        assert_eq!(
            provenance,
            "[briefing: builtin-fallback] [clause: builtin-fallback]"
        );
        assert_eq!(
            body,
            format!("{}\n\n{}", expected_briefing_body(), expected_clause_body())
        );
    }

    /// A cached coord body is rendered with its placeholders substituted, and
    /// line 2 names the document and version in force — the whole honesty
    /// mechanism. Line 1 is untouched, which is what `/whereami`'s unguarded
    /// spawn-SHA parse depends on.
    #[test]
    fn a_coord_body_renders_with_its_version_on_line_two() {
        let pin = pin_plan_capture_level_for_test("off");
        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            briefing_for_test(
                "Edited briefing. Runner HTTP API: {{runner_api_base}}.",
                7,
                BriefingProvenance::Coord,
            ),
        );

        let briefing = runner_context(9876);
        let (marker, provenance, body) = split_render(&briefing);

        assert_eq!(marker, RUNNER_CONTEXT_SOURCE_MARKER);
        assert_eq!(
            provenance,
            "[briefing: coord session_briefing/runner-session v7]"
        );
        assert_eq!(
            body,
            "Edited briefing. Runner HTTP API: http://127.0.0.1:9876."
        );
    }

    /// A body restored from disk that no poll has re-confirmed is labelled
    /// `cached … (stale)`, NEVER `coord`. Claiming a coord version for text the
    /// process has not checked is the one thing the plan says must never
    /// happen.
    #[test]
    fn a_disk_restored_body_is_labelled_stale_not_coord() {
        let pin = pin_plan_capture_level_for_test("off");
        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            briefing_for_test("Restored briefing.", 4, BriefingProvenance::Cached),
        );

        let briefing = runner_context(9876);
        let (_, provenance, body) = split_render(&briefing);

        assert_eq!(provenance, "[briefing: cached v4 (stale)]");
        assert_eq!(body, "Restored briefing.");
    }

    /// The clause is a SEPARATE document with its own version, so line 2 must
    /// account for it too. A render that named only the base would leave half
    /// the injected text unattributed.
    #[test]
    fn the_clause_carries_its_own_provenance_token() {
        let pin = pin_plan_capture_level_for_test(PLAN_CAPTURE_RECORD);
        pin.set_briefing(
            BRIEFING_PLAN_CAPTURE_CLAUSE,
            briefing_for_test("Edited clause.", 3, BriefingProvenance::Coord),
        );

        let briefing = runner_context(9876);
        let (_, provenance, body) = split_render(&briefing);

        assert_eq!(
            provenance,
            "[briefing: builtin-fallback] \
             [clause: coord session_briefing/plan-capture-clause v3]"
        );
        assert!(body.ends_with("\n\nEdited clause."), "{body}");
    }

    /// Every render-time rejection falls back to the builtin AND says which
    /// version it refused. Coord validates the same things at write time; this
    /// end is what keeps a structural guarantee in the process that actually
    /// builds the prompt.
    #[test]
    fn a_body_that_fails_the_render_guard_falls_back_to_the_builtin() {
        let pin = pin_plan_capture_level_for_test("off");
        for bad in [
            // over the 16 KiB ceiling
            "x".repeat(17 * 1024),
            // an unknown placeholder
            "hello {{organization}}".to_string(),
            // a forged runner-owned marker
            "[source: qontinui-runner/runner_context@9.9.9+cafe]\nrules".to_string(),
            // the operator door, which 403s a device JWT
            "see GET https://coord.example.com/coord/prompt-documents".to_string(),
            // tenant/agent identity, named …
            "your tenant_id is 42".to_string(),
            // … and structural
            "you belong to 01a01eb4-718a-7303-825a-94ec0d0ade91".to_string(),
        ] {
            pin.set_briefing(
                BRIEFING_RUNNER_SESSION,
                briefing_for_test(&bad, 11, BriefingProvenance::Coord),
            );

            let briefing = runner_context(9876);
            let (marker, provenance, body) = split_render(&briefing);

            assert_eq!(marker, RUNNER_CONTEXT_SOURCE_MARKER, "body: {bad:.48}");
            assert_eq!(
                provenance, "[briefing: builtin-fallback (rejected coord v11)]",
                "body: {bad:.48}"
            );
            assert_eq!(body, expected_briefing_body(), "body: {bad:.48}");
        }
    }

    /// The FOUR spawn-path call sites of `runner_context` all render the same
    /// string, so the port they pass is the only thing that varies. Pin that
    /// the port really is interpolated rather than hardcoded, since the
    /// substitution path is new.
    #[test]
    fn the_api_port_reaches_the_rendered_briefing() {
        let _pin = pin_plan_capture_level_for_test("off");
        let briefing = runner_context(41234);
        assert!(briefing.contains("http://127.0.0.1:41234"), "{briefing}");
        assert!(!briefing.contains("http://127.0.0.1:9876"), "{briefing}");
    }
}
