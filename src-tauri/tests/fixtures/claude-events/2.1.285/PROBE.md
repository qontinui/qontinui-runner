# Claude Code 2.1.285 — what the CLI really sends (Phase 1 probe)

Plan `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
Phase 1 (kill-or-confirm). Probed 2026-09-30 on Linux (native ELF CLI,
`claude --version` = `2.1.285 (Claude Code)`), Claude Max account, `--model
haiku`, outside any qontinui runner.

The `*.json` files beside this one are type skeletons of every distinct payload
shape observed per event (no values); `tests/claude_event_fixtures.rs` enforces
that. This file answers the plan's five questions. **UNKNOWN always carries the
reason it could not be answered.**

## Verdict in one paragraph

**The statusLine half of the plan is killed for this CLI version.** With any
`statusLine` configured — including one that prints nothing — the footer stops
painting `? for shortcuts` (idle) and `esc to interrupt` (mid-turn); only
`bypass permissions` survives. The runner keys on both missing markers
(`account_migration.rs` `READY_MARKER`/`BUSY_MARKER`, `looping_agent/idle.rs`
`PROCESSING_INDICATORS`, `providerAdapter.ts`), so per the plan's own rule
Phases 6–8 take the transcript + `cachedUsageUtilization` + OAuth-probe variant
and **the carrier must never register a `statusLine`**. The hook half is
confirmed: every event the plan needs fires, with two corrections to the docs
the plan was written from — `StopFailure` carries its error in a key named
`error` (not `error_type`), and `Notification: permission_prompt` is a
*delayed duplicate* of `PermissionRequest` (+6.0 s, only while the dialog is
still open).

## Method

- Probe: `src-tauri/resources/session-restore/probe/` — `probe_settings.json`
  (template; `@@RECORDER@@` / `@@HTTP_PORT@@` placeholders), `recorder.py` (one
  recorder for every hook and the statusLine; writes key/type skeletons plus an
  allow-list of protocol identifiers such as `notification_type`, `reason`,
  `source`, `tool_name`, `error` — never free text), `drive_probe.py` (drives
  interactive sessions in a PTY and renders the screen), `make_fixtures.py`
  (scratch recording → these skeleton files; re-run it for a new CLI version).
- Screen text: the PTY byte stream rendered with **pyte** (VT100 emulator, in a
  throwaway venv), 140×45; hint presence is a substring search of the rendered
  screen, sampled every 0.2–0.5 s. A stray `u` at the top of every render is
  pyte not understanding the CLI's kitty-keyboard query — harmless.
- Isolation: the operator's existing `CLAUDE_CONFIG_DIR` used unchanged (nothing
  written into it by the probe; the CLI itself wrote its normal session state —
  transcripts, history, trust acceptance for the two scratch cwds). Hooks and
  statusLine injected **only** via `--settings <file>`. `--strict-mcp-config`
  (no MCP servers). The parent session's `CLAUDECODE` / `CLAUDE_CODE_*` env
  markers were stripped — inherited, they switch transcript saving off, which
  breaks `--resume`.
- Budget: 13 short model turns (haiku) plus one failed API call.
- Hazard met: the CLI's binary was rewritten in place by an updater several
  times during the probe (exec failed with ETXTBSY / ENOENT; version stayed
  2.1.285). `drive_probe.py` retries the spawn.

## Q1 — footer hints with a statusLine configured

Same scripted session in three variants: no statusLine (control), a statusLine
printing nothing (`sl_empty`), a statusLine printing a marker after a 1.5 s
sleep (`sl_text`). Mid-turn = a Bash tool call running `sleep 8` (5 s under
bypass), sampled ≥ 4 times while the spinner line was on screen.

| Footer hint | control (no statusLine) | statusLine printing nothing | statusLine printing text | Verdict |
|---|---|---|---|---|
| `esc to interrupt` (mid-turn) | PRESENT — footer row `⏸ manual mode on · esc to interrupt · ← for agents` | **ABSENT** (spinner `✢ Levitating… (6s · ↓ 187 tokens)` still shown, footer `⏸ manual mode on · ← for agents`) | **ABSENT** | **disappears** |
| `? for shortcuts` (idle input box) | PRESENT — `⏸ manual mode on · ? for shortcuts · ← for agents` | **ABSENT** | **ABSENT** | **disappears** |
| `bypass permissions` (resume under bypass) | PRESENT — `⏵⏵ bypass permissions on (shift+tab to cycle)` | PRESENT (idle and mid-turn) | not run | survives |
| `until auto-compact:` (near the limit) | UNKNOWN | UNKNOWN | UNKNOWN | **UNKNOWN** |

- The marker text a statusLine prints renders on its own row **above** the
  footer row; it does not bring the hints back.
- Under bypass the control mid-turn footer was not separately sampled; the
  `sl_empty` bypass mid-turn footer showed no `esc to interrupt`.
- **`until auto-compact:` UNKNOWN — reason:** the indicator renders only in the
  context-low band, and reaching it for real costs ~150k tokens of context.
  Two cheaper routes were tried and neither made it render even in the
  **control** session (33.4k tokens of context): `CLAUDE_CODE_AUTO_COMPACT_WINDOW=40000`
  and `CLAUDE_CODE_MAX_CONTEXT_TOKENS=40000`. A static read of the bundled JS
  found the renderer, which is itself conditional:
  `` mt ? `${100-lt}% context used` : `${lt}% until auto-compact` `` — so on
  2.1.285 the marker `context_watcher.rs` / `looping_agent/idle.rs:100` read can
  legitimately be `N% context used` instead, **with or without a statusLine**.
  Whether a statusLine suppresses it could not be settled statically (the gate
  that returns `null` is a minified cross-chunk identifier).
- **Workaround:** none found that keeps the markers. The only lever would be a
  statusLine that prints the marker strings itself — a forgery of the very text
  the runner treats as ground truth, which this plan exists to stop doing.
- **Consequence (the plan's own rule):** losing `? for shortcuts` alone stops
  every migrated or coord-respawned session from receiving its prompt
  (`account_migration.rs` `spawn_prompt_when_idle`), and losing
  `esc to interrupt` declares idle mid-turn (`/exit` typed into a working
  agent). **No statusLine may be registered by the carrier on this CLI.**

## Q2 — does a `--settings` statusLine replace, or lose to, a lower-level one?

- **`--settings` vs project-level `.claude/settings.json`: `--settings` WINS
  and REPLACES.** With both configured, only the `--settings` command ran (the
  recorder logged every run) and only its text (`FLAG-LEVEL-SL`) rendered; the
  project command's text (`PROJECT-LEVEL-SL`) never appeared. There is no
  merge — one command runs.
- **`--settings` vs user-level: UNKNOWN (inferred, not observed).** This box's
  user settings carry no `statusLine`, and the probe is forbidden to write
  under the operator's config dir. The documented precedence
  (flag > local > project > user) plus the observed flag > project make
  "flag wins" the expectation.
- Implication for D3: the carrier's statusLine would *replace* the user's, so a
  wrapper would have to resolve and run the user's inner command itself (as D3
  designs). Moot on 2.1.285 given Q1.

## Q3 — `rate_limits`, run frequency, cancelled runs

- **`rate_limits` present** for this Max account:
  `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}` (both
  numbers). `spend_limit` never appeared. Absent in 8 of 31 recorded runs — the
  runs before the session's first API response (the startup runs).
  Before usage exists, `context_window.{current_usage,used_percentage,
  remaining_percentage}` are `null`, not 0. Undocumented extra keys:
  `prompt_cache.*`, `prompt_id`, `session_name`, `thinking`, `fast_mode`,
  `effort`, `output_style`, `workspace`, `scratchpad_dir`.
- **Run frequency:** 2 runs at startup (0.3 s apart), then one per UI update: a
  one-tool turn produced 3 runs (after the first API response, when the
  permission dialog cleared, after `Stop`); a bypass tool turn produced 3 runs
  during the turn plus 1 after `Stop`.
- **Cancellation:** a newer update, or a permission dialog appearing, sends
  **SIGTERM (15)** to the in-flight run (observed 4×, 0.2–0.45 s after it
  started). The cancelled process kept running after SIGTERM when it handled
  the signal — the last `survived` tick came 1.36 s after SIGTERM, which is
  when the replacing run completed (1.43 s) — and then stopped without any
  further signal being observable (SIGKILL vs pipe teardown: UNKNOWN). A 3 s
  post-SIGTERM grace never completed.
- **Does a cancelled run still get a detached POST out?** Only if the process
  (a) handles or ignores SIGTERM — a Rust binary with the default disposition
  dies on the spot — and (b) finishes inside the ~1.4 s window. Sending the
  POST *before* running any inner command is the only ordering that survives
  routinely. Moot on 2.1.285 given Q1.

## Q4 — which events fire, for what

| Trigger | Events observed (in order) |
|---|---|
| Tool permission ask (Bash, default mode) | `PermissionRequest` (`tool_name: Bash`, `permission_mode: default`, `tool_input`, `permission_suggestions`) at once; then `Notification` (`notification_type: permission_prompt`) **6.0 s later, only if the dialog is still open** (answered after 1 s: no Notification; after 15 s: Notification at +6.0 s) |
| `AskUserQuestion` | `PermissionRequest` (`tool_name: AskUserQuestion`, `tool_input.questions[]`). No `Notification` while it was open (~3 s); whether one follows at +6 s: UNKNOWN (not held open that long). No `elicitation_dialog` observed |
| `ExitPlanMode` | `PermissionRequest` (`tool_name: ExitPlanMode`, `permission_mode: plan`, `tool_input: {}`), then `Notification: permission_prompt` at +6.0 s |
| MCP elicitation | **UNKNOWN** — probe ran with no MCP server (`--strict-mcp-config`); no elicitation-capable server was available to trigger one safely |
| API error (unknown model, `-p`) | `UserPromptSubmit` → **`StopFailure`** (no `Stop`) → `SessionEnd reason: other`. Payload key is **`error`** (string), not `error_type`; the CLI's debug log names the matcher `StopFailure:model_not_found` |
| `/exit` | `SessionEnd reason: prompt_input_exit` |
| SIGTERM to the CLI / end of a `-p` run | `SessionEnd reason: other` |
| `/clear` | `SessionEnd reason: clear`, then `SessionStart source: clear` (30 ms later) |
| `--resume` | `SessionStart source: resume`, with extra keys `context_tokens`, `seconds_since_last_response`, `prompt_cache_likely_expired`, `estimated_cache_write_usd` |
| Ordinary turn | `UserPromptSubmit` (carries `prompt` and `permission_mode`) → `Stop` (carries `last_assistant_message`, `stop_hook_active`, `background_tasks`, `session_crons`) |

- **Notification vs PermissionRequest: duplication, delayed.**
  `PermissionRequest` is the edge (fires immediately, names the tool);
  `Notification: permission_prompt` is a late duplicate of the same ask. A
  reducer must treat it as the same `NeedsYou(Permission)`, never a second ask.
- **`bypassPermissions`:** a write command (`touch …`) under bypass fired **no**
  `PermissionRequest` and **no** `Notification` — only `UserPromptSubmit` /
  `Stop` with `permission_mode: bypassPermissions`. Nothing to phantom-latch on.
- **`StopFailure` error for a usage-limit stop: UNKNOWN.** Triggering one means
  exhausting the account's quota, which the probe must not do.
- **`StopFailure` error for the transient "Server is temporarily limiting
  requests (not your usage limit)" throttle: UNKNOWN.** Server-side and not
  reproducible on demand. Therefore, as the plan already requires, Phase 2
  must not map a rate-limit value to `QuotaExhausted`; the OAuth probe decides.
- Only `model_not_found` was observed as a value. The key name `error` is what
  a projection must read; whether it ever differs from the matcher value is
  UNKNOWN.

## Q5 — `async: true`, `type: "http"`, per-event cost

- **`async: true` is honoured** on `UserPromptSubmit` and on `Stop`. The CLI's
  debug log: `Config-based async hook, backgrounding process … with timeout
  600000ms`. Behaviourally, a 6 s async sleeper on `UserPromptSubmit` was still
  sleeping when the turn's first API response arrived (+1.6 s); a 6 s sleeper
  on `Stop` was still sleeping when the post-turn statusLine update ran
  (+0.3 s).
- **`type: "http"` works.** The CLI POSTs the same JSON body a command hook
  receives on stdin (identical key sets for `UserPromptSubmit` and `Stop` in
  the same session) to the static URL; headers `Accept`, `Accept-Encoding`,
  `Connection`, `Content-Length`, `Content-Type`, `Host`, `User-Agent` (no
  auth). A `200` with body `{}` is accepted as valid hook output. Dispatch →
  response: 5–14 ms on loopback.
- **Per-event wall cost of a native no-op process:**
  - CLI-observed (debug-log timestamps, hook dispatch → the CLI processing the
    hook's output; one sample each, so indicative only): `/bin/true` ≈ 5 ms on
    `UserPromptSubmit`, ≈ 4 ms on `Stop`; the python3 recorder ≈ 20 ms.
  - Host spawn cost, the `$EPOCHREALTIME` method of
    `qontinui-claude-config/scripts/lib/hook-latency.sh` (bash stamps around
    fork+exec+wait, n = 100 each, measured under load average ≈ 95 on 48
    cores): `/bin/true` p50 1.03 ms / p90 2.64 ms / max 6.08 ms;
    `sh -c /bin/true` p50 2.11 / p90 5.38 / max 10.47 ms; `python3 recorder.py`
    p50 22.07 / p90 29.82 / max 267 ms.
  - Whether the CLI launches a command hook through a shell: not verified.
