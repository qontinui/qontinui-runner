#!/usr/bin/env node
/**
 * UI-Bridge test suite for the zone-header session-info dropdown and the
 * restore census (plan
 * `2026-08-16-runner-session-info-dropdown-and-restore-verification`, §6 / P6).
 *
 *   node scripts/uibridge-session-info.test.mjs [flags]
 *
 * Node 20+, ZERO external dependencies — built-in `fetch` only. The UI Bridge is
 * the only frontend-inspection tool here; there is no Playwright, for any
 * reason. If a probe cannot be expressed through the UI Bridge, fix the UI
 * Bridge.
 *
 * ## What it prints, and what the exit code means
 *
 * A per-assertion table with three visually distinct outcomes:
 *
 *   [PASS] +   the assertion was EVALUATED and held
 *   [FAIL] X   the assertion was EVALUATED and did not hold
 *   [SKIP] -   the assertion was NOT EVALUATED, and the row says why
 *
 * A SKIP is never dressed up as coverage: the summary counts each class
 * separately and, whenever anything was skipped, prints an explicit
 * `COVERAGE INCOMPLETE` line naming the unexercised ids. Pass `--strict` to
 * make an unexercised arm exit non-zero too (that is the CI posture; the
 * operator-run posture is exit 0 with the banner).
 *
 *   exit 0  every evaluated assertion passed
 *   exit 1  at least one assertion FAILED
 *   exit 2  no failures, but arms were SKIPPED and --strict was given
 *   exit 3  the harness could not run at all (runner unreachable, bad flags)
 *
 * ## Safety posture (plan §0 / §0.1 — read them before enabling anything)
 *
 * The operator's carve-out for restarting the runner is **conditional on an
 * empty runner that the implementer itself emptied**, and on the test process
 * running OUTSIDE the runner. Both were measured FALSE at vet time. So:
 *
 *   - fixture creation (T0.2-T0.5) requires `--fixture`
 *
 * ## What a FIXTURE is (rewritten 2026-08-20)
 *
 * A fixture is a REAL, terminal-bound provider session, built the way the
 * product builds one: `build_ai_launch_command` composes the flag set, the
 * command is typed into a PTY created by `POST /terminals`, and the runner's
 * own `terminal_session_record_open` writes the spawn-time record.
 *
 * It used to be a `POST /sessions/spawn`, which cannot work: that route makes a
 * HEADLESS session while `/control/sessions/info` projects terminal-bound
 * records, so the waiter blocked on something that could never appear and
 * 18 assertions SKIPped behind it.
 *
 * Two properties are NOT this script's to manufacture:
 *
 *   - `confirmed_at` is written only by the provider's SessionStart hook
 *     (`POST /control/session-open`), so it is proof a provider really started.
 *     This script never posts that route; it waits for it.
 *   - the transcript appears only once the provider writes a conversation to
 *     disk, which is what the trivial T0.3 prompt is for.
 *
 * A real `claude` opens first-run gates before its session starts, and an
 * unanswered gate parks the process at a menu forever. Only gates named in
 * `KNOWN_LAUNCH_GATES` are answered, each with a fixed key sequence that is
 * reported in the run. The external-CLAUDE.md-imports gate is deliberately
 * REFUSED, not answered: it approves loading code from outside the cwd, which
 * is the operator's decision. The default `--fixture-cwd` is an empty temp dir
 * precisely so that gate never opens.
 *   - every restart (T7, T10) requires `--allow-restart`
 *   - BOTH re-run the T0 emptiness + self-hosting gates IMMEDIATELY BEFORE
 *     acting, and abort if either fails
 *   - default is a pure READ-ONLY run: it probes, it reports, it changes
 *     nothing
 *
 * `taskkill /F /IM node.exe`, `Stop-Process -Name node`, `Stop-Process -Name
 * powershell` and ANY process-tree (`/T`) kill are forbidden outright — they
 * terminate live Claude Code sessions, including the caller's own. T10 kills
 * the runner process BY PID and by exact image name, with a hard refusal list.
 *
 * ## Why 127.0.0.1 and why the timeouts look large
 *
 * The runner binds the IPv4 loopback only while Windows resolves `localhost` to
 * `::1` first, so `localhost` pays a doomed connect before the socket that
 * answers (root `CLAUDE.md`, lint check #14). Every URL here is literal
 * `127.0.0.1`. And `/health` has been sampled between 296 ms and 10 120 ms on a
 * loaded box, so timeouts are sized against the TAIL, not the median — there is
 * deliberately no 2 s timeout anywhere in this file.
 */

import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";

// ===========================================================================
// Configuration
// ===========================================================================

/** The port `dev-start.ps1` manages, and this suite's default target. */
const DEFAULT_RUNNER_PORT = 9876;

/** Per-request budget. Sized against the measured 10 120 ms /health tail. */
const REQUEST_TIMEOUT_MS = 30_000;
/** One /health probe inside the restart poll. */
const HEALTH_PROBE_TIMEOUT_MS = 20_000;
/** Total budget for "the runner came back" after a restart. */
const HEALTH_POLL_BUDGET_MS = 300_000;
/** Gap between /health probes while waiting for a restart. */
const HEALTH_POLL_INTERVAL_MS = 3_000;
/** How long the frontend gets to render a panel after a click. */
const PANEL_SETTLE_MS = 1_500;
/** Budget for a fixture session to appear in the durable registry. */
const FIXTURE_APPEAR_BUDGET_MS = 120_000;
/**
 * Budget for a fixture's provider to finish booting and CONFIRM itself.
 *
 * Confirmation is not ours to write: `/control/session-open` is the provider's
 * SessionStart-hook route, and it is the hook POST that flips the spawn-time
 * record from provisional to `confirmed_at`. So this budget covers a real
 * `claude` starting, not a status we can set. Measured warm: ~8 s once the
 * launch gate is answered.
 */
const FIXTURE_CONFIRM_BUDGET_MS = 180_000;
/**
 * Budget for the info triggers to mount after fixtures exist. Sized above the
 * frontend 30s `reconcileClaudeSessionIds` backfill, which is what binds a
 * backend-created tab its `claudeSessionId` and therefore mounts the trigger.
 */
const TRIGGER_MOUNT_BUDGET_MS = 75_000;
/** Budget for a transcript to exist on disk after the trivial prompt. */
const FIXTURE_TRANSCRIPT_BUDGET_MS = 120_000;
/** Gap between fixture-progress polls (buffer reads + restore-health). */
const FIXTURE_POLL_INTERVAL_MS = 4_000;

/** Process image names this script will NEVER kill (they host live sessions). */
const NEVER_KILL = ["node", "node.exe", "powershell", "powershell.exe", "pwsh", "pwsh.exe"];

/** The runner's own image name — the only thing T10 is allowed to stop. */
const RUNNER_PROCESS_NAME = "qontinui-runner";

/** The fourteen addressable row fields (plan D5). */
const INFO_FIELDS = [
  "account",
  "name",
  "terminal-id",
  "claude-session-id",
  "fleet-handle",
  "tenant",
  "task-run",
  "working-dir",
  "provider",
  "origin",
  "opened-at",
  "restore-tier",
  "prs-opened",
  "prs-landed",
];

/** The subset T3 requires to be present once the panel is open. */
const T3_REQUIRED_FIELDS = [
  "account",
  "name",
  "terminal-id",
  "claude-session-id",
  "prs-opened",
  "prs-landed",
];

// ===========================================================================
// Argument parsing
// ===========================================================================

function parseArgs(argv) {
  const opts = {
    port: Number(process.env.RUNNER_PORT || DEFAULT_RUNNER_PORT),
    allowRestart: false,
    fixture: false,
    readOnly: false,
    strict: false,
    accounts: [],
    // Fixture cwd. A directory THIS SCRIPT owns, deliberately outside every
    // repo: a cwd whose CLAUDE.md imports files from outside it makes `claude`
    // open an "Allow external CLAUDE.md file imports?" gate before the session
    // starts, and answering that on the operator's behalf would be approving
    // third-party code they never saw. An empty temp dir raises no such
    // question. Override only if you accept that (see `--fixture-cwd`).
    fixtureCwd: join(tmpdir(), "qontinui-uibridge-fixtures"),
    devStart: process.env.QONTINUI_DEV_START || "C:/qontinui-root/dev-start.ps1",
    devStartExplicit: false,
    oracle: join(tmpdir(), "uibridge-session-info-oracle.json"),
    ffPr: null,
    unknownPr: null,
    repoDir: null,
    allowGitFetch: false,
    json: null,
  };
  const errors = [];
  const take = (i, name) => {
    const inline = argv[i].includes("=") ? argv[i].slice(argv[i].indexOf("=") + 1) : null;
    if (inline !== null) return [inline, i];
    if (i + 1 >= argv.length) {
      errors.push(`${name} requires a value`);
      return [null, i];
    }
    return [argv[i + 1], i + 1];
  };

  for (let i = 0; i < argv.length; i++) {
    const raw = argv[i];
    const flag = raw.includes("=") ? raw.slice(0, raw.indexOf("=")) : raw;
    let v;
    switch (flag) {
      case "--port":
        [v, i] = take(i, "--port");
        opts.port = Number(v);
        if (!Number.isInteger(opts.port) || opts.port <= 0) errors.push(`bad --port ${v}`);
        break;
      case "--allow-restart":
        opts.allowRestart = true;
        break;
      case "--fixture":
        opts.fixture = true;
        break;
      case "--read-only":
        opts.readOnly = true;
        break;
      case "--strict":
        opts.strict = true;
        break;
      case "--accounts":
        [v, i] = take(i, "--accounts");
        opts.accounts = (v || "")
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean);
        break;
      case "--fixture-cwd":
        [v, i] = take(i, "--fixture-cwd");
        opts.fixtureCwd = v;
        break;
      case "--dev-start":
        [v, i] = take(i, "--dev-start");
        opts.devStart = v;
        opts.devStartExplicit = true;
        break;
      case "--oracle":
        [v, i] = take(i, "--oracle");
        opts.oracle = v;
        break;
      case "--ff-pr":
        [v, i] = take(i, "--ff-pr");
        opts.ffPr = v;
        break;
      case "--unknown-pr":
        [v, i] = take(i, "--unknown-pr");
        opts.unknownPr = v;
        break;
      case "--repo-dir":
        [v, i] = take(i, "--repo-dir");
        opts.repoDir = v;
        break;
      case "--allow-git-fetch":
        opts.allowGitFetch = true;
        break;
      case "--json":
        [v, i] = take(i, "--json");
        opts.json = v;
        break;
      case "--help":
      case "-h":
        opts.help = true;
        break;
      default:
        errors.push(`unknown flag ${raw}`);
    }
  }
  return { opts, errors };
}

const HELP = `
node scripts/uibridge-session-info.test.mjs [flags]

  --port <n>            runner port (default 9876, or $RUNNER_PORT).
                        Always dialled as 127.0.0.1, never localhost.
  --read-only           never click anything in the runner UI. T3-T5/T9 SKIP.
  --fixture             CREATE the three T0 fixture sessions. Requires the T0
                        emptiness + self-hosting gates to pass first.
  --allow-restart       permit the T7 / T10 restarts. Default OFF. Each restart
                        re-runs the T0 gates immediately before acting.
  --accounts a,b        Claude account names for the fixture spawns (>=2).
  --fixture-cwd <path>  cwd for fixture sessions (default: a temp dir this
                        script creates). Point it at a repo and the launch
                        opens an external-CLAUDE.md-imports gate this script
                        REFUSES to answer for you.
  --dev-start <path>    dev-start.ps1 (default C:/qontinui-root/dev-start.ps1).
  --oracle <path>       where T6 writes / T8 reads the pre-restart census.
  --ff-pr owner/repo#N  the known ff-landed PR for T11.
  --unknown-pr o/r#N    the known unevaluable-land-signal PR for T11b.
  --repo-dir <path>     checkout used for T11's head-object precondition.
  --allow-git-fetch     let the T11 precondition run 'git fetch origin <base>'.
  --strict              exit 2 when any arm was SKIPPED (CI posture).
  --json <path>         also write the result table as JSON.
  -h, --help            this text.

Default run is READ-ONLY-ish: it probes and reports, creates nothing, restarts
nothing. --fixture and --allow-restart are the only mutating doors, and both are
gated on the plan's §0 conditions being LIVE-verified, not assumed.
`.trimStart();

// ===========================================================================
// HTTP — never throws, never hangs
// ===========================================================================

let BASE = "http://127.0.0.1:9876";

/**
 * One HTTP call. Returns a plain record; a network error, a timeout and a non-2xx
 * are all DATA, never an exception — an unhandled rejection in a diagnostic is
 * indistinguishable from the defect it is meant to observe.
 */
async function http(path, { method = "GET", body, timeoutMs = REQUEST_TIMEOUT_MS } = {}) {
  const url = path.startsWith("http") ? path : BASE + path;
  const started = Date.now();
  try {
    const res = await fetch(url, {
      method,
      headers: body === undefined ? undefined : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(timeoutMs),
    });
    const text = await res.text();
    let json = null;
    try {
      json = JSON.parse(text);
    } catch {
      json = null;
    }
    return {
      ok: res.ok,
      status: res.status,
      json,
      text,
      error: res.ok ? null : `HTTP ${res.status}`,
      ms: Date.now() - started,
      url,
    };
  } catch (err) {
    const kind =
      err?.name === "TimeoutError" ? `timeout after ${timeoutMs}ms` : String(err?.message ?? err);
    return {
      ok: false,
      status: 0,
      json: null,
      text: "",
      error: kind,
      ms: Date.now() - started,
      url,
    };
  }
}

/**
 * Unwrap the runner's `ApiResponse` envelope (`{success, data, error}`), while
 * tolerating a bare body. Returns `{ok, data, error}` — an absent answer is
 * always an ERROR with a reason, never a silent `{}` (served policy
 * `verification-and-evidence` `silent-empty-is-unknown`).
 */
function unwrap(res) {
  if (!res.ok) {
    const detail = res.json?.error ? `${res.error}: ${res.json.error}` : res.error;
    return { ok: false, data: null, error: `${detail} (${res.url})` };
  }
  if (res.json === null) return { ok: false, data: null, error: `non-JSON body from ${res.url}` };
  if (typeof res.json === "object" && "success" in res.json) {
    if (res.json.success === false) {
      return { ok: false, data: null, error: res.json.error || "success:false with no error" };
    }
    return { ok: true, data: res.json.data ?? null, error: null };
  }
  return { ok: true, data: res.json, error: null };
}

async function getJson(path, opts) {
  return unwrap(await http(path, opts));
}

// ===========================================================================
// Result recording
// ===========================================================================

const PASS = "PASS";
const FAIL = "FAIL";
const SKIP = "SKIP";

const results = [];

function record(id, what, status, detail) {
  results.push({ id, what, status, detail: detail ?? "" });
  const mark = status === PASS ? "+" : status === FAIL ? "X" : "-";
  console.log(`  [${status}] ${mark} ${id.padEnd(9)} ${what}`);
  if (detail) {
    for (const line of String(detail).split("\n")) console.log(`             ${line}`);
  }
}

const pass = (id, what, detail) => record(id, what, PASS, detail);
const fail = (id, what, detail) => record(id, what, FAIL, detail);
const skip = (id, what, why) => record(id, what, SKIP, `not exercised: ${why}`);
/** Operator-facing narration that is NOT an assertion — never counted in the
 * PASS/FAIL/SKIP tallies, so it can never be mistaken for coverage. */
const note = (msg) => console.log(`  [note]    ${msg}`);

/** Fold a list of per-item findings into one assertion row. */
function assertAll(id, what, findings, skipReason) {
  if (skipReason) return skip(id, what, skipReason);
  const bad = findings.filter((f) => !f.ok);
  if (findings.length === 0) {
    return skip(id, what, "no subjects to evaluate (see the rows above for why)");
  }
  if (bad.length === 0) return pass(id, what, `${findings.length} subject(s) checked`);
  return fail(id, what, bad.map((f) => f.detail).join("\n"));
}

function section(title) {
  console.log(`\n--- ${title} ${"-".repeat(Math.max(0, 62 - title.length))}`);
}

// ===========================================================================
// Domain helpers
// ===========================================================================

/**
 * Dataset/attribute extractor spanning the three shapes the Control API has
 * used: `state.dataset` (camelCase keys — what the runner serves today),
 * `attributes` and `state.attributes` (kebab `data-*` keys). Returns
 * `{present, value}` so "attribute absent" is distinguishable from
 * "attribute present and empty" — T4 depends on exactly that distinction.
 */
function readDataAttr(element, kebabName) {
  const camel = kebabName
    .replace(/^data-/, "")
    .split("-")
    .map((p, i) => (i === 0 ? p : p[0].toUpperCase() + p.slice(1)))
    .join("");
  const ds = element?.state?.dataset;
  if (ds && typeof ds === "object" && camel in ds) return { present: true, value: ds[camel] };
  for (const bag of [element?.attributes, element?.state?.attributes]) {
    if (bag && typeof bag === "object" && kebabName in bag) {
      return { present: true, value: bag[kebabName] };
    }
  }
  return { present: false, value: undefined };
}

const infoElementId = (field, zoneIndex) => `terminal-session-info-${field}-${zoneIndex}`;

/** `GET /control/sessions/info` → `{ok, status, reason, sessions, error}`. */
async function fetchSessionsInfo() {
  const r = await getJson("/control/sessions/info");
  if (!r.ok) return { ok: false, error: r.error, sessions: [], status: null, reason: null };
  const d = r.data ?? {};
  return {
    ok: true,
    error: null,
    status: d.status ?? null,
    reason: d.reason ?? null,
    sessions: Array.isArray(d.sessions) ? d.sessions : [],
  };
}

/** `GET /control/sessions/restore-census`. */
async function fetchCensus() {
  const r = await getJson("/control/sessions/restore-census");
  if (!r.ok) return { ok: false, error: r.error, census: null };
  return { ok: true, error: null, census: r.data ?? null };
}

/** `GET /control/sessions/restore-health`. */
async function fetchRestoreHealth() {
  const r = await getJson("/control/sessions/restore-health");
  if (!r.ok) return { ok: false, error: r.error, sessions: [], unrestorable: null };
  const d = r.data ?? {};
  return {
    ok: true,
    error: null,
    sessions: Array.isArray(d.sessions) ? d.sessions : [],
    unrestorable: d.unrestorable ?? null,
  };
}

/** `GET /terminals`. */
async function fetchTerminals() {
  const r = await getJson("/terminals");
  if (!r.ok) return { ok: false, error: r.error, terminals: [] };
  const d = r.data ?? {};
  return { ok: true, error: null, terminals: Array.isArray(d.terminals) ? d.terminals : [] };
}

/** `GET /ui-bridge/control/elements` → the registry, normalized to an array. */
async function fetchElements() {
  const r = await getJson("/ui-bridge/control/elements?refresh=true");
  if (!r.ok) return { ok: false, error: r.error, elements: [] };
  const d = r.data;
  const arr = Array.isArray(d) ? d : Array.isArray(d?.elements) ? d.elements : [];
  return { ok: true, error: null, elements: arr };
}

/** `GET /ui-bridge/control/element/<id>` — the only shape carrying attributes. */
async function fetchElement(id) {
  const r = await getJson(`/ui-bridge/control/element/${encodeURIComponent(id)}`);
  if (!r.ok) return { ok: false, error: r.error, element: null };
  const d = r.data;
  const el = d && typeof d === "object" && d.element ? d.element : d;
  return { ok: true, error: null, element: el ?? null };
}

/**
 * `POST /ui-bridge/control/find`. The canonical filter matches `text` against
 * label/text/accessibleName/id, so an element id is a legitimate needle.
 */
async function findElements(criteria) {
  const r = await getJson("/ui-bridge/control/find", { method: "POST", body: criteria });
  if (!r.ok) return { ok: false, error: r.error, elements: [] };
  const d = r.data;
  const arr = Array.isArray(d) ? d : Array.isArray(d?.elements) ? d.elements : [];
  return { ok: true, error: null, elements: arr };
}

async function clickElement(id) {
  const r = await getJson(`/ui-bridge/control/element/${encodeURIComponent(id)}/action`, {
    method: "POST",
    body: { action: "click" },
  });
  return r;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ===========================================================================
// T0 — the two gates
// ===========================================================================

/**
 * The live emptiness measurement. Returns `{empty, detail}`; the CALLER decides
 * whether a non-empty runner is a FAIL (it is, for any mutating mode) or merely
 * a reported measurement (read-only mode, where nothing is conditional on it).
 */
async function measureEmptiness() {
  const terms = await fetchTerminals();
  const health = await fetchRestoreHealth();
  if (!terms.ok)
    return { empty: false, evaluable: false, detail: `GET /terminals: ${terms.error}` };
  if (!health.ok) {
    return {
      empty: false,
      evaluable: false,
      detail: `GET /control/sessions/restore-health: ${health.error}`,
    };
  }
  const alive = terms.terminals.filter((t) => t.isAlive !== false);
  const empty = alive.length === 0 && health.sessions.length === 0;
  const detail =
    `/terminals: ${terms.terminals.length} record(s), ${alive.length} alive; ` +
    `restore-health: ${health.sessions.length} session(s)` +
    (health.sessions.length
      ? `\nsessions: ${health.sessions.map((s) => `${String(s.claudeSessionId).slice(0, 8)}@${String(s.terminalId).slice(0, 8)}`).join(", ")}`
      : "");
  return { empty, evaluable: true, detail, health };
}

/**
 * The §0.1 self-hosting gate: would restarting this runner kill THIS process?
 *
 * The authoritative test is PROCESS ANCESTRY, not the runner's registry.
 * Restarting a runner kills its process tree, so the only thing that decides
 * the question is whether a runner process is an ancestor of this one.
 *
 * ⚠️ Do NOT gate on "is my `CLAUDE_CODE_SESSION_ID` in `/control/sessions/
 * restore-health`". That was this gate's original implementation and it is
 * WRONG in the common direction — it aborts runs that are perfectly safe.
 * The runner tracks Claude sessions it merely NOTICED: `origin: "observed"` is
 * documented in `session_lifecycle_store.rs` as "a claude-process-start-anchored,
 * uniquely-correlated TRANSCRIPT bind — the transcript proves the session
 * exists". A `claude` launched from an ordinary PowerShell window is discovered
 * exactly that way and appears in the registry bound to a terminal id, while
 * being no part of the runner's process tree. Measured 2026-08-17: a session
 * present in `restore-health` as `origin: "observed"` SURVIVED the runner being
 * killed, and its ancestry was
 * `powershell.exe <- claude.exe <- powershell.exe <- WindowsTerminal.exe`.
 *
 * Registry presence is therefore reported as CONTEXT, never as the verdict.
 *
 * Fails CLOSED: if ancestry cannot be walked, we cannot prove we are outside,
 * which is not the same as being outside.
 */
async function measureSelfHosting(health) {
  const own = (process.env.CLAUDE_CODE_SESSION_ID || "").trim();

  // Context only — see the warning above. Never decides `safe`.
  let registryNote = "not checked";
  if (own) {
    const h = health ?? (await fetchRestoreHealth());
    if (h.ok) {
      const hit = h.sessions.find((s) => String(s.claudeSessionId).trim() === own);
      registryNote = hit
        ? `own session ${own} IS in the registry (terminal ${hit.terminalId}, origin ` +
          `${hit.origin ?? "?"}) — context only, not proof of hosting`
        : `own session ${own} is not among the runner's ${h.sessions.length} record(s)`;
    } else {
      registryNote = `restore-health unavailable: ${h.error}`;
    }
  }

  const ancestry = await walkProcessAncestry();
  if (!ancestry.ok) {
    return {
      safe: false,
      evaluable: false,
      detail:
        `cannot walk process ancestry (${ancestry.error}) — cannot prove this process is ` +
        `outside the runner's process tree. Failing closed. [${registryNote}]`,
    };
  }
  const runnerAncestor = ancestry.chain.find((p) => /qontinui-runner/i.test(p.name));
  if (runnerAncestor) {
    return {
      safe: false,
      evaluable: true,
      detail:
        `SELF-HOSTED: ${runnerAncestor.name} (pid ${runnerAncestor.pid}) is an ancestor of this ` +
        `process. Restarting this runner would kill the test mid-run. [${registryNote}]`,
    };
  }
  return {
    safe: true,
    evaluable: true,
    detail:
      `no runner process in this process's ancestry ` +
      `(${ancestry.chain.map((p) => p.name).join(" <- ")}) [${registryNote}]`,
  };
}

/**
 * Walk this process's parent chain. Windows-only via WMI; on other platforms it
 * reads `/proc/<pid>/stat`. Bounded to 16 hops so a cycle cannot hang the gate.
 */
async function walkProcessAncestry() {
  const chain = [];
  try {
    if (process.platform === "win32") {
      const { execFile } = await import("node:child_process");
      const { promisify } = await import("node:util");
      const run = promisify(execFile);
      const ps =
        "$p=$PID;$out=@();for($i=0;$i -lt 16;$i++){" +
        '$o=Get-CimInstance Win32_Process -Filter "ProcessId = $p" -ErrorAction SilentlyContinue;' +
        'if(-not $o){break};$out+="$($o.Name)|$($o.ProcessId)";' +
        "if(-not $o.ParentProcessId -or $o.ParentProcessId -eq 0){break};$p=$o.ParentProcessId};" +
        '$out -join "`n"';
      const { stdout } = await run("powershell.exe", ["-NoProfile", "-Command", ps], {
        timeout: 30_000,
      });
      for (const line of String(stdout).split(/\r?\n/)) {
        const [name, pid] = line.trim().split("|");
        if (name) chain.push({ name, pid: Number(pid) });
      }
    } else {
      const { readFile } = await import("node:fs/promises");
      let pid = process.pid;
      for (let i = 0; i < 16 && pid > 0; i++) {
        const stat = await readFile(`/proc/${pid}/stat`, "utf8");
        const name = stat.slice(stat.indexOf("(") + 1, stat.lastIndexOf(")"));
        const rest = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
        chain.push({ name, pid });
        pid = Number(rest[1]);
      }
    }
  } catch (e) {
    return { ok: false, error: e?.message ?? String(e), chain };
  }
  if (chain.length === 0) return { ok: false, error: "empty ancestry", chain };
  return { ok: true, chain };
}

/**
 * Re-run BOTH gates and return whether a mutating action may proceed. Called
 * once in T0 and AGAIN immediately before every restart in T7/T10 — the plan's
 * "every restart re-runs the emptiness check from T0 first", made mechanical.
 */
async function gatesAllowMutation(label) {
  const e = await measureEmptiness();
  const s = await measureSelfHosting(e.health);
  const ok = e.evaluable && e.empty && s.evaluable && s.safe;
  return {
    ok,
    detail:
      `${label} gate re-check:\n  emptiness: ${e.empty ? "EMPTY" : "NOT EMPTY"} — ${e.detail}\n` +
      `  self-hosting: ${s.safe ? "OUTSIDE the runner" : "BLOCKED"} — ${s.detail}`,
  };
}

// ===========================================================================
// T0 — fixture setup (plan §6 "Fixture setup")
// ===========================================================================

// ===========================================================================
// Fixture plumbing — a fixture is a REAL terminal-bound provider session
// ===========================================================================

/** `POST /ui-bridge/control/page/evaluate` — the UI Bridge's JS door. */
async function evaluatePage(expression) {
  const r = await getJson("/ui-bridge/control/page/evaluate", {
    method: "POST",
    body: { expression, awaitPromise: true },
  });
  if (!r.ok) return { ok: false, value: null, error: r.error };
  return { ok: true, value: r.data?.result?.value ?? null, error: null };
}

/**
 * Invoke a Tauri command through the webview.
 *
 * Arguments travel as BASE64-ENCODED JSON, never as a JS literal, and the
 * reason is not stylistic. A Windows path inside a JS string literal is
 * silently corrupted: in "C:\claude\.claude-gmail" neither \c nor \. is a valid
 * escape, so JS drops both backslashes and yields `C:claude.claude-gmail`. The
 * command then receives a config dir that does not exist, and the failure
 * surfaces far away as "the account never launched" (measured 2026-08-20 — it
 * first read as a bug in the Rust launch builder). Base64 carries no
 * backslashes, so nothing can be re-interpreted on the way in.
 *
 * Wrapped in an IIFE because `page/evaluate` does NOT return the last
 * statement's value of a multi-statement expression — a bare `a; b; c` comes
 * back as `a`.
 */
async function invokeTauri(command, args) {
  const b64 = Buffer.from(JSON.stringify(args ?? {}), "utf8").toString("base64");
  const expr =
    `(function(){var a=JSON.parse(atob(${JSON.stringify(b64)}));` +
    `return window.__TAURI__.core.invoke(${JSON.stringify(command)},a).then(` +
    `function(r){return JSON.stringify({ok:true,data:r===undefined?null:r});},` +
    `function(e){return JSON.stringify({ok:false,error:String(e)});});})()`;
  const r = await evaluatePage(expr);
  if (!r.ok) return { ok: false, data: null, error: `invoke ${command}: ${r.error}` };
  let parsed;
  try {
    parsed = JSON.parse(String(r.value));
  } catch {
    return { ok: false, data: null, error: `invoke ${command}: unparseable answer ${r.value}` };
  }
  if (!parsed.ok) return { ok: false, data: null, error: `invoke ${command}: ${parsed.error}` };
  return { ok: true, data: parsed.data, error: null };
}

/** Write raw text to a terminal PTY stdin (`/terminals/{id}/write` takes base64). */
async function writeTerminal(terminalId, text) {
  return getJson(`/terminals/${encodeURIComponent(terminalId)}/write`, {
    method: "POST",
    body: { data: Buffer.from(text, "utf8").toString("base64") },
  });
}

/** A terminal visible text: base64 buffer, ANSI stripped. */
async function readTerminalText(terminalId) {
  const r = await getJson(`/terminals/${encodeURIComponent(terminalId)}/buffer`);
  if (!r.ok) return { ok: false, text: "", error: r.error };
  const b64 = r.data?.data;
  if (typeof b64 !== "string") return { ok: false, text: "", error: "buffer had no base64 `data`" };
  let raw;
  try {
    raw = Buffer.from(b64, "base64").toString("utf8");
  } catch (e) {
    return { ok: false, text: "", error: `undecodable buffer: ${e}` };
  }
  // eslint-disable-next-line no-control-regex
  const text = raw.replace(/\x1b\[[0-9;?]*[a-zA-Z]/g, "").replace(/\r/g, "");
  return { ok: true, text, error: null };
}

/**
 * The first-run interactive gates a real `claude` opens BEFORE its session
 * starts — and therefore before its SessionStart hook can confirm anything.
 * Until one is answered the provider is not running, so a fixture that ignores
 * them waits out its whole budget on a process parked at a menu (measured
 * 2026-08-20: `confirmed` stayed false for 190 s while the PTY sat on the trust
 * prompt).
 *
 * Two rules keep this from becoming a key-masher:
 *
 *  - ONLY a gate matched here is answered, with a FIXED key sequence that is
 *    reported in the result table. An unrecognized prompt is never guessed at:
 *    the fixture fails and prints the buffer tail.
 *  - The external-imports gate is deliberately in REFUSED_LAUNCH_GATES, not
 *    here. It asks whether to trust files imported from OUTSIDE the cwd — code
 *    the operator has not seen — so answering it automatically would launder a
 *    real security decision through a test.
 */
/**
 * Build a whitespace-TOLERANT matcher from a readable phrase.
 *
 * A TUI paints with cursor positioning rather than spaces, so once the escape
 * sequences are stripped the prompt arrives with NO spaces at all:
 * `Isthisaprojectyoucreatedoroneyoutrust?`. A pattern written with spaces
 * therefore never matches, the gate is never answered, and the provider sits at
 * the menu until the confirm budget expires — surfacing as the misleading
 * "the provider never started".
 *
 * This hid for three green runs because both accounts already trusted the
 * default `--fixture-cwd`, so no gate ever opened. It appeared the moment the
 * suite was pointed at a FRESH directory (2026-08-20), which is the only
 * configuration that exercises the gate path at all.
 */
function gatePhrase(phrase) {
  const escaped = phrase.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(escaped.trim().split(/\s+/).join("\\s*"), "i");
}

const KNOWN_LAUNCH_GATES = [
  {
    id: "folder-trust",
    match: gatePhrase("Is this a project you created or one you trust"),
    keys: ["1", "\r"],
    why: "the fixture cwd is a directory this script created, so trusting it is not a judgement about code the operator has not seen",
  },
];

/** Gates this script REFUSES to answer, with the reason it gives the operator. */
const REFUSED_LAUNCH_GATES = [
  {
    id: "external-claude-md-imports",
    match: gatePhrase("Allow external CLAUDE.md file imports"),
    why: "answering it would approve loading files from OUTSIDE the cwd, which is a security decision belonging to the operator rather than to a test. Use the default --fixture-cwd (an empty temp dir), which never opens this gate.",
  },
];

/**
 * Answer any known launch gate showing in `terminalId`. Returns what it did so
 * the caller can report it; an unknown or refused gate is an ERROR, never a
 * silent skip.
 */
async function answerLaunchGates(terminalId, alreadyAnswered = new Set()) {
  const buf = await readTerminalText(terminalId);
  if (!buf.ok) return { ok: false, answered: [], error: `buffer read: ${buf.error}` };
  const tail = buf.text.slice(-4000);
  for (const gate of REFUSED_LAUNCH_GATES) {
    if (alreadyAnswered.has(gate.id)) continue;
    if (gate.match.test(tail)) {
      return { ok: false, answered: [], error: `launch gate ${gate.id} is open and ${gate.why}` };
    }
  }
  const answered = [];
  for (const gate of KNOWN_LAUNCH_GATES) {
    // ONCE per terminal. The PTY buffer is scrollback, so a gate's text still
    // matches after it has been answered and the provider has moved on —
    // re-answering would type `1` and Enter into a LIVE session as ordinary
    // input, spending tokens and polluting the very transcript T0.3 asserts on.
    if (alreadyAnswered.has(gate.id)) continue;
    if (!gate.match.test(tail)) continue;
    for (const key of gate.keys) {
      const w = await writeTerminal(terminalId, key);
      if (!w.ok) return { ok: false, answered, error: `answering ${gate.id}: ${w.error}` };
      await sleep(600);
    }
    answered.push(gate.id);
  }
  return { ok: true, answered, error: null };
}

/**
 * The zone the FRONTEND assigned a terminal, read from its own layout state.
 *
 * The record `zone_index` must match what the UI rendered, so this mirrors
 * production: `recordSessionOpen` resolves the zone from the live assignments
 * map rather than inventing an index. Backend-created terminals reach the tab
 * list through the mount/close/`terminal-exit` re-sync, and the `useZoneLayout`
 * auto-grow then widens the layout and assigns a zone — so the index exists
 * without this script choosing one (verified: two terminals grew `single` into
 * `split` with assignments {0,1}).
 *
 * Coupled to a storage key on purpose: it is the only place the assignment is
 * observable BEFORE the session id is bound — the zone header label carries
 * that id, which is exactly what we do not have yet.
 */
async function readZoneAssignments(pageId, port) {
  const keys = [`page:${pageId}:qontinui-zone-layout:${port}`, `qontinui-zone-layout:${port}`];
  const expr =
    `(function(){var ks=${JSON.stringify(keys)};` +
    `for(var i=0;i<ks.length;i++){var v=localStorage.getItem(ks[i]);if(v)return v;}return "";})()`;
  const r = await evaluatePage(expr);
  if (!r.ok) return { ok: false, assignments: {}, layoutId: null, error: r.error };
  const raw = String(r.value ?? "");
  if (!raw) return { ok: true, assignments: {}, layoutId: null, error: null };
  try {
    const parsed = JSON.parse(raw);
    return {
      ok: true,
      assignments: parsed?.assignments ?? {},
      layoutId: parsed?.layoutId ?? null,
      error: null,
    };
  } catch (e) {
    return { ok: false, assignments: {}, layoutId: null, error: `unparseable zone layout: ${e}` };
  }
}

/** Zone index the frontend gave `terminalId`, or null when it has not yet. */
function zoneIndexOf(assignments, terminalId) {
  for (const [zone, tid] of Object.entries(assignments ?? {})) {
    if (String(tid) === String(terminalId)) return Number(zone);
  }
  return null;
}

/**
 * The page the UI is actually showing. Fixtures must land there or their zone
 * headers never render, and every T1-T5 assertion reads a rendered header.
 * `/terminal-pages` can legitimately be empty while the frontend holds an
 * active page, so the frontend own state is the authority here.
 */
async function resolveActivePageId(port) {
  const r = await evaluatePage(
    `(function(){return localStorage.getItem("qontinui-terminal-active-page:${port}")||"";})()`,
  );
  const fromUi = r.ok ? String(r.value ?? "").trim() : "";
  if (fromUi) return { pageId: fromUi, source: "frontend active page" };
  const pages = await getJson("/terminal-pages");
  const first = pages.ok ? (pages.data?.pages ?? [])[0]?.id : null;
  if (first) return { pageId: String(first), source: "GET /terminal-pages" };
  return { pageId: "default", source: "fallback" };
}

/** Resolve one `--accounts` name to its roster `config_dir`. */
async function resolveConfigDir(accountName) {
  const r = await invokeTauri("get_claude_config_dirs", {});
  if (!r.ok) return { ok: false, configDir: null, error: r.error };
  const dirs = r.data?.data?.dirs ?? r.data?.dirs ?? [];
  if (!Array.isArray(dirs) || dirs.length === 0) {
    return { ok: false, configDir: null, error: "the machine-global Claude roster is EMPTY" };
  }
  const want = String(accountName).trim().toLowerCase();
  const derive = (d) => {
    const base =
      String(d)
        .replace(/[\\/]+$/, "")
        .split(/[\\/]/)
        .pop() ?? "";
    return (base.match(/^\.claude-(.+)$/)?.[1] ?? base).toLowerCase();
  };
  const hit =
    dirs.find((d) => derive(d) === want) || dirs.find((d) => String(d).toLowerCase() === want);
  if (!hit) {
    return {
      ok: false,
      configDir: null,
      error: `account ${accountName} is not in the roster (${dirs.map(derive).join(", ")})`,
    };
  }
  return { ok: true, configDir: hit, error: null };
}

/**
 * Build ONE fixture: a real, TERMINAL-BOUND provider session.
 *
 * The previous implementation posted `/sessions/spawn` and waited for the
 * session to show up in `/control/sessions/info`. It never could.
 * `/sessions/spawn` creates a HEADLESS AI session (`mcp/sessions.rs` — register,
 * then `send_initial_prompt`) and answers `state: "ready"`, while
 * `/control/sessions/info` projects `TerminalSessionRecord`s out of the
 * `SessionLifecycleStore` — terminal-bound sessions. A headless spawn is not a
 * terminal, so the waiter blocked on something that could not arrive and every
 * downstream arm SKIPped behind it (measured 2026-08-20: created 0/3, three
 * 120 s timeouts, while the spawn had genuinely launched `claude` each time).
 *
 * So this builds the session the way the product does — the same sequence
 * `TerminalPage.handleLaunchAiSession` runs, minus the menu:
 *
 *   1. resolve the account to its roster `config_dir`;
 *   2. `build_ai_launch_command` composes the flag set. It is the SINGLE source
 *      of truth (the operator per-account override and the machine-global
 *      template layer in there), so the fixture cannot drift from what a real
 *      launch types;
 *   3. `POST /terminals` with `initial_command`, on the page the UI is showing;
 *   4. take the zone the FRONTEND assigned — never invent one;
 *   5. `terminal_session_record_open` writes the spawn-time record, which is
 *      deliberately PROVISIONAL (`confirmed_at: None`);
 *   6. answer the first-run launch gate, then wait for the provider to CONFIRM
 *      itself.
 *
 * Step 6 is the load-bearing one. This script does not and must not write
 * `confirmed_at`: that flips only when the provider SessionStart hook POSTs
 * `/control/session-open`, which happens only if a real provider actually
 * started in that terminal. Forging it here — by calling that route directly —
 * would make T0.3 assert a fact the fixture had manufactured, which is worth
 * less than no fixture at all.
 */
async function spawnFixtureSession(spec, ctx) {
  const { taskName, account } = spec;
  const { pageId, port, fixtureCwd } = ctx;

  const cd = await resolveConfigDir(account);
  if (!cd.ok) return { ok: false, error: `${taskName}: ${cd.error}` };

  const sessionId = randomUUID();
  const built = await invokeTauri("build_ai_launch_command", {
    configDir: cd.configDir,
    sessionId,
    isWindows: process.platform === "win32",
  });
  if (!built.ok) return { ok: false, error: `${taskName}: ${built.error}` };
  const command = built.data?.data?.command ?? built.data?.command ?? null;
  const pinned = built.data?.data?.pinnedSessionId ?? built.data?.pinnedSessionId ?? null;
  if (!command) {
    return { ok: false, error: `${taskName}: build_ai_launch_command returned no command` };
  }
  if (!pinned) {
    // An opaque per-account alias was configured, so the runner could not pin
    // `--session-id`. Production falls back to a freshest-mtime capture; a
    // fixture must not, because a guessed id cannot be asserted against.
    return {
      ok: false,
      error:
        `${taskName}: the launch command for '${account}' is an opaque alias with no --session-id pin, ` +
        `so the session id cannot be known up front. Clear that account custom launch command to build fixtures on it.`,
    };
  }

  const created = await getJson("/terminals", {
    method: "POST",
    body: {
      page_id: pageId,
      title: taskName,
      working_dir: fixtureCwd,
      initial_command: command,
    },
  });
  if (!created.ok) return { ok: false, error: `${taskName}: POST /terminals: ${created.error}` };
  const terminalId = created.data?.id;
  if (!terminalId) return { ok: false, error: `${taskName}: POST /terminals returned no id` };
  const workingDir = created.data?.workingDir ?? fixtureCwd;

  // The zone is the FRONTEND's to decide. Backend-created terminals reach the
  // tab list through the re-sync, and the layout auto-grow assigns an index;
  // taking that index (rather than picking one) is what keeps the record and
  // the rendered header agreeing, which is exactly what T1 checks.
  let zoneIndex = null;
  const zoneDeadline = Date.now() + FIXTURE_APPEAR_BUDGET_MS;
  while (Date.now() < zoneDeadline) {
    const za = await readZoneAssignments(pageId, port);
    if (za.ok) {
      const z = zoneIndexOf(za.assignments, terminalId);
      if (z !== null) {
        zoneIndex = z;
        break;
      }
    }
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  if (zoneIndex === null) {
    return {
      ok: false,
      error:
        `${taskName}: the frontend never assigned terminal ${terminalId} a zone within ` +
        `${FIXTURE_APPEAR_BUDGET_MS}ms — it is on page ${pageId}, which the UI may not be showing`,
    };
  }

  const rec = await invokeTauri("terminal_session_record_open", {
    claudeSessionId: pinned,
    configDir: cd.configDir,
    workingDir,
    pageId,
    zoneIndex,
    title: taskName,
    terminalId,
    origin: "authoritative",
    provider: "claude",
  });
  if (!rec.ok) return { ok: false, error: `${taskName}: ${rec.error}` };

  const answered = new Set();
  let confirmed = false;
  const confirmDeadline = Date.now() + FIXTURE_CONFIRM_BUDGET_MS;
  while (Date.now() < confirmDeadline) {
    const rh = await fetchRestoreHealth();
    const row = rh.ok ? rh.sessions.find((s) => s.claudeSessionId === pinned) : null;
    if (row?.confirmed) {
      confirmed = true;
      break;
    }
    const gate = await answerLaunchGates(terminalId, answered);
    if (!gate.ok) return { ok: false, error: `${taskName}: ${gate.error}` };
    for (const g of gate.answered) answered.add(g);
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  if (!confirmed) {
    const buf = await readTerminalText(terminalId);
    const tail = buf.ok
      ? buf.text.slice(-360).replace(/\s+/g, " ").trim()
      : `(PTY buffer unreadable: ${buf.error})`;
    return {
      ok: false,
      error:
        `${taskName}: never confirmed within ${FIXTURE_CONFIRM_BUDGET_MS}ms. Confirmation comes from ` +
        `the provider SessionStart hook, so this means the provider never started. PTY tail: ${tail}`,
    };
  }

  const infoDeadline = Date.now() + FIXTURE_APPEAR_BUDGET_MS;
  while (Date.now() < infoDeadline) {
    const info = await fetchSessionsInfo();
    if (info.ok && info.status === "ok") {
      const session = info.sessions.find((s) => s.identity?.claudeSessionId === pinned);
      if (session) {
        return {
          ok: true,
          session,
          terminalId,
          zoneIndex,
          gatesAnswered: [...answered],
        };
      }
    }
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  return {
    ok: false,
    error:
      `${taskName}: confirmed, but never appeared in /control/sessions/info within ` +
      `${FIXTURE_APPEAR_BUDGET_MS}ms (session ${pinned})`,
  };
}

async function createFixtures(ctx) {
  const { opts } = ctx;
  const accounts = opts.accounts;
  // A SECOND account is needed by exactly ONE arm (T5's cross-session account
  // distinctness). Every other arm needs only three sessions. Refusing to build
  // any fixtures without two accounts therefore collapses ~18 assertions to 2 on
  // a single-account machine -- measured 2026-08-18, where the roster
  // (%APPDATA%/com.qontinui.runner/claude-accounts.json) is EMPTY, i.e. one
  // default account. Coupling an optional precondition to a mandatory one is the
  // wrong trade: it converts "one arm is unprovable here" into "nothing is
  // proved", which is exactly the silent-coverage-loss this suite exists to
  // prevent.
  //
  // So: zero accounts named -> still refuse (we will not guess a roster we
  // cannot see). One account -> build all three fixtures on it, and let T5's
  // distinctness arm SKIP for the honest reason, while T5's nameSource branch
  // (operator vs derived) still runs.
  if (accounts.length < 1) {
    skip(
      "T0.2",
      "three fixture sessions across >=2 Claude accounts, in distinct zones",
      "--fixture needs at least one name in --accounts; the roster is machine-global and this script will not guess account names",
    );
    skip(
      "T0.3",
      "a trivial prompt in each fixture session (transcript + confirmed_at)",
      "no fixtures",
    );
    skip("T0.4", "one operator-renamed session and one Claude-derived one", "no fixtures");
    return;
  }
  const singleAccount = accounts.length < 2;
  ctx.singleAccount = singleAccount;
  if (singleAccount) {
    note(
      `only one account ('${accounts[0]}') was supplied — building all three fixtures on it. ` +
        `T5's cross-account distinctness arm will SKIP; every other arm still runs.`,
    );
  }

  // Fixtures run in a directory this script owns, deliberately outside every
  // repo — see `opts.fixtureCwd`. Created here so the very first run does not
  // fail on a missing cwd.
  try {
    mkdirSync(opts.fixtureCwd, { recursive: true });
  } catch (e) {
    fail(
      "T0.2",
      "three fixture sessions across >=2 Claude accounts, in distinct zones",
      `could not create the fixture cwd ${opts.fixtureCwd}: ${e}`,
    );
    skip("T0.3", "a trivial prompt in each fixture session (transcript + confirmed_at)", "no cwd");
    skip("T0.4", "one operator-renamed session and one Claude-derived one", "no cwd");
    return;
  }

  // Fixtures must land on the page the UI is SHOWING or their zone headers
  // never render, and every T1-T5 assertion reads a rendered header.
  const page = await resolveActivePageId(opts.port);
  const fixtureCtx = { pageId: page.pageId, port: opts.port, fixtureCwd: opts.fixtureCwd };
  note(
    `fixtures: cwd ${opts.fixtureCwd}; page ${page.pageId} (${page.source}); ` +
      `accounts ${accounts.join(", ")}`,
  );

  const specs = [
    { taskName: "uibridge-fixture-a", account: accounts[0] },
    { taskName: "uibridge-fixture-b", account: accounts[singleAccount ? 0 : 1] },
    { taskName: "uibridge-fixture-c", account: accounts[0] },
  ];
  // Snapshot BEFORE creating anything, so a fixture that fails partway can be
  // torn down. A failed fixture otherwise leaves a terminal and/or a durable
  // record behind, and the NEXT run's T0.1 emptiness gate then refuses to build
  // anything at all -- one orphaned record aborted the following run entirely
  // (observed 2026-08-20). Cleaning up after ourselves keeps a single bad run
  // from poisoning the suite until someone clears it by hand.
  const beforeTerminals = new Set(((await fetchTerminals()).terminals || []).map((t) => t.id));
  const beforeRecords = new Set(
    ((await fetchRestoreHealth()).sessions || []).map((x) => x.claudeSessionId),
  );
  const created = [];
  const problems = [];
  const gatesAnswered = new Set();
  for (const spec of specs) {
    const r = await spawnFixtureSession(spec, fixtureCtx);
    if (!r.ok) {
      problems.push(r.error);
      continue;
    }
    for (const g of r.gatesAnswered ?? []) gatesAnswered.add(g);
    created.push({ spec, session: r.session, terminalId: r.terminalId, zoneIndex: r.zoneIndex });
  }
  ctx.fixtures = created;

  if (problems.length > 0) {
    const keepTerminals = new Set(created.map((x) => x.terminalId));
    const keepSessions = new Set(created.map((x) => x.session.identity?.claudeSessionId));
    const nowTerminals = await fetchTerminals();
    const strayTerminals = nowTerminals.ok
      ? nowTerminals.terminals
          .map((t) => t.id)
          .filter((id) => !beforeTerminals.has(id) && !keepTerminals.has(id))
      : [];
    for (const id of strayTerminals) {
      await getJson(`/terminals/${encodeURIComponent(id)}`, { method: "DELETE" });
    }
    const nowRecords = await fetchRestoreHealth();
    const strayRecords = nowRecords.ok
      ? nowRecords.sessions
          .map((x) => x.claudeSessionId)
          .filter((sid) => !beforeRecords.has(sid) && !keepSessions.has(sid))
      : [];
    for (const sid of strayRecords) {
      await invokeTauri("terminal_session_record_close", {
        claudeSessionId: sid,
        reason: "orphaned by a failed uibridge fixture",
      });
    }
    if (strayTerminals.length > 0 || strayRecords.length > 0) {
      note(
        `cleaned up after failed fixture(s): ${strayTerminals.length} terminal(s), ` +
          `${strayRecords.length} durable record(s)`,
      );
    }
  }
  // Every automated answer is reported. A launch gate answered silently is a
  // decision made on the operator's behalf without a record of it.
  if (gatesAnswered.size > 0) {
    note(`answered first-run launch gate(s): ${[...gatesAnswered].join(", ")}`);
  }

  // Settle the zones, THEN re-record.
  //
  // Each fixture recorded the zone it held at ITS creation, but the layout is
  // still growing at that point: terminal #2 turns `single` into `split` and
  // #3 turns that into `quad`, and the frontend reflows the assignments each
  // time. A per-fixture read therefore captures an index that is already
  // stale when the next fixture lands — measured 2026-08-20, where all three
  // records said zone 0 while the UI had them in 0, 1 and 2.
  //
  // Re-recording against the settled map is not a workaround bolted on here;
  // it is the same move the product makes when a session is dragged between
  // zones (TerminalPage's zone-move backstop re-records with the new index).
  if (created.length > 0) {
    // WAIT for the layout to finish growing before reading it. The frontend
    // reflows asynchronously (re-sync -> auto-grow -> assign), so a read taken
    // the instant the last fixture confirms still shows the pre-growth map —
    // which is how the first attempt at this fix still recorded every fixture
    // at zone 0 (measured 2026-08-20). Settled means: every fixture terminal
    // has an assignment, and the assignments are distinct.
    let settled = { ok: false, assignments: {}, error: "never read" };
    const settleDeadline = Date.now() + FIXTURE_APPEAR_BUDGET_MS;
    while (Date.now() < settleDeadline) {
      settled = await readZoneAssignments(fixtureCtx.pageId, fixtureCtx.port);
      if (settled.ok) {
        const zones = created.map((c) => zoneIndexOf(settled.assignments, c.terminalId));
        if (zones.every((z) => z !== null) && new Set(zones).size === zones.length) break;
      }
      await sleep(FIXTURE_POLL_INTERVAL_MS);
    }
    note(
      `zone layout after settle: layoutId=${settled.layoutId ?? "?"} ` +
        `assignments=${JSON.stringify(settled.assignments ?? {})} ` +
        `fixtures=${JSON.stringify(created.map((c) => ({ t: c.terminalId, z: c.zoneIndex })))}` +
        (settled.ok ? "" : ` (read failed: ${settled.error})`),
    );
    if (settled.ok) {
      for (const c of created) {
        const zone = zoneIndexOf(settled.assignments, c.terminalId);
        // Compare against the DURABLE value, not against the index this script
        // intended to write. Something between the pre-confirmation record and
        // here resets `zone_index` to 0 — the SessionStart hook preserves a
        // prior record's zone (`record_session_open_into`), so a record it
        // creates FIRST is the likely resetter, but the harness does not need
        // to know which writer won: it needs the record to end up agreeing with
        // the rendered grid. Comparing intent-to-intent skipped every
        // re-record while the stored zone was still 0 (measured 2026-08-20:
        // the settle read was correct and the fixtures already carried
        // z=0,1,2, yet all three records read 0).
        const stored = c.session.placement?.zoneIndex ?? null;
        if (zone === null || zone === stored) continue;
        const ident = c.session.identity ?? {};
        const rr = await invokeTauri("terminal_session_record_open", {
          claudeSessionId: ident.claudeSessionId,
          configDir: c.session.account?.configDir,
          workingDir: c.session.placement?.workingDir,
          pageId: fixtureCtx.pageId,
          zoneIndex: zone,
          title: c.spec.taskName,
          terminalId: c.terminalId,
          origin: "authoritative",
          provider: "claude",
        });
        if (rr.ok) c.zoneIndex = zone;
        else problems.push(`${c.spec.taskName}: re-record to zone ${zone}: ${rr.error}`);
      }
      // Re-read so T0.2 asserts on the durable record, not on local state.
      const after = await fetchSessionsInfo();
      if (after.ok && after.status === "ok") {
        for (const c of created) {
          const fresh = after.sessions.find(
            (x) => x.identity?.claudeSessionId === c.session.identity?.claudeSessionId,
          );
          if (fresh) c.session = fresh;
        }
      }
    } else {
      problems.push(`could not read the settled zone layout: ${settled.error}`);
    }
  }

  const zones = new Set(created.map((c) => c.session.placement?.zoneIndex));
  const accountLabels = new Set(created.map((c) => c.session.account?.label).filter(Boolean));
  if (problems.length || created.length !== 3) {
    fail(
      "T0.2",
      "three fixture sessions across >=2 Claude accounts, in distinct zones",
      [`created ${created.length}/3`, ...problems].join("\n"),
    );
  } else if (zones.size !== 3) {
    fail(
      "T0.2",
      "three fixture sessions across >=2 Claude accounts, in distinct zones",
      `zone indices were ${[...zones].join(", ")} — not three distinct zones`,
    );
  } else if (accountLabels.size < 2) {
    fail(
      "T0.2",
      "three fixture sessions across >=2 Claude accounts, in distinct zones",
      `only ${accountLabels.size} distinct account label(s) observed: ${[...accountLabels].join(", ")}`,
    );
  } else {
    pass(
      "T0.2",
      "three fixture sessions across >=2 Claude accounts, in distinct zones",
      `zones ${[...zones].join(",")}; accounts ${[...accountLabels].join(",")}`,
    );
  }

  // Step 3 — a trivial prompt each, so every fixture has a real transcript and
  // a `confirmed_at`. Trivial on purpose: cost near-nothing, finish fast.
  // Submit the prompt, then assert what the name PROMISES. This used to assert
  // only that the POST returned 200, which is not evidence of either half: a
  // transcript exists only once the provider has written a conversation to
  // disk, and `confirmed_at` is set only by its SessionStart hook. Both are
  // read back from `/control/sessions/restore-health`, which reports them per
  // open record.
  const promptFindings = [];
  for (const c of created) {
    const tid = c.session.identity.terminalId;
    const sid = c.session.identity.claudeSessionId;
    const r = await getJson(`/terminals/${encodeURIComponent(tid)}/submit-prompt`, {
      method: "POST",
      body: { message: "echo uibridge-fixture-ready" },
    });
    if (!r.ok) {
      promptFindings.push({ ok: false, detail: `submit-prompt to ${tid}: ${r.error}` });
      continue;
    }
    let row = null;
    const deadline = Date.now() + FIXTURE_TRANSCRIPT_BUDGET_MS;
    while (Date.now() < deadline) {
      const rh = await fetchRestoreHealth();
      row = rh.ok ? (rh.sessions.find((x) => x.claudeSessionId === sid) ?? null) : null;
      if (row?.transcriptExists && row?.confirmed) break;
      await sleep(FIXTURE_POLL_INTERVAL_MS);
    }
    if (row?.transcriptExists && row?.confirmed) {
      promptFindings.push({ ok: true, detail: "" });
    } else if (!row) {
      promptFindings.push({
        ok: false,
        detail: `${sid} vanished from /control/sessions/restore-health after its prompt`,
      });
    } else {
      promptFindings.push({
        ok: false,
        detail:
          `${sid}: confirmed=${row.confirmed} transcriptExists=${row.transcriptExists} after ` +
          `${FIXTURE_TRANSCRIPT_BUDGET_MS}ms — the prompt was accepted but did not produce both`,
      });
    }
  }
  assertAll(
    "T0.3",
    "a trivial prompt in each fixture session (transcript + confirmed_at)",
    promptFindings,
  );

  // Step 4 — one operator `/rename`, one left Claude-derived, so BOTH branches
  // of the name-provenance split are covered rather than only the default one.
  if (created.length === 0) {
    skip(
      "T0.4",
      "one operator-renamed session and one Claude-derived one",
      "no fixtures were created",
    );
    return;
  }
  const renameTarget = created[0];
  const r = await getJson(
    `/terminals/${encodeURIComponent(renameTarget.session.identity.terminalId)}/submit-prompt`,
    { method: "POST", body: { message: "/rename uibridge-fixture-operator-named" } },
  );
  if (!r.ok) {
    fail(
      "T0.4",
      "one operator-renamed session and one Claude-derived one",
      `/rename submit failed: ${r.error}`,
    );
    return;
  }
  await sleep(PANEL_SETTLE_MS * 4);
  const after = await fetchSessionsInfo();
  const renamed = after.sessions.find(
    (s) => s.identity?.claudeSessionId === renameTarget.session.identity.claudeSessionId,
  );
  const derived = after.sessions.find(
    (s) =>
      created.some((c) => c.session.identity.claudeSessionId === s.identity?.claudeSessionId) &&
      s.identity?.claudeSessionId !== renameTarget.session.identity.claudeSessionId &&
      s.name?.source === "derived",
  );
  if (renamed?.name?.source === "operator" && derived) {
    pass(
      "T0.4",
      "one operator-renamed session and one Claude-derived one",
      `operator: ${renamed.name.value}; derived: ${derived.name.value}`,
    );
  } else {
    fail(
      "T0.4",
      "one operator-renamed session and one Claude-derived one",
      `renamed session name.source=${renamed?.name?.source ?? "?"}; a 'derived' sibling was ${derived ? "found" : "NOT found"}`,
    );
  }
}

/**
 * T0 — the two gates, then (only behind `--fixture`) the fixture population.
 *
 * The gates are recorded as assertions in their own right. In a read-only run
 * NOTHING is conditional on them, so a non-empty runner is reported as a
 * measurement (SKIP with the numbers) rather than manufactured into a failure;
 * the moment a mutating mode is requested they become hard FAILs that abort the
 * mutation, which is what the plan's §0 conditions actually demand.
 */
async function runT0(ctx) {
  section("T0 — fixture gates");
  const mutating = ctx.opts.fixture || ctx.opts.allowRestart;

  const e = await measureEmptiness();
  ctx.gates.empty = e.evaluable && e.empty;
  if (!e.evaluable) {
    fail("T0.1", "the runner is EMPTY (zero terminals, zero restore-health records)", e.detail);
  } else if (e.empty) {
    pass("T0.1", "the runner is EMPTY (zero terminals, zero restore-health records)", e.detail);
  } else if (mutating) {
    fail(
      "T0.1",
      "the runner is EMPTY (zero terminals, zero restore-health records)",
      `${e.detail}\nThe §0 authorizations are conditional on an empty runner the implementer itself emptied. ` +
        `Fixture creation and every restart are ABORTED.`,
    );
  } else {
    skip(
      "T0.1",
      "the runner is EMPTY (zero terminals, zero restore-health records)",
      `read-only run — nothing is conditional on emptiness here. MEASURED NOT EMPTY: ${e.detail}. ` +
        `A --fixture or --allow-restart run would ABORT on this.`,
    );
  }

  const s = await measureSelfHosting(e.health);
  ctx.gates.notSelfHosted = s.evaluable && s.safe;
  if (s.evaluable && s.safe) {
    pass("T0.1b", "the test process's own session is NOT hosted in this runner", s.detail);
  } else if (mutating) {
    fail("T0.1b", "the test process's own session is NOT hosted in this runner", s.detail);
  } else {
    skip(
      "T0.1b",
      "the test process's own session is NOT hosted in this runner",
      `read-only run — no restart is attempted, so nothing is conditional on it. MEASURED: ${s.detail}`,
    );
  }

  if (!ctx.opts.fixture) {
    const why = "--fixture not given (default is a non-mutating run)";
    skip("T0.2", "three fixture sessions across >=2 Claude accounts, in distinct zones", why);
    skip("T0.3", "a trivial prompt in each fixture session (transcript + confirmed_at)", why);
    skip("T0.4", "one operator-renamed session and one Claude-derived one", why);
  } else if (!ctx.gates.empty || !ctx.gates.notSelfHosted) {
    const why = "the T0 gates did not hold — fixture creation is aborted (plan §0/R4)";
    skip("T0.2", "three fixture sessions across >=2 Claude accounts, in distinct zones", why);
    skip("T0.3", "a trivial prompt in each fixture session (transcript + confirmed_at)", why);
    skip("T0.4", "one operator-renamed session and one Claude-derived one", why);
  } else {
    await createFixtures(ctx);
  }
}

// ===========================================================================
// T1-T5 — the dropdown
// ===========================================================================

/**
 * The rendered zone headers, each with the session the UI claims is bound to it.
 * The pivot the whole dropdown suite runs on: ids are ZONE-indexed (matching
 * the existing `terminal-zone-header-<n>` convention), so a UI Bridge assertion
 * reads "zone N shows session X" — precisely the placement claim restore has to
 * get right.
 */
async function collectZones() {
  const reg = await fetchElements();
  if (!reg.ok) return { ok: false, error: reg.error, zones: [] };
  const headerIds = reg.elements
    .map((e) => String(e?.id ?? ""))
    .filter((id) => /^terminal-zone-header-\d+$/.test(id));
  const zones = [];
  for (const headerId of headerIds) {
    const zoneIndex = Number(headerId.slice("terminal-zone-header-".length));
    const el = await fetchElement(headerId);
    if (!el.ok) {
      zones.push({ headerId, zoneIndex, error: el.error, claudeSessionId: null, zoneAttr: null });
      continue;
    }
    const sid = readDataAttr(el.element, "data-claude-session-id");
    const zi = readDataAttr(el.element, "data-zone-index");
    zones.push({
      headerId,
      zoneIndex,
      error: null,
      element: el.element,
      claudeSessionId: sid.present ? String(sid.value) : null,
      zoneAttr: zi.present ? String(zi.value) : null,
    });
  }
  return { ok: true, error: null, zones, registry: reg.elements };
}

/** The backend's own value for one addressable row — the RAW half of D5. */
function backendValueForField(body, field) {
  const { identity, name, account, placement, lifecycle, prs } = body;
  const prsOk = prs?.status === "ok";
  switch (field) {
    case "account":
      return account?.label ?? null;
    case "name":
      return name?.value ?? null;
    case "terminal-id":
      return identity?.terminalId ?? null;
    case "claude-session-id":
      return identity?.claudeSessionId ?? null;
    case "fleet-handle":
      return identity?.fleetSessionHandle ?? null;
    case "tenant":
      return identity?.tenantId ?? null;
    case "task-run":
      return identity?.taskRunId ?? null;
    case "working-dir":
      return placement?.workingDir ?? null;
    case "provider":
      return lifecycle?.provider ?? null;
    case "origin":
      return lifecycle?.origin ?? null;
    case "opened-at":
      return lifecycle?.openedAt === undefined || lifecycle?.openedAt === null
        ? null
        : String(lifecycle.openedAt);
    case "restore-tier":
      return lifecycle?.restoreTier ?? null;
    case "prs-opened":
      return prsOk ? String(prs.openCount) : null;
    case "prs-landed":
      return prsOk ? String(prs.landedCount) : null;
    default:
      return null;
  }
}

/**
 * Compare ONE rendered row against ONE backend value — raw against raw, never
 * scraped display text.
 *
 * The unknown contract is load-bearing: when a value was never observed the
 * component OMITS `data-session-info-value` and sets
 * `data-session-info-unknown="true"`. So an absent attribute + the unknown flag
 * MATCHES a `null` backend value, and an absent attribute must NEVER be read as
 * the empty string — "" is a value the runner would be asserting, and it did
 * not.
 */
function compareRow(element, field, expected, label) {
  const val = readDataAttr(element, "data-session-info-value");
  const unk = readDataAttr(element, "data-session-info-unknown");
  const fieldAttr = readDataAttr(element, "data-session-info-field");
  const isUnknown = unk.present && String(unk.value) === "true";

  if (fieldAttr.present && String(fieldAttr.value) !== field) {
    return {
      ok: false,
      detail: `${label}: data-session-info-field='${fieldAttr.value}', expected '${field}'`,
    };
  }
  if (expected === null) {
    if (!val.present && isUnknown) return { ok: true, detail: "" };
    if (val.present) {
      return {
        ok: false,
        detail: `${label}: backend says UNKNOWN (null) but the row carries data-session-info-value='${val.value}'`,
      };
    }
    return {
      ok: false,
      detail: `${label}: backend says UNKNOWN (null); the row omits the value attribute but does NOT set data-session-info-unknown="true" — absence alone is ambiguous`,
    };
  }
  if (!val.present) {
    return {
      ok: false,
      detail: `${label}: backend value '${expected}' but the row omits data-session-info-value${isUnknown ? ' and claims data-session-info-unknown="true"' : ""}`,
    };
  }
  if (String(val.value) !== String(expected)) {
    return { ok: false, detail: `${label}: rendered '${val.value}' != backend '${expected}'` };
  }
  if (isUnknown) {
    return {
      ok: false,
      detail: `${label}: carries a value AND data-session-info-unknown="true" — contradictory`,
    };
  }
  return { ok: true, detail: "" };
}

/**
 * T3/T4/T5 as one pass, parameterised by phase so T9 can re-run the identical
 * assertions against the restarted runner rather than a near-copy of them.
 */
/**
 * Make sure zone `zoneIndex`'s panel is OPEN, and say so honestly if it cannot
 * be.
 *
 * Only one session-info panel is open at a time: opening the next zone's closes
 * the previous one, and the closed panel's rows leave the element registry with
 * it. T3 could not see this because it re-reads the registry immediately after
 * each click, while its panel is still up; T4 ran afterwards and fetched rows
 * for zones whose panels had since closed, so every row 404'd for every zone
 * except the last one opened (measured 2026-08-20: 26 spurious failures across
 * zones 0 and 1, none in zone 2).
 *
 * The trigger TOGGLES, so a click on an already-open panel closes it. Check
 * first, then click at most twice.
 */
async function ensurePanelOpen(zoneIndex) {
  const panelId = infoElementId("panel", zoneIndex);
  const triggerId = infoElementId("trigger", zoneIndex);
  for (let attempt = 0; attempt < 2; attempt++) {
    const probe = await fetchElement(panelId);
    if (probe.ok) return { ok: true, error: null };
    const click = await clickElement(triggerId);
    if (!click.ok) return { ok: false, error: `click ${triggerId}: ${click.error}` };
    await sleep(PANEL_SETTLE_MS);
  }
  const probe = await fetchElement(panelId);
  return probe.ok
    ? { ok: true, error: null }
    : { ok: false, error: `${panelId} did not appear after two clicks on ${triggerId}` };
}

async function runDropdownPass(ctx, phase, ids) {
  const { opts } = ctx;
  const label = phase === "post-restore" ? " (post-restore)" : "";

  const info = await fetchSessionsInfo();
  if (!info.ok) {
    const why = `GET /control/sessions/info: ${info.error}`;
    skip(ids.t3, `the panel opens and renders every required field${label}`, why);
    skip(ids.t4, `rendered raw value == backend raw value, field by field${label}`, why);
    skip(ids.t5, `account and name are correct and distinct across sessions${label}`, why);
    return { info: null };
  }
  if (info.status !== "ok") {
    const why = `/control/sessions/info reported status='${info.status}' reason='${info.reason}'`;
    skip(ids.t3, `the panel opens and renders every required field${label}`, why);
    skip(ids.t4, `rendered raw value == backend raw value, field by field${label}`, why);
    skip(ids.t5, `account and name are correct and distinct across sessions${label}`, why);
    return { info };
  }

  const z = await collectZones();
  const bound = (z.zones || []).filter((x) => x.claudeSessionId);
  if (!z.ok) {
    const why = `GET /ui-bridge/control/elements: ${z.error}`;
    skip(ids.t3, `the panel opens and renders every required field${label}`, why);
    skip(ids.t4, `rendered raw value == backend raw value, field by field${label}`, why);
    skip(ids.t5, `account and name are correct and distinct across sessions${label}`, why);
    return { info };
  }

  // --- T3: open each panel, then require the field rows -------------------
  if (opts.readOnly) {
    const why = "--read-only forbids the trigger click that opens the panel";
    skip(ids.t3, `the panel opens and renders every required field${label}`, why);
    skip(ids.t4, `rendered raw value == backend raw value, field by field${label}`, why);
    skip(ids.t5, `account and name are correct and distinct across sessions${label}`, why);
    return { info };
  }

  const t3 = [];
  const opened = [];
  for (const zone of bound) {
    const triggerId = infoElementId("trigger", zone.zoneIndex);
    const click = await clickElement(triggerId);
    if (!click.ok) {
      t3.push({
        ok: false,
        detail: `zone ${zone.zoneIndex}: click ${triggerId} failed — ${click.error}`,
      });
      continue;
    }
    await sleep(PANEL_SETTLE_MS);
    const reg = await fetchElements();
    if (!reg.ok) {
      t3.push({
        ok: false,
        detail: `zone ${zone.zoneIndex}: elements re-read failed — ${reg.error}`,
      });
      continue;
    }
    const present = new Set(reg.elements.map((e) => String(e?.id ?? "")));
    const missing = [
      infoElementId("panel", zone.zoneIndex),
      ...T3_REQUIRED_FIELDS.map((f) => infoElementId(f, zone.zoneIndex)),
    ].filter((id) => !present.has(id));
    if (missing.length) {
      t3.push({ ok: false, detail: `zone ${zone.zoneIndex}: missing ${missing.join(", ")}` });
    } else {
      t3.push({ ok: true, detail: "" });
      opened.push(zone);
    }
  }
  assertAll(ids.t3, `the panel opens and renders every required field${label}`, t3);

  // --- T4: raw against raw, field by field --------------------------------
  const t4 = [];
  for (const zone of opened) {
    const session = info.sessions.find(
      (s) => s.identity?.claudeSessionId === zone.claudeSessionId && s.available,
    );
    if (!session) {
      t4.push({
        ok: false,
        detail: `zone ${zone.zoneIndex}: header claims session ${zone.claudeSessionId} but /control/sessions/info has no available projection for it`,
      });
      continue;
    }
    // Re-open: T3 left only the LAST zone's panel up (see `ensurePanelOpen`).
    const reopened = await ensurePanelOpen(zone.zoneIndex);
    if (!reopened.ok) {
      t4.push({ ok: false, detail: `zone ${zone.zoneIndex}: ${reopened.error}` });
      continue;
    }
    for (const field of INFO_FIELDS) {
      const id = infoElementId(field, zone.zoneIndex);
      const el = await fetchElement(id);
      if (!el.ok) {
        t4.push({ ok: false, detail: `zone ${zone.zoneIndex}: GET element ${id} — ${el.error}` });
        continue;
      }
      t4.push(
        compareRow(
          el.element,
          field,
          backendValueForField(session, field),
          `zone ${zone.zoneIndex} ${field}`,
        ),
      );
    }
  }
  assertAll(ids.t4, `rendered raw value == backend raw value, field by field${label}`, t4);

  // --- T5: account + name across sessions ---------------------------------
  const projections = opened
    .map((zone) => info.sessions.find((s) => s.identity?.claudeSessionId === zone.claudeSessionId))
    .filter(Boolean);
  const t5 = [];
  if (projections.length < 2) {
    skip(
      ids.t5,
      `account and name are correct and distinct across sessions${label}`,
      `only ${projections.length} panel(s) could be opened — the cross-session distinctness claim needs at least two`,
    );
  } else {
    const labels = projections.map((p) => p.account?.label ?? null);
    const distinct = new Set(labels.filter(Boolean));
    if (distinct.size < 2) {
      // Distinguish "the dropdown got it wrong" from "this machine cannot pose
      // the question". With a single-account roster every fixture is on the SAME
      // account BY CONSTRUCTION, so a FAIL here would be an accusation the
      // evidence does not support. Report it as an unexercised arm instead --
      // which still costs coverage, and still says so out loud.
      if (ctx.singleAccount) {
        note(
          `T5 account-distinctness NOT EXERCISED: all fixtures share the single ` +
            `available account (${labels.join(", ")}). The dropdown is proven to RENDER an ` +
            `account, never to DISTINGUISH two. Add a second config dir to ` +
            `claude_config_dirs to close this hole.`,
        );
      } else {
        t5.push({
          ok: false,
          detail: `all opened sessions report the same account label(s): ${labels.join(", ")} — session A's account must differ from session B's`,
        });
      }
    }
    const sources = projections.map((p) => p.name?.source ?? "unknown");
    if (!sources.includes("operator")) {
      t5.push({
        ok: false,
        detail: `no opened session reports name.source='operator' (saw: ${sources.join(", ")})`,
      });
    }
    if (!sources.includes("derived")) {
      t5.push({
        ok: false,
        detail: `no opened session reports name.source='derived' (saw: ${sources.join(", ")})`,
      });
    }
    if (t5.length === 0) t5.push({ ok: true, detail: "" });
    assertAll(ids.t5, `account and name are correct and distinct across sessions${label}`, t5);
  }
  return { info };
}

/**
 * Wait until every session-bound zone has its info trigger registered.
 *
 * The trigger mounts only once the FRONTEND tab carries a `claudeSessionId`,
 * and for a backend-created terminal that arrives on the periodic
 * `reconcileClaudeSessionIds` backfill -- a 30s interval. The suite can finish
 * building fixtures well inside one tick, so sampling immediately catches the
 * UI mid-convergence: one run had all three triggers, the next had only zone 0
 * and failed T2 for two zones that were merely not there YET (2026-08-20).
 *
 * T2 claims the trigger renders, which is a steady-state property, so waiting
 * for convergence is the honest probe. This does NOT paper over a real absence:
 * when the budget expires the assertions run against whatever is there and T2
 * fails exactly as before, having given the UI more than one backfill tick.
 */
async function waitForTriggersToMount(budgetMs = TRIGGER_MOUNT_BUDGET_MS) {
  const deadline = Date.now() + budgetMs;
  while (Date.now() < deadline) {
    const z = await collectZones();
    const bound = (z.zones || []).filter((x) => x.claudeSessionId);
    if (bound.length > 0) {
      const reg = await fetchElements();
      if (reg.ok) {
        const present = new Set(reg.elements.map((e) => String(e?.id ?? "")));
        const missing = bound.filter((x) => !present.has(infoElementId("trigger", x.zoneIndex)));
        if (missing.length === 0) return;
      }
    }
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
}

async function runDropdownAssertions(ctx) {
  section("T1-T5 — dropdown");

  await waitForTriggersToMount();
  const info = await fetchSessionsInfo();
  const z = await collectZones();

  // --- T1 --------------------------------------------------------------
  if (!z.ok) {
    fail(
      "T1",
      "each zone header is discoverable and bound to the expected session",
      `GET /ui-bridge/control/elements: ${z.error}`,
    );
  } else if (z.zones.length === 0) {
    skip(
      "T1",
      "each zone header is discoverable and bound to the expected session",
      "the UI Bridge registry contains no terminal-zone-header-<n> element",
    );
  } else if (!info.ok || info.status !== "ok") {
    fail(
      "T1",
      "each zone header is discoverable and bound to the expected session",
      info.ok
        ? `/control/sessions/info status='${info.status}' reason='${info.reason}' — cannot confirm the binding`
        : `GET /control/sessions/info: ${info.error}`,
    );
  } else {
    const findings = [];
    for (const zone of z.zones) {
      // By SELECTOR, not by `text`: `text` matches VISIBLE text, and a zone
      // header's text is its label ("Zone 2: claude (85590fb4)"), never its id.
      // Asking for the id as text matched nothing and read as "the header is
      // not discoverable" while it was registered and clickable all along.
      const found = await findElements({
        selector: `[data-ui-bridge-id="${zone.headerId}"]`,
      });
      const hit = found.ok && found.elements.some((e) => String(e?.id ?? "") === zone.headerId);
      if (!hit) {
        findings.push({
          ok: false,
          detail: `POST /ui-bridge/control/find did not return ${zone.headerId}${found.ok ? "" : ` (${found.error})`}`,
        });
        continue;
      }
      if (!zone.claudeSessionId) {
        // A PTY tab with no Claude session is a legitimate state, not a defect.
        findings.push({ ok: true, detail: "" });
        continue;
      }
      const session = info.sessions.find(
        (s) => s.identity?.claudeSessionId === zone.claudeSessionId && s.available,
      );
      if (!session) {
        findings.push({
          ok: false,
          detail: `${zone.headerId} claims data-claude-session-id=${zone.claudeSessionId}, absent from /control/sessions/info`,
        });
        continue;
      }
      if (String(session.placement?.zoneIndex) !== String(zone.zoneAttr ?? zone.zoneIndex)) {
        findings.push({
          ok: false,
          detail: `${zone.headerId}: header data-zone-index=${zone.zoneAttr} but the backend places ${zone.claudeSessionId} at zoneIndex ${session.placement?.zoneIndex}`,
        });
        continue;
      }
      if (
        ctx.fixtures &&
        !ctx.fixtures.some((f) => f.session.identity.claudeSessionId === zone.claudeSessionId)
      ) {
        findings.push({
          ok: false,
          detail: `${zone.headerId} is bound to ${zone.claudeSessionId}, which is not one of the fixtures this run created`,
        });
        continue;
      }
      findings.push({ ok: true, detail: "" });
    }
    assertAll("T1", "each zone header is discoverable and bound to the expected session", findings);
  }

  // --- T2 --------------------------------------------------------------
  const bound = (z.zones || []).filter((x) => x.claudeSessionId);
  if (!z.ok) {
    skip(
      "T2",
      "the info trigger exists for every session-bound zone and does NOT self-hide",
      `elements read failed: ${z.error}`,
    );
  } else if (bound.length === 0) {
    skip(
      "T2",
      "the info trigger exists for every session-bound zone and does NOT self-hide",
      "no zone header carries a data-claude-session-id",
    );
  } else {
    const present = new Set((z.registry || []).map((e) => String(e?.id ?? "")));
    const findings = bound.map((zone) => {
      const id = infoElementId("trigger", zone.zoneIndex);
      return present.has(id)
        ? { ok: true, detail: "" }
        : {
            ok: false,
            detail: `${id} is absent from the UI Bridge registry although zone ${zone.zoneIndex} is bound to session ${zone.claudeSessionId} — the trigger must render even when the backing read (or the PR ledger) is unavailable (G5)`,
          };
    });
    assertAll(
      "T2",
      "the info trigger exists for every session-bound zone and does NOT self-hide",
      findings,
    );
  }

  // --- T0.5 (PR arm honesty) + T3/T4/T5 -----------------------------------
  const withPrs = info.ok
    ? info.sessions.filter((s) => s.available && s.prs?.status === "ok" && s.prs.openCount > 0)
    : [];
  const degradedPrs = info.ok
    ? info.sessions.filter((s) => s.available && s.prs?.status && s.prs.status !== "ok")
    : [];
  if (!info.ok) {
    skip(
      "T0.5",
      "either a populated PR ledger OR the prs.status/reason path is exercised",
      `/control/sessions/info: ${info.error}`,
    );
  } else if (withPrs.length > 0) {
    pass(
      "T0.5",
      "either a populated PR ledger OR the prs.status/reason path is exercised",
      `populated-PR arm EXERCISED: ${withPrs.length} session(s) carry attributed PRs ` +
        `(${withPrs.map((s) => `${String(s.identity.claudeSessionId).slice(0, 8)}:${s.prs.openCount}op/${s.prs.landedCount}land/${s.prs.unknownCount}unk`).join(", ")})`,
    );
  } else if (degradedPrs.length > 0) {
    pass(
      "T0.5",
      "either a populated PR ledger OR the prs.status/reason path is exercised",
      `populated-PR arm NOT exercised (no session has an attributed PR). The degraded path WAS: ` +
        `${degradedPrs.length} session(s) report prs.status='${degradedPrs[0].prs.status}' reason='${degradedPrs[0].prs.reason}'. ` +
        `Reported, not omitted (plan §6 T0 step 5).`,
    );
  } else {
    skip(
      "T0.5",
      "either a populated PR ledger OR the prs.status/reason path is exercised",
      "no session reports either an attributed PR or a degraded prs.status — neither arm was reached",
    );
  }

  await runDropdownPass(ctx, "pre-restart", { t3: "T3", t4: "T4", t5: "T5" });
}

// ===========================================================================
// Restart machinery (T7 / T10) — gated, guarded, never a process-tree kill
// ===========================================================================

/** Run a PowerShell command, returning `{ok, stdout, stderr, code}`. */
function powershell(args, { timeoutMs = 600_000 } = {}) {
  const r = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", ...args], {
    encoding: "utf8",
    timeout: timeoutMs,
    windowsHide: true,
  });
  return {
    ok: r.status === 0,
    code: r.status,
    stdout: (r.stdout || "").trim(),
    stderr: (r.stderr || "").trim(),
    error: r.error ? String(r.error.message ?? r.error) : null,
  };
}

/** Poll `/health` until the runner answers, sized against the measured tail. */
async function waitForHealthy(budgetMs = HEALTH_POLL_BUDGET_MS) {
  const deadline = Date.now() + budgetMs;
  let last = "never probed";
  while (Date.now() < deadline) {
    const r = await http("/health", { timeoutMs: HEALTH_PROBE_TIMEOUT_MS });
    if (r.ok) return { ok: true, detail: `healthy after ${r.ms}ms on the final probe` };
    last = r.error;
    await sleep(HEALTH_POLL_INTERVAL_MS);
  }
  return { ok: false, detail: `no healthy /health within ${budgetMs}ms (last: ${last})` };
}

/**
 * Stop ONLY the runner process, by pid, by exact image name.
 *
 * Never `node.exe`, never `powershell.exe`, and never a process-tree (`/T`)
 * kill — all three terminate live Claude Code sessions, including the caller's
 * own, and none of them is inside the operator's carve-out. The refusal list is
 * checked at the point of the kill, not assumed from the query.
 */
function killRunnerProcessOnly() {
  const list = powershell([
    "-Command",
    `Get-Process -Name '${RUNNER_PROCESS_NAME}' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id`,
  ]);
  const pids = (list.stdout || "")
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter((s) => /^\d+$/.test(s));
  if (pids.length === 0) {
    return {
      ok: false,
      detail: `no process named '${RUNNER_PROCESS_NAME}' is running — nothing to crash`,
    };
  }
  const killed = [];
  for (const pid of pids) {
    const nameProbe = powershell([
      "-Command",
      `(Get-Process -Id ${pid} -ErrorAction SilentlyContinue).ProcessName`,
    ]);
    const name = (nameProbe.stdout || "").trim().toLowerCase();
    if (NEVER_KILL.includes(name) || name !== RUNNER_PROCESS_NAME.toLowerCase()) {
      return {
        ok: false,
        detail: `REFUSED: pid ${pid} resolves to '${name}', not '${RUNNER_PROCESS_NAME}'. This script never kills node/powershell and never uses a process-tree flag.`,
      };
    }
    // Single pid, no -T / tree semantics anywhere.
    const k = powershell(["-Command", `Stop-Process -Id ${pid} -Force -Confirm:$false`]);
    if (!k.ok)
      return { ok: false, detail: `Stop-Process -Id ${pid} failed: ${k.stderr || k.error}` };
    killed.push(pid);
  }
  return {
    ok: true,
    detail: `stopped runner pid(s) ${killed.join(", ")} (no tree flag, no node/powershell touched)`,
  };
}

// ===========================================================================
// T6-T9 — restore
// ===========================================================================

/** Skip the whole post-restore dropdown re-run with one stated reason. */
function skipT9Pass(why) {
  skip("T9.T3", "the panel opens and renders every required field (post-restore)", why);
  skip("T9.T4", "rendered raw value == backend raw value, field by field (post-restore)", why);
  skip("T9.T5", "account and name are correct and distinct across sessions (post-restore)", why);
}

async function runRestoreAssertions(ctx) {
  section("T6-T9 — restore");
  const { opts } = ctx;

  // --- T6: the pre-restart census -----------------------------------------
  const c = await fetchCensus();
  if (!c.ok) {
    fail(
      "T6",
      "the pre-restart census is complete, and is saved as the oracle",
      `GET /control/sessions/restore-census: ${c.error}`,
    );
  } else {
    const census = c.census ?? {};
    const expected = Array.isArray(census.expected) ? census.expected : [];
    const problems = [];
    if (census.status !== "ok") {
      problems.push(`census status='${census.status}' reason='${census.reason ?? "<none>"}'`);
    }
    // The ORACLE is the set that must SURVIVE the coming restart — the sessions
    // open right now. It is deliberately NOT the census's `expected`.
    //
    // `expected` is latched ONCE AT BOOT from the shutdown-time set, before any
    // restore, reconcile or liveness tick can touch a record (see
    // `session/restore_census.rs`: sourcing it from the post-restart registry
    // instead would make the census self-referential and pin every verdict to
    // `match`). A session created AFTER boot — which every fixture is — can
    // therefore never appear in it. Requiring that it did was a category error
    // that failed T6 against a census behaving exactly as designed (measured
    // 2026-08-20: all three fixtures reported as "omitted" from `expected[]`).
    //
    // So pre-restart T6 checks what it honestly can: the census answers, it is
    // internally coherent, and the live set handed to T8 is non-empty.
    const live = await fetchSessionsInfo();
    let oracleExpected = [];
    if (!live.ok) {
      problems.push(`GET /control/sessions/info (oracle source): ${live.error}`);
    } else if (live.status !== "ok") {
      problems.push(`oracle source degraded: status='${live.status}' reason='${live.reason}'`);
    } else {
      oracleExpected = live.sessions
        .filter((x) => x.identity?.claudeSessionId)
        .map((x) => ({
          claudeSessionId: x.identity.claudeSessionId,
          terminalId: x.identity.terminalId ?? null,
          zoneIndex: x.placement?.zoneIndex ?? null,
          pageId: x.placement?.pageId ?? null,
        }));
      if (oracleExpected.length === 0) {
        problems.push(
          "no session is open, so there is nothing for a restart to bring back — this is a genuine " +
            "UNKNOWN, not a completed census",
        );
      }
      if (ctx.fixtures) {
        const oracleIds = new Set(oracleExpected.map((e) => e.claudeSessionId));
        const absent = ctx.fixtures
          .map((f) => f.session.identity.claudeSessionId)
          .filter((id) => !oracleIds.has(id));
        if (absent.length) {
          problems.push(`fixture session(s) missing from the live open set: ${absent.join(", ")}`);
        }
      }
    }
    if (census.verdict === undefined || census.verdict === null) {
      problems.push("census carries no verdict");
    }
    if (problems.length === 0) {
      try {
        mkdirSync(dirname(opts.oracle), { recursive: true });
        const oracle = { expected: oracleExpected, census };
        writeFileSync(opts.oracle, JSON.stringify({ savedAt: Date.now(), ...oracle }, null, 2));
        ctx.oracle = oracle;
        pass(
          "T6",
          "the pre-restart census is complete, and is saved as the oracle",
          `oracle=${oracleExpected.length} live session(s); census expected=${expected.length} ` +
            `(boot-latched), cleanShutdown=${census.cleanShutdown}, verdict=${census.verdict}; ` +
            `oracle -> ${opts.oracle}`,
        );
      } catch (e) {
        fail(
          "T6",
          "the pre-restart census is complete, and is saved as the oracle",
          `could not write the oracle to ${opts.oracle}: ${e?.message ?? e}`,
        );
      }
    } else {
      fail(
        "T6",
        "the pre-restart census is complete, and is saved as the oracle",
        problems.join("\n"),
      );
    }
  }

  // --- T7: the restart -----------------------------------------------------
  if (!opts.allowRestart) {
    const why =
      "--allow-restart is OFF by default. The plan's §0 restart authorization is conditional on an empty " +
      "runner the implementer itself emptied, and §0.1 measured both premises FALSE; the default-off flag is " +
      "that condition made mechanical.";
    skip("T7", "the runner restarts cleanly (clean shutdown marker, then healthy)", why);
    skip(
      "T8",
      "the correct sessions were restored — verdict match, oracle set equality, placement held",
      why + " T7 never ran, so there is no post-restart state.",
    );
    skipT9Pass(why + " T7 never ran.");
    skip(
      "T9.tier",
      "restore-tier is populated and honest ('resumed' implies transcriptExists)",
      why + " T7 never ran.",
    );
    return;
  }

  const gate = await gatesAllowMutation("T7 pre-restart");
  if (!gate.ok) {
    fail(
      "T7",
      "the runner restarts cleanly (clean shutdown marker, then healthy)",
      `ABORTED — ${gate.detail}`,
    );
    skip(
      "T8",
      "the correct sessions were restored — verdict match, oracle set equality, placement held",
      "T7 aborted at its gate re-check",
    );
    skipT9Pass("T7 aborted at its gate re-check");
    skip(
      "T9.tier",
      "restore-tier is populated and honest ('resumed' implies transcriptExists)",
      "T7 aborted at its gate re-check",
    );
    return;
  }

  // `dev-start.ps1 -StopRunner/-Runner` manages the PRIMARY runner. If this run
  // is pointed at any other port, restarting through it would stop a runner the
  // suite is not testing and leave the one it IS testing untouched — then poll
  // the untouched runner back to "healthy" and report a restart that never
  // happened. Refuse instead, and say which two things disagree.
  if (opts.port !== DEFAULT_RUNNER_PORT && !opts.devStartExplicit) {
    fail(
      "T7",
      "the runner restarts cleanly (clean shutdown marker, then healthy)",
      `REFUSED: --port ${opts.port} is not the port ${opts.devStart} manages (${DEFAULT_RUNNER_PORT}), ` +
        `so restarting through it would target a different runner. Pass --dev-start pointing at a ` +
        `script that stops and starts THIS instance.`,
    );
    skip(
      "T8",
      "the correct sessions were restored — verdict match, oracle set equality, placement held",
      "T7 refused: the restart door manages a different runner than --port",
    );
    skipT9Pass("T7 refused: the restart door manages a different runner than --port");
    skip(
      "T9.tier",
      "restore-tier is populated and honest ('resumed' implies transcriptExists)",
      "T7 refused: the restart door manages a different runner than --port",
    );
    return;
  }

  const stop = powershell(["-File", opts.devStart, "-StopRunner"]);
  if (!stop.ok) {
    fail(
      "T7",
      "the runner restarts cleanly (clean shutdown marker, then healthy)",
      `dev-start.ps1 -StopRunner failed (${stop.code}): ${stop.stderr || stop.error}`,
    );
    skip(
      "T8",
      "the correct sessions were restored — verdict match, oracle set equality, placement held",
      "the restart did not complete",
    );
    skipT9Pass("the restart did not complete");
    skip(
      "T9.tier",
      "restore-tier is populated and honest ('resumed' implies transcriptExists)",
      "the restart did not complete",
    );
    return;
  }
  const start = powershell(["-File", opts.devStart, "-Runner"]);
  const healthy = await waitForHealthy();
  if (!healthy.ok) {
    fail(
      "T7",
      "the runner restarts cleanly (clean shutdown marker, then healthy)",
      `${healthy.detail}\n-Runner exit ${start.code}: ${start.stderr || "(no stderr)"}`,
    );
    skip(
      "T8",
      "the correct sessions were restored — verdict match, oracle set equality, placement held",
      "the runner never came back healthy",
    );
    skipT9Pass("the runner never came back healthy");
    skip(
      "T9.tier",
      "restore-tier is populated and honest ('resumed' implies transcriptExists)",
      "the runner never came back healthy",
    );
    return;
  }
  // "A clean shutdown marker was written" is asserted through the census's own
  // report of the boundary it read, rather than by guessing the marker's path.
  const post = await fetchCensus();
  const clean = post.ok && post.census?.cleanShutdown === true && post.census?.shutdownAt != null;
  if (clean) {
    pass(
      "T7",
      "the runner restarts cleanly (clean shutdown marker, then healthy)",
      `${healthy.detail}; cleanShutdown=true, shutdownAt=${post.census.shutdownAt}`,
    );
  } else {
    fail(
      "T7",
      "the runner restarts cleanly (clean shutdown marker, then healthy)",
      post.ok
        ? `the runner is healthy but the census reports cleanShutdown=${post.census?.cleanShutdown} shutdownAt=${post.census?.shutdownAt} reason=${post.census?.reason} — no clean shutdown marker was written`
        : `census unreadable after restart: ${post.error}`,
    );
  }

  // --- T8: the core claim --------------------------------------------------
  if (!ctx.oracle) {
    skip(
      "T8",
      "the correct sessions were restored — verdict match, oracle set equality, placement held",
      "T6 never saved an oracle",
    );
  } else if (!post.ok) {
    fail(
      "T8",
      "the correct sessions were restored — verdict match, oracle set equality, placement held",
      `GET /control/sessions/restore-census: ${post.error}`,
    );
  } else {
    const census = post.census ?? {};
    const problems = [];
    if (census.verdict !== "match") {
      problems.push(
        `verdict='${census.verdict}' (reason='${census.reason ?? "<none>"}'), expected 'match'`,
      );
    }
    const missing = Array.isArray(census.missing) ? census.missing : [];
    const unexpected = Array.isArray(census.unexpected) ? census.unexpected : [];
    if (missing.length)
      problems.push(
        `missing[]: ${missing.map((m) => `${m.claudeSessionId}(${m.reason})`).join(", ")}`,
      );
    if (unexpected.length) {
      problems.push(
        `unexpected[]: ${unexpected.map((u) => `${u.claudeSessionId}(${u.reason})`).join(", ")}`,
      );
    }
    const oracleIds = new Set((ctx.oracle.expected || []).map((e) => e.claudeSessionId));
    const restoredIds = new Set((census.restored || []).map((r) => r.claudeSessionId));
    const notBack = [...oracleIds].filter((id) => !restoredIds.has(id));
    const surprise = [...restoredIds].filter((id) => !oracleIds.has(id));
    if (notBack.length)
      problems.push(`in the oracle's expected set but not restored: ${notBack.join(", ")}`);
    if (surprise.length)
      problems.push(`restored but absent from the oracle's expected set: ${surprise.join(", ")}`);
    // Placement: zoneIndex AND pageId must survive the restart.
    const byId = new Map((ctx.oracle.expected || []).map((e) => [e.claudeSessionId, e]));
    const info = await fetchSessionsInfo();
    for (const r of census.restored || []) {
      const was = byId.get(r.claudeSessionId);
      if (!was) continue;
      if (Number(r.zoneIndex) !== Number(was.zoneIndex)) {
        problems.push(`${r.claudeSessionId}: zoneIndex ${was.zoneIndex} -> ${r.zoneIndex}`);
      }
      const now = info.ok
        ? info.sessions.find((s) => s.identity?.claudeSessionId === r.claudeSessionId)
        : null;
      if (now && now.placement?.pageId !== was.pageId) {
        problems.push(`${r.claudeSessionId}: pageId '${was.pageId}' -> '${now.placement?.pageId}'`);
      }
    }
    if (problems.length === 0) {
      pass(
        "T8",
        "the correct sessions were restored — verdict match, oracle set equality, placement held",
        `verdict=match; ${restoredIds.size} session(s) restored, set-equal to the oracle's expected set; placement unchanged`,
      );
    } else {
      fail(
        "T8",
        "the correct sessions were restored — verdict match, oracle set equality, placement held",
        problems.join("\n"),
      );
    }
  }

  // --- T9: the dropdown still tells the truth ------------------------------
  await runDropdownPass(ctx, "post-restore", { t3: "T9.T3", t4: "T9.T4", t5: "T9.T5" });

  const info2 = await fetchSessionsInfo();
  const rh = await fetchRestoreHealth();
  if (!info2.ok || info2.status !== "ok" || !rh.ok) {
    skip(
      "T9.tier",
      "restore-tier is populated and honest ('resumed' implies transcriptExists)",
      info2.ok ? `restore-health: ${rh.error ?? "unreadable"}` : `sessions/info: ${info2.error}`,
    );
  } else {
    const restored = info2.sessions.filter(
      (s) => s.available && s.lifecycle?.restoredFromBootAt != null,
    );
    if (restored.length === 0) {
      skip(
        "T9.tier",
        "restore-tier is populated and honest ('resumed' implies transcriptExists)",
        "no session carries a restoredFromBootAt stamp after the restart",
      );
    } else {
      const findings = restored.map((s) => {
        const id = s.identity.claudeSessionId;
        const tier = s.lifecycle.restoreTier;
        const h = rh.sessions.find((x) => x.claudeSessionId === id);
        if (!tier)
          return {
            ok: false,
            detail: `${id}: restoredFromBootAt is set but restoreTier is null — the tier must be populated`,
          };
        if (tier === "resumed" && h && h.transcriptExists !== true) {
          return {
            ok: false,
            detail: `${id}: claims restoreTier='resumed' but restore-health reports transcriptExists=false`,
          };
        }
        if (tier === "terminal-only" && s.lifecycle.restoreTier === "resumed") {
          return { ok: false, detail: `${id}: a terminal-only restore must not claim 'resumed'` };
        }
        return { ok: true, detail: "" };
      });
      assertAll(
        "T9.tier",
        "restore-tier is populated and honest ('resumed' implies transcriptExists)",
        findings,
      );
    }
  }
}

// ===========================================================================
// T10-T11b — negative / honesty
// ===========================================================================

/** `owner/repo#123` → `{repo, prNumber}`; `null` when the spelling is not that. */
function parsePrRef(ref) {
  const m = /^(.+)#(\d+)$/.exec(String(ref || "").trim());
  return m ? { repo: m[1], prNumber: Number(m[2]) } : null;
}

/** Flatten every session's PR ledger into one list of rows with their owner. */
function allPrRows(info) {
  const out = { landed: [], unknown: [], opened: [], degraded: [] };
  for (const s of info.sessions || []) {
    if (!s.available || !s.prs) continue;
    const sid = s.identity?.claudeSessionId;
    if (s.prs.status !== "ok") {
      out.degraded.push({ sid, reason: s.prs.reason });
      continue;
    }
    for (const p of s.prs.landed || []) out.landed.push({ sid, ...p });
    for (const p of s.prs.unknown || []) out.unknown.push({ sid, ...p });
    for (const p of s.prs.opened || []) out.opened.push({ sid, ...p });
  }
  return out;
}

/**
 * T11's precondition (D3's guard): the head object and the base ref must both
 * exist locally, or the ancestor test cannot be evaluated at all and the test
 * would fail for the WRONG reason — reading as a G3 regression when it is
 * really a stale local ref. `git fetch` is only run behind an explicit flag,
 * because fetching is a network op against the operator's own checkout.
 */
function checkHeadObjectPresent(repoDir, base, allowFetch) {
  if (!repoDir)
    return {
      evaluable: false,
      detail: "--repo-dir not given, so the head-object precondition could not be checked",
    };
  if (allowFetch) {
    const f = powershell(["-Command", `git -C '${repoDir}' fetch --quiet origin ${base}`], {
      timeoutMs: 180_000,
    });
    if (!f.ok)
      return {
        evaluable: false,
        detail: `git fetch origin ${base} failed: ${f.stderr || f.error}`,
      };
  }
  const baseRef = powershell([
    "-Command",
    `git -C '${repoDir}' rev-parse --verify --quiet 'origin/${base}^{commit}'`,
  ]);
  if (!baseRef.stdout) {
    return {
      evaluable: false,
      detail: `origin/${base} does not resolve in ${repoDir} (no_base_ref)`,
    };
  }
  return {
    evaluable: true,
    detail: `origin/${base} resolves to ${baseRef.stdout.slice(0, 12)} in ${repoDir}${allowFetch ? " (freshly fetched)" : " (NOT fetched — --allow-git-fetch was off, so the ref may be stale)"}`,
  };
}

async function runHonestyAssertions(ctx) {
  section("T10-T11b — negative / honesty");
  const { opts } = ctx;

  // --- T10: a hard-crash boot reports unknown, never match -----------------
  if (!opts.allowRestart) {
    skip(
      "T10",
      "a hard-crash boot reports verdict 'unknown', not 'match'",
      "--allow-restart is OFF by default. T10 crashes the runner (runner process only, by pid, no tree flag) " +
        "and that is inside the plan's §0 carve-out ONLY for an empty runner the implementer itself emptied.",
    );
  } else {
    const gate = await gatesAllowMutation("T10 pre-crash");
    if (!gate.ok) {
      fail(
        "T10",
        "a hard-crash boot reports verdict 'unknown', not 'match'",
        `ABORTED — ${gate.detail}`,
      );
    } else {
      const k = killRunnerProcessOnly();
      if (!k.ok) {
        fail("T10", "a hard-crash boot reports verdict 'unknown', not 'match'", k.detail);
      } else {
        const start = powershell(["-File", opts.devStart, "-Runner"]);
        const healthy = await waitForHealthy();
        if (!healthy.ok) {
          fail(
            "T10",
            "a hard-crash boot reports verdict 'unknown', not 'match'",
            `${k.detail}\n${healthy.detail}\n-Runner exit ${start.code}: ${start.stderr || "(no stderr)"}`,
          );
        } else {
          const c = await fetchCensus();
          if (!c.ok) {
            fail(
              "T10",
              "a hard-crash boot reports verdict 'unknown', not 'match'",
              `census unreadable after the crash boot: ${c.error}`,
            );
          } else if (c.census?.verdict === "unknown" && c.census?.reason) {
            pass(
              "T10",
              "a hard-crash boot reports verdict 'unknown', not 'match'",
              `${k.detail}; verdict='unknown' reason='${c.census.reason}' cleanShutdown=${c.census.cleanShutdown}`,
            );
          } else {
            fail(
              "T10",
              "a hard-crash boot reports verdict 'unknown', not 'match'",
              `after a kill that wrote no shutdown marker the census reports verdict='${c.census?.verdict}' reason='${c.census?.reason ?? "<none>"}' — ` +
                `a crash boot has no trustworthy pre-restart set and must never claim an outcome`,
            );
          }
        }
      }
    }
  }

  // --- T11 / T11b: the land-signal honesty pair ---------------------------
  const info = await fetchSessionsInfo();
  if (!info.ok) {
    const why = `GET /control/sessions/info: ${info.error}`;
    skip("T11", "an ff-landed PR is reported LANDED with landSignal 'ff-land'", why);
    skip(
      "T11b",
      "an unevaluable land signal reports 'land-unknown' WITH a reason, never a confident not-landed",
      why,
    );
    return;
  }
  if (info.status !== "ok") {
    const why = `/control/sessions/info reported status='${info.status}' reason='${info.reason}'`;
    skip("T11", "an ff-landed PR is reported LANDED with landSignal 'ff-land'", why);
    skip(
      "T11b",
      "an unevaluable land signal reports 'land-unknown' WITH a reason, never a confident not-landed",
      why,
    );
    return;
  }

  const rows = allPrRows(info);

  // T11 — the G3 guard.
  const target = parsePrRef(opts.ffPr);
  const ffCandidates = target
    ? rows.landed.filter((p) => p.repo === target.repo && p.prNumber === target.prNumber)
    : rows.landed.filter((p) => p.landSignal === "ff-land");
  if (target && ffCandidates.length === 0) {
    const anywhere = [...rows.opened, ...rows.unknown].filter(
      (p) => p.repo === target.repo && p.prNumber === target.prNumber,
    );
    fail(
      "T11",
      "an ff-landed PR is reported LANDED with landSignal 'ff-land'",
      anywhere.length
        ? `${opts.ffPr} is in the ledger but NOT in prs.landed — an ff-land must render as landed (G3)`
        : `${opts.ffPr} is not attributed to any session in this runner's ledger, so the ff-land claim could not be evaluated against it`,
    );
  } else if (ffCandidates.length === 0) {
    skip(
      "T11",
      "an ff-landed PR is reported LANDED with landSignal 'ff-land'",
      rows.degraded.length
        ? `no session's ledger carries an ff-landed PR (and ${rows.degraded.length} session(s) report prs.status unavailable: ${rows.degraded[0].reason}). Pass --ff-pr owner/repo#N to target a known ff-land.`
        : "no session's ledger carries a PR with landSignal 'ff-land'. Pass --ff-pr owner/repo#N to target a known ff-land.",
    );
  } else {
    const pre = checkHeadObjectPresent(opts.repoDir, "main", opts.allowGitFetch);
    const bad = ffCandidates.filter((p) => p.landSignal !== "ff-land");
    if (bad.length) {
      fail(
        "T11",
        "an ff-landed PR is reported LANDED with landSignal 'ff-land'",
        `${bad.map((p) => `${p.repo}#${p.prNumber} landSignal='${p.landSignal ?? "<null>"}'`).join(", ")}\nprecondition: ${pre.detail}`,
      );
    } else {
      pass(
        "T11",
        "an ff-landed PR is reported LANDED with landSignal 'ff-land'",
        `${ffCandidates.map((p) => `${p.repo}#${p.prNumber} (landedAt ${p.landedAt ?? "?"})`).join(", ")}\nprecondition: ${pre.detail}`,
      );
    }
  }

  // T11b — the R1 sharpening: a negative the code never established is worse
  // than no answer at all.
  const utarget = parsePrRef(opts.unknownPr);
  const unknowns = utarget
    ? rows.unknown.filter((p) => p.repo === utarget.repo && p.prNumber === utarget.prNumber)
    : rows.unknown;
  if (utarget && unknowns.length === 0) {
    fail(
      "T11b",
      "an unevaluable land signal reports 'land-unknown' WITH a reason, never a confident not-landed",
      `${opts.unknownPr} is not in any session's prs.unknown bucket — an unevaluable verdict must land there, not be demoted to a confident not-landed`,
    );
  } else if (unknowns.length === 0) {
    skip(
      "T11b",
      "an unevaluable land signal reports 'land-unknown' WITH a reason, never a confident not-landed",
      "no session's ledger carries a row whose land verdict could not be evaluated. Pass --unknown-pr owner/repo#N " +
        "(a repo whose origin/<base> ref is absent, or whose PR head object was never fetched) to target one.",
    );
  } else {
    const landedKeys = new Set(rows.landed.map((p) => `${p.repo}#${p.prNumber}`));
    const findings = unknowns.map((p) => {
      const key = `${p.repo}#${p.prNumber}`;
      if (!p.reason || String(p.reason).trim() === "") {
        return { ok: false, detail: `${key}: in the unknown bucket but carries NO reason` };
      }
      if (landedKeys.has(key)) {
        return {
          ok: false,
          detail: `${key}: appears in BOTH prs.landed and prs.unknown — the verdict cannot be both proved and unevaluable`,
        };
      }
      return { ok: true, detail: "" };
    });
    // And the counts must agree, so an unknown row is never silently rolled
    // into a confident not-landed total.
    for (const s of info.sessions) {
      if (!s.available || s.prs?.status !== "ok") continue;
      if ((s.prs.unknown || []).length !== s.prs.unknownCount) {
        findings.push({
          ok: false,
          detail: `${s.identity.claudeSessionId}: unknownCount=${s.prs.unknownCount} but unknown[] has ${(s.prs.unknown || []).length} row(s)`,
        });
      }
    }
    assertAll(
      "T11b",
      "an unevaluable land signal reports 'land-unknown' WITH a reason, never a confident not-landed",
      findings,
    );
  }
}

// ===========================================================================
// Reporting
// ===========================================================================

function report(opts) {
  const passed = results.filter((r) => r.status === PASS);
  const failed = results.filter((r) => r.status === FAIL);
  const skipped = results.filter((r) => r.status === SKIP);

  console.log(`\n${"=".repeat(72)}`);
  console.log("ASSERTION TABLE");
  console.log("=".repeat(72));
  const idw = Math.max(9, ...results.map((r) => r.id.length));
  for (const r of results) {
    const mark = r.status === PASS ? "+" : r.status === FAIL ? "X" : "-";
    console.log(`  ${r.status} ${mark}  ${r.id.padEnd(idw)}  ${r.what}`);
  }
  console.log("=".repeat(72));
  console.log(
    `SUMMARY: ${passed.length} PASS (evaluated, held) | ${failed.length} FAIL ` +
      `(evaluated, did not hold) | ${skipped.length} SKIP (NOT evaluated)`,
  );
  if (skipped.length > 0) {
    console.log(
      `COVERAGE INCOMPLETE: ${skipped.length} assertion(s) were NOT exercised — ` +
        `${skipped.map((r) => r.id).join(", ")}`,
    );
    console.log("  An unexercised arm is reported, never omitted, and never counted as a pass.");
  } else {
    console.log("COVERAGE COMPLETE: every assertion in the table was evaluated.");
  }

  if (opts.json) {
    try {
      mkdirSync(dirname(opts.json), { recursive: true });
      writeFileSync(opts.json, JSON.stringify({ generatedAt: Date.now(), results }, null, 2));
      console.log(`Result table written to ${opts.json}`);
    } catch (e) {
      console.log(`WARNING: could not write --json ${opts.json}: ${e?.message ?? e}`);
    }
  }

  if (failed.length > 0) return 1;
  if (skipped.length > 0 && opts.strict) {
    console.log("--strict: exiting 2 because coverage is incomplete.");
    return 2;
  }
  return 0;
}

// ===========================================================================
// Main
// ===========================================================================

async function main() {
  const { opts, errors } = parseArgs(process.argv.slice(2));
  if (opts.help) {
    console.log(HELP);
    return 0;
  }
  if (errors.length) {
    console.error(`Bad arguments:\n  ${errors.join("\n  ")}\n\n${HELP}`);
    return 3;
  }
  // NEVER `localhost` — the runner binds IPv4 loopback only and Windows tries
  // ::1 first, paying a doomed connect before the socket that answers.
  BASE = `http://127.0.0.1:${opts.port}`;

  console.log(`UI-Bridge session-info + restore-census suite`);
  console.log(`  target        ${BASE}  (literal 127.0.0.1 — never localhost)`);
  console.log(
    `  mode          ${opts.fixture ? "FIXTURE" : "no-fixture"}, ` +
      `${opts.allowRestart ? "RESTARTS ALLOWED" : "restarts OFF"}, ` +
      `${opts.readOnly ? "read-only (no clicks)" : "clicks allowed"}`,
  );
  console.log(
    `  timeouts      request ${REQUEST_TIMEOUT_MS}ms, health poll ${HEALTH_POLL_BUDGET_MS}ms`,
  );

  const health = await http("/health", { timeoutMs: HEALTH_PROBE_TIMEOUT_MS });
  if (!health.ok) {
    console.error(`\nFATAL: the runner did not answer GET ${BASE}/health — ${health.error}`);
    console.error("Nothing below could be evaluated, so nothing is reported as passing.");
    return 3;
  }
  const buildId = health.json?.buildId ?? health.json?.data?.buildId ?? "unknown";
  const behind = health.json?.data?.buildDrift?.commitsBehind;
  console.log(
    `  runner build  ${buildId}${behind === undefined ? "" : ` (${behind} commits behind origin/main)`}`,
  );
  console.log(`  /health       ${health.status} in ${health.ms}ms`);

  const ctx = { opts, gates: { empty: false, notSelfHosted: false }, oracle: null };

  await runT0(ctx);
  await runDropdownAssertions(ctx);
  await runRestoreAssertions(ctx);
  await runHonestyAssertions(ctx);

  return report(opts);
}

main()
  .then((code) => process.exit(code))
  .catch((err) => {
    console.error(`\nFATAL: the harness itself threw — ${err?.stack ?? err}`);
    process.exit(3);
  });
