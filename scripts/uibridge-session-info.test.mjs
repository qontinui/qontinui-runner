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
 * ## What a FIXTURE is (rewritten 2026-11-08: no script evaluation)
 *
 * A fixture is a REAL, terminal-bound provider session, built over the
 * runner's HTTP API and read-only UI Bridge component actions — never by
 * evaluating script in the webview, which the exe-mode CSP (`script-src
 * 'self'`) refuses (plan `2026-11-08-tenant-badge-parity-and-oracle-fixtures`
 * D5; the old route built 0/3 fixtures on every current build):
 *
 *   - `terminal-page` `get-zone-view-state` names the page the UI shows and
 *     its grid zone map (a build that serves neither is refused by name);
 *   - `POST /terminals {page_id, title, working_dir, account, initial_command:
 *     "claude"}` spawns it — the RUNNER pins the account (roster-validated) and
 *     its identity seam writes the one spawn-time record; a spawn refused by
 *     the runner's admission guard for load is retried with bounded backoff;
 *   - the session id is read from `/control/sessions/restore-health` by
 *     terminal id, and the rendered zone from the grid map.
 *
 * A supervisor temp runner starts with an EMPTY account roster: the run then
 * refuses naming the missing accounts, unless `--seed-roster` names config
 * dirs to add for the run (restored at teardown, verified by read-back).
 *
 * Three properties are NOT this script's to manufacture:
 *
 *   - `confirmed_at` is written only by the provider's SessionStart hook
 *     (`POST /control/session-open`), so it is proof a provider really started.
 *     This script never posts that route; it waits for it.
 *   - the transcript appears only once the provider writes a conversation to
 *     disk, which is what the trivial T0.3 prompt is for.
 *   - the RECORDED zone. T1 asserts the backend places each session where the
 *     UI renders it; re-recording zones here would make T1 check the
 *     harness's own write (plan D6).
 *
 * A real `claude` opens first-run gates before its session starts, and an
 * unanswered gate parks the process at a menu forever. Only gates named in
 * `KNOWN_LAUNCH_GATES` are answered — BY LABEL: the cursor is moved to the
 * target option and Enter is sent only once a re-read shows it there; every
 * key is reported in the run. The external-CLAUDE.md-imports gate is
 * deliberately REFUSED, not answered: it approves loading code from outside
 * the cwd, which is the operator's decision. The default `--fixture-cwd` is a
 * fresh empty temp dir precisely so that gate never opens.
 *
 * Every run that changed anything ends in a teardown (T0.6): its terminals
 * are DELETEd, their records must leave the open set (residue is named), the
 * roster is restored and the temp cwd removed.
 *   - every restart (T7, T10) requires `--allow-restart`
 *   - BOTH re-run the T0 emptiness + self-hosting gates IMMEDIATELY BEFORE
 *     acting, and abort if either fails
 *   - default is a pure READ-ONLY run: it probes, it reports, it changes
 *     nothing
 *
 * `taskkill /F /IM node.exe`, `Stop-Process -Name node`, `Stop-Process -Name
 * powershell` and ANY process-tree (`/T`) kill are forbidden outright — they
 * terminate live Claude Code sessions, including the caller's own. T10 kills
 * ONE pid — the `--port` target's, as the self-hosting gate resolved it from
 * the OS and re-resolved immediately before the kill — after a runner-image
 * check and a hard refusal list (`planTargetKill` in
 * `uibridge-session-info-lib.mjs`). It never enumerates runners by name.
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
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";

import {
  KEY_DOWN,
  KEY_ENTER,
  KNOWN_LAUNCH_GATES,
  LEGACY_TENANT_BADGE_ID,
  SPAWN_TENANT_PICKER_ID,
  badgeCardinality,
  buildFixtureTerminalRequest,
  classifyTerminalSpawn,
  compareBadgeElement,
  killTargetRunner,
  openResidue,
  parseZoneViewState,
  pickFixtureSessionId,
  planMenuAnswer,
  planRosterSeed,
  planSpawnRetry,
  spawnRefuser,
  refusedGateIn,
  resolvePowerShellInterpreter,
  resolveTargetRunnerPid,
  sameRoster,
  selfHostingVerdict,
  tenantBadgeElementId,
  tenantBadgeExpectation,
  terminalBufferText,
  verifyRosterSeed,
  zoneIndexOf,
} from "./uibridge-session-info-lib.mjs";

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
/**
 * Budget for RESTORE to finish after a restart. Each restored session
 * re-launches a real provider with `--resume`, so this is provider-boot time
 * multiplied by the fixture count, not a round trip.
 */
const RESTORE_SETTLE_BUDGET_MS = 300_000;
/** Budget for a transcript to exist on disk after the trivial prompt. */
const FIXTURE_TRANSCRIPT_BUDGET_MS = 120_000;
/** Gap between fixture-progress polls (buffer reads + restore-health). */
const FIXTURE_POLL_INTERVAL_MS = 4_000;
/**
 * T0.3's prompt. A plain one-word reply: measured answering in 2-3 s with no
 * tool call (plan Phase 1), so it cannot park at a permission dialog.
 */
const FIXTURE_PROMPT = "Reply with the single word ok.";

// `NEVER_KILL` and the runner-image predicate live in
// `uibridge-session-info-lib.mjs`, beside the kill planner that enforces them.

/** The seventeen addressable row fields this oracle compares raw-to-raw (plan
 * D5). The panel renders eighteen: `prs-scanned` is not compared here. */
const INFO_FIELDS = [
  "account",
  "name",
  "terminal-id",
  "claude-session-id",
  "fleet-handle",
  "tenant",
  "spawn-stamp",
  "task-run",
  "working-dir",
  "recorded-page",
  "recorded-zone",
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
    // Config dirs `--seed-roster` may add to an EMPTY temp-runner roster.
    seedRoster: [],
    // Fixture cwd. `null` = a FRESH temp dir THIS SCRIPT creates per run and
    // removes at teardown, deliberately outside every repo: a cwd whose
    // CLAUDE.md imports files from outside it makes `claude` open an "Allow
    // external CLAUDE.md file imports?" gate before the session starts, and
    // answering that on the operator's behalf would be approving third-party
    // code they never saw. An empty temp dir raises no such question. Override
    // only if you accept that (see `--fixture-cwd`).
    fixtureCwd: null,
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
      case "--seed-roster":
        [v, i] = take(i, "--seed-roster");
        opts.seedRoster = (v || "")
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
  --accounts a,b        Claude account names for the fixture spawns (>=2),
                        as the runner's roster names them ('.claude-<name>').
  --seed-roster d1,d2   config dirs to ADD to the target's account roster when
                        an --accounts name is missing (a supervisor temp runner
                        starts with an EMPTY roster). Never on port 9876. The
                        prior roster is restored at teardown and read back.
  --fixture-cwd <path>  cwd for fixture sessions (default: a fresh temp dir
                        this script creates and removes). Point it at a repo
                        and the launch opens an external-CLAUDE.md-imports
                        gate this script REFUSES to answer for you.
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
async function measureEmptiness(owned) {
  // `owned` carries the fixtures THIS RUN built. At T0 it is empty, so this is
  // the strict "is the runner empty" gate it has always been. Before a RESTART
  // it is not, and that distinction is load-bearing: the plan's carve-out is
  // conditional on "an empty runner the implementer itself emptied", whose
  // PURPOSE is that no work belonging to someone else is destroyed. Read as
  // literal emptiness it makes T7/T10 unreachable in the only configuration
  // that can exercise them, and T8 impossible outright — T8 asserts sessions
  // were RESTORED, which requires sessions to exist before the restart
  // (measured 2026-08-21: T7 and T10 both aborted on this suite's OWN three
  // fixtures). A foreign terminal or record still blocks every mutation,
  // exactly as before; our own fixtures no longer do.
  const ownedSessions = owned?.sessions ?? new Set();
  const ownedTerminals = owned?.terminals ?? new Set();
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
  // A terminal is OURS if it hosts one of our sessions, whatever its id.
  // Restore recreates every terminal with a NEW id, so the pre-restart
  // `terminalId` stops matching the moment T7 has run -- which made T10
  // abort on the suite's own three restored fixtures while correctly
  // reporting `0 not ours` for the sessions in the very same line
  // (measured 2026-08-21). The session id survives the restart; the
  // terminal id does not, so ownership has to be keyed on the session.
  const ownedTerminalsNow = new Set(
    health.sessions.filter((x) => ownedSessions.has(x.claudeSessionId)).map((x) => x.terminalId),
  );
  const foreignAlive = alive.filter(
    (t) => !ownedTerminals.has(t.id) && !ownedTerminalsNow.has(t.id),
  );
  const foreignSessions = health.sessions.filter((x) => !ownedSessions.has(x.claudeSessionId));
  const empty = foreignAlive.length === 0 && foreignSessions.length === 0;
  const mine = ownedSessions.size > 0 ? ` (plus ${ownedSessions.size} of this run's own)` : "";
  const detail =
    `/terminals: ${terms.terminals.length} record(s), ${alive.length} alive, ` +
    `${foreignAlive.length} not ours; restore-health: ${health.sessions.length} session(s), ` +
    `${foreignSessions.length} not ours${mine}` +
    (foreignSessions.length
      ? `\nforeign sessions: ${foreignSessions.map((x) => `${String(x.claudeSessionId).slice(0, 8)}@${String(x.terminalId).slice(0, 8)}`).join(", ")}`
      : "");
  return { empty, evaluable: true, detail, health };
}

/**
 * The §0.1 self-hosting gate: would restarting the `--port` TARGET runner kill
 * THIS process?
 *
 * That is a question about the target's own pid, so the gate resolves it from
 * the OS (the process holding the LISTEN socket on `port` — plan D3, option A,
 * which reaches every build because it asks the OS rather than the runner) and
 * checks whether that pid is in this process's ancestry. Restarting a runner
 * kills ITS process tree, and only its.
 *
 * ⚠️ It used to refuse whenever ANY `qontinui-runner` was an ancestor — a proxy
 * for the property, not the property. Measured 2026-11-08: the target temp
 * runner (pid 149760) was not an ancestor, yet T0.1b refused on ancestor pid
 * 1158459, the primary. Another runner in the chain is now CONTEXT.
 *
 * ⚠️ Do NOT gate on "is my `CLAUDE_CODE_SESSION_ID` in `/control/sessions/
 * restore-health`" either. The runner tracks Claude sessions it merely NOTICED
 * (`origin: "observed"`, a transcript bind): measured 2026-08-17, such a
 * session SURVIVED the runner being killed, its ancestry being
 * `powershell.exe <- claude.exe <- powershell.exe <- WindowsTerminal.exe`.
 * Registry presence is reported as CONTEXT, never as the verdict.
 *
 * Fails CLOSED: an unresolved target pid or an unwalkable ancestry is UNKNOWN
 * (`evaluable: false, safe: false`) — we cannot prove we are outside, which is
 * not the same as being outside.
 *
 * @returns {Promise<{safe:boolean, evaluable:boolean, targetPid:number|null, detail:string}>}
 */
async function measureSelfHosting(health, port) {
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

  const target = await resolveTargetRunnerPid(port);
  const ancestry = await walkProcessAncestry();
  if (!ancestry.ok) {
    return {
      safe: false,
      evaluable: false,
      targetPid: target.pid,
      detail:
        `cannot walk process ancestry (${ancestry.error}) — cannot prove this process is ` +
        `outside the target runner's process tree. Failing closed. [${registryNote}]`,
    };
  }
  return selfHostingVerdict({ chain: ancestry.chain, target, registryNote });
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
 * Also returns `pid`: the target runner pid the self-hosting check resolved
 * (null unless `ok`), which T10 hands to `killTargetRunner`.
 */
async function gatesAllowMutation(label, ctx) {
  const owned = {
    sessions: new Set(
      (ctx?.fixtures ?? []).map((x) => x.session?.identity?.claudeSessionId).filter(Boolean),
    ),
    terminals: new Set((ctx?.fixtures ?? []).map((x) => x.terminalId).filter(Boolean)),
  };
  const e = await measureEmptiness(owned);
  const s = await measureSelfHosting(e.health, ctx.opts.port);
  const ok = e.evaluable && e.empty && s.evaluable && s.safe;
  return {
    ok,
    // The target runner pid THIS check resolved — the only pid T10 may kill.
    pid: ok ? s.targetPid : null,
    detail:
      `${label} gate re-check:\n  emptiness: ${e.empty ? "EMPTY" : "NOT EMPTY"} — ${e.detail}\n` +
      `  self-hosting: ${s.safe ? "OUTSIDE the runner" : "BLOCKED"} — ${s.detail}`,
  };
}

// ===========================================================================
// T0 — fixture setup (plan §6 "Fixture setup")
// ===========================================================================

// ===========================================================================
// Fixture plumbing — a fixture is a REAL terminal-bound provider session,
// built with no script evaluation in the webview (plan
// `2026-11-08-tenant-badge-parity-and-oracle-fixtures` D5): `POST /terminals`
// plus read-only UI Bridge component actions. The exe-mode CSP
// (`script-src 'self'`) refuses string eval, which is why the old route built
// 0/3 fixtures on every current build.
// ===========================================================================

/** Write raw text to a terminal PTY stdin (`/terminals/{id}/write` takes base64). */
async function writeTerminal(terminalId, text) {
  return getJson(`/terminals/${encodeURIComponent(terminalId)}/write`, {
    method: "POST",
    body: { data: Buffer.from(text, "utf8").toString("base64") },
  });
}

/**
 * A terminal's visible text: the base64 buffer, decoded by
 * `terminalBufferText` (cursor-column moves become spaces BEFORE the other
 * escape sequences are stripped, so a menu reads as words).
 */
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
  return { ok: true, text: terminalBufferText(raw), error: null };
}

/** How many of the buffer's last characters a gate is read from. */
const GATE_TAIL_CHARS = 6000;
/** Re-reads allowed for the cursor to land after the arrow keys. */
const GATE_VERIFY_READS = 3;
/** Gap between a key and the re-read that verifies it. */
const GATE_KEY_SETTLE_MS = 600;

/**
 * Answer any known launch gate showing in `terminalId` (plan D7): find the
 * menu's last render, move the cursor to the TARGET label, re-read the buffer
 * and send Enter only when the cursor is on it. Returns what it did so the
 * caller can report every key; a refused or unrecognised gate is an ERROR
 * carrying the buffer tail, never a guess and never a silent skip.
 *
 * Each gate is answered ONCE per terminal (`alreadyAnswered`): the buffer is
 * scrollback, so a gate's text still matches after the provider has moved on,
 * and re-answering would type into a LIVE session — spending tokens and
 * polluting the transcript T0.3 asserts on. `planMenuAnswer` also treats a gate
 * followed by a later gate's title as scrollback.
 */
async function answerLaunchGates(terminalId, alreadyAnswered = new Set()) {
  const buf = await readTerminalText(terminalId);
  if (!buf.ok) return { ok: false, answered: [], keys: [], error: `buffer read: ${buf.error}` };
  const tail = buf.text.slice(-GATE_TAIL_CHARS);
  const shortTail = () => tail.slice(-400).replace(/\s+/g, " ").trim();
  const refused = refusedGateIn(tail, alreadyAnswered);
  if (refused) {
    return {
      ok: false,
      answered: [],
      keys: [],
      error: `launch gate ${refused.id} is open and ${refused.why}`,
    };
  }
  const answered = [];
  const keys = [];
  for (const gate of KNOWN_LAUNCH_GATES) {
    let plan = planMenuAnswer(tail, gate, { answered: alreadyAnswered });
    if (plan.action === "none") continue;
    if (plan.action === "refuse") {
      return { ok: false, answered, keys, error: `${plan.reason}. Buffer tail: ${shortTail()}` };
    }
    if (plan.action === "move") {
      for (const key of plan.keys) {
        const w = await writeTerminal(terminalId, key);
        if (!w.ok) return { ok: false, answered, keys, error: `answering ${gate.id}: ${w.error}` };
        keys.push(`${gate.id}:${key === KEY_DOWN ? "Down" : "Up"}`);
        await sleep(GATE_KEY_SETTLE_MS);
      }
      let lastTail = tail;
      for (let i = 0; i < GATE_VERIFY_READS; i++) {
        const again = await readTerminalText(terminalId);
        if (!again.ok) {
          return { ok: false, answered, keys, error: `re-reading ${gate.id}: ${again.error}` };
        }
        lastTail = again.text.slice(-GATE_TAIL_CHARS);
        plan = planMenuAnswer(lastTail, gate, { answered: alreadyAnswered, afterMove: true });
        if (plan.action === "confirm") break;
        await sleep(GATE_KEY_SETTLE_MS);
      }
      if (plan.action !== "confirm") {
        const why = plan.reason ?? `${gate.id}: the menu disappeared before Enter was verified`;
        const t = lastTail.slice(-400).replace(/\s+/g, " ").trim();
        return { ok: false, answered, keys, error: `${why}. Buffer tail: ${t}` };
      }
    }
    // `confirm`: the cursor is verified on the target label.
    const w = await writeTerminal(terminalId, KEY_ENTER);
    if (!w.ok) return { ok: false, answered, keys, error: `answering ${gate.id}: ${w.error}` };
    keys.push(`${gate.id}:Enter on '${gate.target}'`);
    answered.push(gate.id);
    await sleep(GATE_KEY_SETTLE_MS);
  }
  return { ok: true, answered, keys, error: null };
}

/**
 * `terminal-page` `get-zone-view-state` — the eval-free door to the page the UI
 * shows (`pageId`), the GRID zone map (`assignments`) and the work mode. A
 * `read` action. An envelope without those fields refuses, naming the build.
 */
async function fetchZoneViewState(build) {
  const r = await getJson("/ui-bridge/control/component/terminal-page/action/get-zone-view-state", {
    method: "POST",
    body: {},
  });
  if (!r.ok) return { ok: false, error: `get-zone-view-state: ${r.error}` };
  // The control route may wrap the handler's return value; accept both.
  const d = r.data;
  const payload =
    d && typeof d === "object" && d.result && typeof d.result === "object" ? d.result : d;
  return parseZoneViewState(payload, build);
}

/** `/health` `spawnAdmission.threads.verdict`, or null when unread. */
async function readAdmissionVerdict() {
  const h = await http("/health", { timeoutMs: HEALTH_PROBE_TIMEOUT_MS });
  const sa = h.json?.spawnAdmission ?? h.json?.data?.spawnAdmission ?? null;
  return sa?.threads?.verdict ?? sa?.verdict ?? null;
}

/**
 * `POST /terminals`, retried with bounded backoff while the runner's spawn
 * admission (its resource guard) refuses for load — HTTP 500
 * `resource_guard:<severity>:<lane>: …`. Every other refusal returns at once,
 * classified; a guard that keeps refusing past the budget is reported with
 * its reason and the last `/health` admission verdict. The budget counts
 * request and `/health` time as well as the sleeps (see `planSpawnRetry` for
 * the worst-case overrun).
 */
async function postTerminalWithAdmission(body) {
  const started = Date.now();
  for (let attempt = 0; ; attempt++) {
    const attemptStarted = Date.now();
    const res = await http("/terminals", { method: "POST", body });
    const c = classifyTerminalSpawn(res);
    if (c.kind === "ok") {
      const u = unwrap(res);
      return u.ok
        ? { ok: true, data: u.data, attempts: attempt + 1 }
        : { ok: false, kind: "error", error: u.error };
    }
    const verdict = c.retry ? await readAdmissionVerdict() : null;
    const next = planSpawnRetry({
      classification: c,
      attempt,
      elapsedMs: Date.now() - started,
      // This attempt's request + /health time, as the estimate of the next one.
      lastAttemptMs: Date.now() - attemptStarted,
      admissionVerdict: verdict,
    });
    if (!next.retry) return { ok: false, kind: c.kind, error: next.reason };
    note(
      `POST /terminals refused by ${spawnRefuser(c.kind)} (${c.detail}; threads verdict ` +
        `${verdict ?? "unread"}) — retrying in ${next.delayMs}ms`,
    );
    await sleep(next.delayMs);
  }
}

/** `GET /settings/claude-config-dirs` → `{ok, dirs}`. */
async function fetchRoster() {
  const r = await getJson("/settings/claude-config-dirs");
  if (!r.ok) return { ok: false, dirs: [], error: r.error };
  const dirs = r.data?.dirs;
  if (!Array.isArray(dirs)) return { ok: false, dirs: [], error: "no `dirs` array in the answer" };
  return { ok: true, dirs: dirs.map(String), error: null };
}

/** `PUT /settings/claude-config-dirs`, then read it back: the PUT drops dirs silently. */
async function putRoster(dirs) {
  const put = await getJson("/settings/claude-config-dirs", { method: "PUT", body: { dirs } });
  if (!put.ok) return { ok: false, dirs: [], error: `PUT: ${put.error}` };
  const back = await fetchRoster();
  if (!back.ok) return { ok: false, dirs: [], error: `read-back: ${back.error}` };
  return { ok: true, dirs: back.dirs, error: null };
}

/**
 * Make every `--accounts` name resolvable on the target before any spawn.
 *
 * A supervisor temp runner starts with an EMPTY roster, and `POST /terminals`
 * with an account then answers 400 `not in roster`. Without `--seed-roster`
 * this REFUSES naming the missing accounts and the flag; with it, the named
 * config dirs are added (never on the primary's port), the accepted roster is
 * read back, and the prior roster is recorded in `ctx.rosterSeed` so teardown
 * restores it exactly — state-neutral, verified by read-back.
 */
async function ensureRoster(ctx) {
  const { opts } = ctx;
  const cur = await fetchRoster();
  if (!cur.ok) return { ok: false, error: `GET /settings/claude-config-dirs: ${cur.error}` };
  const plan = planRosterSeed({
    current: cur.dirs,
    accounts: opts.accounts,
    seedDirs: opts.seedRoster,
    port: opts.port,
    protectedPort: DEFAULT_RUNNER_PORT,
  });
  if (plan.action === "none") return { ok: true, error: null };
  if (plan.action === "refuse") return { ok: false, error: plan.error };
  // Recorded BEFORE the write, so a PUT that half-lands is still restored.
  ctx.rosterSeed = { prior: plan.prior, requested: plan.next };
  const put = await putRoster(plan.next);
  if (!put.ok) return { ok: false, error: `--seed-roster: ${put.error}` };
  const v = verifyRosterSeed({ accepted: put.dirs, requested: plan.next, accounts: opts.accounts });
  note(
    `--seed-roster: roster was ${plan.prior.length ? plan.prior.join(", ") : "EMPTY"}; ` +
      `now ${put.dirs.join(", ") || "EMPTY"} (restored at teardown)` +
      (v.dropped.length ? `; the runner DROPPED ${v.dropped.join(", ")}` : ""),
  );
  return v.ok ? { ok: true, error: null } : { ok: false, error: v.error };
}

/** M4's ceiling for a deleted fixture's record to leave the open set. */
const FIXTURE_RETIRE_BUDGET_MS = 600_000;

/**
 * DELETE `terminalIds`, then wait (up to the M4 ceiling) for every owned record
 * to leave restore-health's open set. Returns the residue BY NAME — a record
 * still open is reported, never dropped. Measured: M1 records leave in ~1-2 s
 * (`closeReason: pty-exit`).
 */
async function deleteAndAwaitRetire(terminalIds, sessionIds) {
  const owned = {
    terminalIds: new Set([...terminalIds].map(String)),
    sessionIds: new Set([...sessionIds].map(String)),
  };
  // Every owned session's CURRENT terminal: a restore re-creates terminals
  // under new ids, and the session id is what survives it.
  const before = await fetchRestoreHealth();
  if (before.ok) {
    for (const r of before.sessions) {
      if (owned.sessionIds.has(String(r.claudeSessionId)))
        owned.terminalIds.add(String(r.terminalId));
      if (owned.terminalIds.has(String(r.terminalId)) && r.claudeSessionId) {
        owned.sessionIds.add(String(r.claudeSessionId));
      }
    }
  }
  const deleteErrors = [];
  const live = await fetchTerminals();
  const liveIds = new Set(live.ok ? live.terminals.map((t) => String(t.id)) : owned.terminalIds);
  for (const id of owned.terminalIds) {
    if (!liveIds.has(id)) continue;
    const d = await getJson(`/terminals/${encodeURIComponent(id)}`, { method: "DELETE" });
    if (!d.ok) deleteErrors.push(`DELETE /terminals/${id}: ${d.error}`);
  }
  let residue = [];
  let readError = null;
  const deadline = Date.now() + FIXTURE_RETIRE_BUDGET_MS;
  const started = Date.now();
  while (Date.now() < deadline) {
    const rh = await fetchRestoreHealth();
    if (rh.ok) {
      readError = null;
      residue = openResidue(rh.sessions, owned);
      if (residue.length === 0) break;
    } else {
      readError = rh.error;
    }
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  return {
    residue,
    deleteErrors,
    readError,
    deleted: owned.terminalIds.size,
    ms: Date.now() - started,
  };
}

/**
 * Build ONE fixture: a real, TERMINAL-BOUND provider session, with no script
 * evaluation anywhere.
 *
 *   1. `POST /terminals` with `buildFixtureTerminalRequest` — Phase 1's
 *      measured spelling M1: a bare `claude`, account-pinned by the RUNNER
 *      (roster-validated; it seeds `CLAUDE_CONFIG_DIR`). The runner's identity
 *      seam pins the session id and writes the spawn-time record itself, with
 *      the account stamped — one authoritative record per terminal. Retried
 *      while spawn admission refuses for load.
 *   2. take the zone the FRONTEND assigned, from `get-zone-view-state`
 *      `assignments` — never invent one;
 *   3. read the session id from restore-health by `terminalId`, answering the
 *      first-run launch gates by label meanwhile, and wait for the provider to
 *      CONFIRM itself.
 *
 * This script does not and must not write `confirmed_at`: it flips only when
 * the provider's SessionStart hook POSTs `/control/session-open`, which happens
 * only if a real provider started in that terminal. Nor does it write the
 * record's zone (plan D6): T1 asserts that the BACKEND places each session
 * where the UI shows it, a product property the harness must not manufacture.
 */
async function spawnFixtureSession(spec, ctx) {
  const { taskName, account } = spec;
  const { pageId, fixtureCwd, build } = ctx;

  const created = await postTerminalWithAdmission(
    buildFixtureTerminalRequest({ pageId, title: taskName, fixtureCwd, account }),
  );
  if (!created.ok) {
    const hint =
      created.kind === "roster"
        ? " — the account is not in this runner's roster; a supervisor temp runner starts EMPTY (see --seed-roster)"
        : "";
    return { ok: false, error: `${taskName}: POST /terminals: ${created.error}${hint}` };
  }
  const terminalId = created.data?.id;
  if (!terminalId) return { ok: false, error: `${taskName}: POST /terminals returned no id` };
  ctx.spawnedTerminals.add(String(terminalId));

  // The zone is the FRONTEND's to decide: a backend-created terminal reaches
  // the tab list through the re-sync, and the layout auto-grow assigns it.
  let zoneIndex = null;
  let viewError = null;
  const zoneDeadline = Date.now() + FIXTURE_APPEAR_BUDGET_MS;
  while (Date.now() < zoneDeadline) {
    const v = await fetchZoneViewState(build);
    if (v.ok) {
      viewError = null;
      zoneIndex = zoneIndexOf(v.view.assignments, terminalId);
      if (zoneIndex !== null) break;
    } else {
      viewError = v.error;
      if (v.tmux) break;
    }
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  if (zoneIndex === null) {
    return {
      ok: false,
      terminalId,
      error:
        `${taskName}: the frontend never assigned terminal ${terminalId} a zone within ` +
        `${FIXTURE_APPEAR_BUDGET_MS}ms on page ${pageId}` +
        (viewError ? ` (${viewError})` : ""),
    };
  }

  const answered = new Set();
  const keys = [];
  let sessionId = null;
  let pickError = null;
  let confirmed = false;
  const confirmDeadline = Date.now() + FIXTURE_CONFIRM_BUDGET_MS;
  while (Date.now() < confirmDeadline) {
    const rh = await fetchRestoreHealth();
    if (rh.ok) {
      const pick = pickFixtureSessionId(rh.sessions, terminalId);
      if (pick.ok) {
        sessionId = pick.sessionId;
        pickError = null;
        if (pick.row?.confirmed) {
          confirmed = true;
          break;
        }
      } else if (!pick.pending) {
        return { ok: false, terminalId, error: `${taskName}: ${pick.error}` };
      } else {
        pickError = pick.error;
      }
    }
    const gate = await answerLaunchGates(terminalId, answered);
    keys.push(...gate.keys);
    if (!gate.ok) return { ok: false, terminalId, error: `${taskName}: ${gate.error}` };
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
      terminalId,
      error:
        `${taskName}: never confirmed within ${FIXTURE_CONFIRM_BUDGET_MS}ms ` +
        `(session ${sessionId ?? `unknown — ${pickError}`}). Confirmation comes from the provider ` +
        `SessionStart hook, so this means the provider never started. PTY tail: ${tail}`,
    };
  }

  const infoDeadline = Date.now() + FIXTURE_APPEAR_BUDGET_MS;
  while (Date.now() < infoDeadline) {
    const info = await fetchSessionsInfo();
    if (info.ok && info.status === "ok") {
      const session = info.sessions.find((s) => s.identity?.claudeSessionId === sessionId);
      if (session) {
        return { ok: true, session, terminalId, zoneIndex, gatesAnswered: [...answered], keys };
      }
    }
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  return {
    ok: false,
    terminalId,
    error:
      `${taskName}: confirmed, but never appeared in /control/sessions/info within ` +
      `${FIXTURE_APPEAR_BUDGET_MS}ms (session ${sessionId})`,
  };
}

const T02_WHAT = "three fixture sessions across >=2 Claude accounts, in distinct RENDERED zones";

async function createFixtures(ctx) {
  const { opts } = ctx;
  const accounts = opts.accounts;
  // A SECOND account is needed by T0.2's account half and T5's distinctness
  // arm. Every other arm needs only three sessions, so one account still
  // builds all three fixtures; the account half is then reported as NOT
  // EVALUABLE, by name, rather than failed or silently passed.
  if (accounts.length < 1) {
    skip(
      "T0.2",
      T02_WHAT,
      "--fixture needs at least one name in --accounts; the roster is machine-global and this script will not guess account names",
    );
    skip("T0.2b", T02B_WHAT, "no fixtures");
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
        `T0.2's >=2-account half and T5's cross-account arm are NOT evaluable; every other arm runs.`,
    );
  }

  const refuseAll = (why) => {
    fail("T0.2", T02_WHAT, why);
    skip("T0.2b", T02B_WHAT, "no fixtures");
    skip(
      "T0.3",
      "a trivial prompt in each fixture session (transcript + confirmed_at)",
      "no fixtures",
    );
    skip("T0.4", "one operator-renamed session and one Claude-derived one", "no fixtures");
  };

  // The page and the grid, from the eval-free read. An envelope without
  // `pageId` / `assignments` refuses here, naming the build; so does tmux mode.
  const view = await fetchZoneViewState(ctx.build);
  if (!view.ok) {
    refuseAll(view.error);
    return;
  }

  // Fixtures run in a directory this script owns, outside every repo. By
  // default a FRESH temp dir per run, removed at teardown: it exercises the
  // gate path every run and leaves nothing behind.
  try {
    if (opts.fixtureCwd) {
      mkdirSync(opts.fixtureCwd, { recursive: true });
      ctx.fixtureCwd = opts.fixtureCwd;
    } else {
      ctx.fixtureCwd = mkdtempSync(join(tmpdir(), "qontinui-uibridge-fixtures-"));
      ctx.ownedCwd = ctx.fixtureCwd;
    }
  } catch (e) {
    refuseAll(`could not create the fixture cwd: ${e}`);
    return;
  }

  const roster = await ensureRoster(ctx);
  if (!roster.ok) {
    refuseAll(roster.error);
    return;
  }

  const fixtureCtx = {
    pageId: view.view.pageId,
    fixtureCwd: ctx.fixtureCwd,
    build: ctx.build,
    spawnedTerminals: ctx.spawnedTerminals,
  };
  note(
    `fixtures: cwd ${ctx.fixtureCwd}; page ${view.view.pageId} (get-zone-view-state, layout ` +
      `${view.view.layoutId ?? "?"}); accounts ${accounts.join(", ")}; launch: bare 'claude', account-pinned`,
  );

  const specs = [
    { taskName: "uibridge-fixture-a", account: accounts[0] },
    { taskName: "uibridge-fixture-b", account: accounts[singleAccount ? 0 : 1] },
    { taskName: "uibridge-fixture-c", account: accounts[0] },
  ];
  const created = [];
  const problems = [];
  const failedTerminals = [];
  const gatesAnswered = new Set();
  const keysSent = [];
  for (const spec of specs) {
    const r = await spawnFixtureSession(spec, fixtureCtx);
    keysSent.push(...(r.keys ?? []));
    if (!r.ok) {
      problems.push(r.error);
      if (r.terminalId) failedTerminals.push(String(r.terminalId));
      continue;
    }
    for (const g of r.gatesAnswered ?? []) gatesAnswered.add(g);
    created.push({ spec, session: r.session, terminalId: r.terminalId, zoneIndex: r.zoneIndex });
    // T0.2b reads the ONE-terminal page, which exists only between the first
    // fixture confirming and the second one landing.
    if (created.length === 1) await runSingleZoneBadgeArm(ctx, created[0]);
  }
  if (created.length === 0) {
    skip("T0.2b", T02B_WHAT, "no fixture was created, so there is no single-zone page to read");
  }
  ctx.fixtures = created;

  // A failed fixture's terminal is torn down NOW: left up, it is a foreign
  // zone to every later arm, and its record poisons the next run's T0.1.
  if (failedTerminals.length > 0) {
    const t = await deleteAndAwaitRetire(failedTerminals, []);
    for (const id of failedTerminals) ctx.spawnedTerminals.delete(id);
    note(
      `cleaned up after failed fixture(s): DELETEd ${failedTerminals.length} terminal(s)` +
        (t.residue.length
          ? `; RESIDUE still open: ${t.residue.join(", ")}`
          : "; no record left open") +
        (t.deleteErrors.length ? `; ${t.deleteErrors.join("; ")}` : ""),
    );
    ctx.residue.push(...t.residue);
  }
  // Every automated answer is reported. A launch gate answered silently is a
  // decision made on the operator's behalf without a record of it.
  if (gatesAnswered.size > 0) {
    note(
      `answered first-run launch gate(s): ${[...gatesAnswered].join(", ")}; keys ${keysSent.join(", ")}`,
    );
  }

  // T0.2 is FIXTURE CONSTRUCTION: the RENDERED zones (the grid's assignments)
  // are distinct. The RECORDED zone is not re-written here (plan D6) — T1
  // keeps that product claim, and fails honestly while it does not hold.
  let renderedZones = created.map(() => null);
  let lastView = view;
  const settleDeadline = Date.now() + FIXTURE_APPEAR_BUDGET_MS;
  while (created.length > 0 && Date.now() < settleDeadline) {
    lastView = await fetchZoneViewState(ctx.build);
    if (lastView.ok) {
      renderedZones = created.map((c) => zoneIndexOf(lastView.view.assignments, c.terminalId));
      if (
        renderedZones.every((z) => z !== null) &&
        new Set(renderedZones).size === created.length
      ) {
        break;
      }
    }
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  created.forEach((c, i) => {
    if (renderedZones[i] !== null) c.zoneIndex = renderedZones[i];
  });
  const refreshed = await fetchSessionsInfo();
  if (refreshed.ok && refreshed.status === "ok") {
    for (const c of created) {
      const fresh = refreshed.sessions.find(
        (x) => x.identity?.claudeSessionId === c.session.identity?.claudeSessionId,
      );
      if (fresh) c.session = fresh;
    }
  }
  const zones = new Set(renderedZones.filter((z) => z !== null));
  const accountLabels = new Set(created.map((c) => c.session.account?.label).filter(Boolean));
  const zoneText = created
    .map((c, i) => `${c.spec.taskName}=zone ${renderedZones[i] ?? "?"}`)
    .join(", ");
  if (problems.length || created.length !== 3) {
    fail("T0.2", T02_WHAT, [`created ${created.length}/3`, ...problems].join("\n"));
  } else if (zones.size !== 3 || renderedZones.some((z) => z === null)) {
    fail(
      "T0.2",
      T02_WHAT,
      `rendered zones were ${zoneText} — not three distinct zones` +
        (lastView.ok ? "" : ` (last get-zone-view-state read failed: ${lastView.error})`),
    );
  } else if (accountLabels.size < 2 && singleAccount) {
    skip(
      "T0.2",
      T02_WHAT,
      `only ONE account was supplied (--accounts ${accounts[0]}), so the >=2-account half cannot be ` +
        `posed here. The rest held: 3/3 fixtures, rendered zones ${[...zones].join(",")}, account ` +
        `label ${[...accountLabels].join(",") || "?"}. Supply a second roster account to evaluate it.`,
    );
  } else if (accountLabels.size < 2) {
    fail(
      "T0.2",
      T02_WHAT,
      `--accounts named ${accounts.length} accounts (${accounts.join(", ")}), but only ` +
        `${accountLabels.size} distinct account label(s) were observed: ${[...accountLabels].join(", ") || "none"}`,
    );
  } else {
    pass(
      "T0.2",
      T02_WHAT,
      `rendered zones ${[...zones].join(",")} (${zoneText}); accounts ${[...accountLabels].join(",")}`,
    );
  }

  // Step 3 — a trivial prompt each, so every fixture has a real transcript and
  // a `confirmed_at`. A plain one-word reply: measured finishing in 2-3 s with
  // no tool call, so it can never park at a permission dialog. Both halves are
  // read back from `/control/sessions/restore-health`, which reports them per
  // open record — a 200 from submit-prompt is evidence of neither.
  const promptFindings = [];
  for (const c of created) {
    const tid = c.session.identity.terminalId;
    const sid = c.session.identity.claudeSessionId;
    const r = await getJson(`/terminals/${encodeURIComponent(tid)}/submit-prompt`, {
      method: "POST",
      body: { message: FIXTURE_PROMPT },
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

  const s = await measureSelfHosting(e.health, ctx.opts.port);
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
    skip("T0.2", T02_WHAT, why);
    skip("T0.2b", T02B_WHAT, why);
    skip("T0.3", "a trivial prompt in each fixture session (transcript + confirmed_at)", why);
    skip("T0.4", "one operator-renamed session and one Claude-derived one", why);
  } else if (!ctx.gates.empty || !ctx.gates.notSelfHosted) {
    const why = "the T0 gates did not hold — fixture creation is aborted (plan §0/R4)";
    skip("T0.2", T02_WHAT, why);
    skip("T0.2b", T02B_WHAT, why);
    skip("T0.3", "a trivial prompt in each fixture session (transcript + confirmed_at)", why);
    skip("T0.4", "one operator-renamed session and one Claude-derived one", why);
  } else {
    await createFixtures(ctx);
  }
}

// ===========================================================================
// T0.2b / T2b — the per-zone tenant badge (plan Phase 5)
// ===========================================================================

const T02B_WHAT =
  "single zone: the info trigger and the tenant badge render in the one-terminal strip";
const T2B_WHAT =
  "every bound zone's terminal-tenant-badge-<n> matches its expectation; ids distinct, one per zone expecting a badge";

/** Wait for one element id to be registered; returns the registry ids last read. */
async function waitForElement(id, budgetMs = TRIGGER_MOUNT_BUDGET_MS) {
  const deadline = Date.now() + budgetMs;
  let last = { ok: false, ids: new Set(), error: "never read" };
  while (Date.now() < deadline) {
    const reg = await fetchElements();
    last = reg.ok
      ? { ok: true, ids: new Set(reg.elements.map((e) => String(e?.id ?? ""))), error: null }
      : { ok: false, ids: new Set(), error: reg.error };
    if (last.ok && last.ids.has(id)) return last;
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  return last;
}

/** The badge element for `zoneIndex`, or null when its per-zone id is not registered. */
async function readBadgeElement(ids, zoneIndex) {
  const id = tenantBadgeElementId(zoneIndex);
  if (!ids.has(id)) return { ok: true, element: null, error: null };
  const el = await fetchElement(id);
  return el.ok
    ? { ok: true, element: el.element, error: null }
    : { ok: false, element: null, error: el.error };
}

/** A note appended to a missing-badge finding when the pre-D1 shared id is what renders. */
const legacyNote = (ids) =>
  ids.has(LEGACY_TENANT_BADGE_ID)
    ? ` (the shared pre-per-zone id '${LEGACY_TENANT_BADGE_ID}' IS registered — this build predates the per-zone badge id)`
    : "";

/**
 * Fold badge findings (`{status, detail}`, status PASS / FAIL / UNKNOWN) into
 * one row. Any FAIL fails the arm; otherwise any UNKNOWN makes it NOT
 * evaluated (a SKIP whose text says UNKNOWN) — never a PASS.
 */
function recordBadgeArm(id, what, findings) {
  const bad = findings.filter((f) => f.status === "FAIL");
  const unknown = findings.filter((f) => f.status === "UNKNOWN");
  if (findings.length === 0) return skip(id, what, "no subjects to evaluate");
  if (bad.length) return fail(id, what, bad.map((f) => f.detail).join("\n"));
  if (unknown.length) {
    return skip(id, what, `UNKNOWN — ${unknown.map((f) => f.detail).join("\n")}`);
  }
  return pass(id, what, findings.map((f) => f.detail).join("; "));
}

/**
 * T0.2b — the one-terminal page, the layout most operators run, where the
 * badge used to be missing entirely (defect 1). Runs right after the FIRST
 * fixture confirms, and only when the page has exactly one zone; any other
 * layout SKIPs naming it — this arm never changes the layout.
 */
async function runSingleZoneBadgeArm(ctx, fixture) {
  const v = await fetchZoneViewState(ctx.build);
  if (!v.ok) return skip("T0.2b", T02B_WHAT, `get-zone-view-state: ${v.error}`);
  if (v.view.zoneCount !== 1) {
    return skip(
      "T0.2b",
      T02B_WHAT,
      `the page's layout is '${v.view.layoutId ?? "?"}' with ${v.view.zoneCount ?? "?"} zone(s), not one — ` +
        `this arm reads the single-zone strip and never changes the layout`,
    );
  }
  const zone = zoneIndexOf(v.view.assignments, fixture.terminalId);
  if (zone === null || zone === undefined) {
    // An absent assignment is UNKNOWN — never zone 0 by default.
    return skip(
      "T0.2b",
      T02B_WHAT,
      `UNKNOWN — get-zone-view-state assigns terminal ${fixture.terminalId} to no zone, so which ` +
        `zone's badge to read is unknown`,
    );
  }
  const triggerId = infoElementId("trigger", zone);
  const reg = await waitForElement(triggerId);
  if (!reg.ok) return fail("T0.2b", T02B_WHAT, `GET /ui-bridge/control/elements: ${reg.error}`);
  const findings = [];
  findings.push(
    reg.ids.has(triggerId)
      ? { status: "PASS", detail: `${triggerId} rendered` }
      : {
          status: "FAIL",
          detail: `${triggerId} absent after ${TRIGGER_MOUNT_BUDGET_MS}ms on the one-terminal page`,
        },
  );
  const info = await fetchSessionsInfo();
  const sid = fixture.session?.identity?.claudeSessionId;
  const session = info.ok ? info.sessions.find((s) => s.identity?.claudeSessionId === sid) : null;
  const tenancy = session ? (session.tenancy ?? null) : (fixture.session?.tenancy ?? null);
  const expectation = tenantBadgeExpectation({
    pickerPresent: reg.ids.has(SPAWN_TENANT_PICKER_ID),
    tenancy,
  });
  const badge = await readBadgeElement(reg.ids, zone);
  if (!badge.ok) {
    findings.push({
      status: "FAIL",
      detail: `GET element ${tenantBadgeElementId(zone)}: ${badge.error}`,
    });
  } else {
    const cmp = compareBadgeElement(badge.element, expectation, tenancy, {
      build: ctx.build,
      label: `zone ${zone} ${tenantBadgeElementId(zone)}`,
    });
    for (const f of cmp.findings) {
      const extra = f.check === "presence" && f.status === "FAIL" ? legacyNote(reg.ids) : "";
      findings.push({ status: f.status, detail: f.detail + extra });
    }
  }
  return recordBadgeArm("T0.2b", T02B_WHAT, findings);
}

/**
 * T2b — every session-bound zone's badge, by its per-zone id, against the
 * expectation its own session's tenancy implies; then the cardinality: the
 * badge ids are distinct and as many as the zones expecting one (defect 2:
 * one `terminal.tenant-badge` for two zones).
 */
async function runMultiZoneBadgeArm(ctx, z, info) {
  if (!z.ok) return skip("T2b", T2B_WHAT, `elements read failed: ${z.error}`);
  const bound = z.zones.filter((x) => x.claudeSessionId);
  if (z.zones.length < 2) {
    return skip(
      "T2b",
      T2B_WHAT,
      `${z.zones.length} zone header(s) — not a multi-zone page (T0.2b covers the single zone)`,
    );
  }
  if (bound.length === 0)
    return skip("T2b", T2B_WHAT, "no zone header carries a data-claude-session-id");
  if (!info.ok || info.status !== "ok") {
    return skip(
      "T2b",
      T2B_WHAT,
      `UNKNOWN — /control/sessions/info ${info.ok ? `status='${info.status}'` : info.error}, so no zone's tenancy is known`,
    );
  }
  const ids = new Set((z.registry || []).map((e) => String(e?.id ?? "")));
  const pickerPresent = ids.has(SPAWN_TENANT_PICKER_ID);
  const findings = [];
  const zonesExp = [];
  for (const zone of bound) {
    const session = info.sessions.find((s) => s.identity?.claudeSessionId === zone.claudeSessionId);
    const tenancy = session?.tenancy ?? null;
    const expectation = tenantBadgeExpectation({ pickerPresent, tenancy });
    zonesExp.push({ zoneIndex: zone.zoneIndex, expect: expectation.expect });
    const badge = await readBadgeElement(ids, zone.zoneIndex);
    if (!badge.ok) {
      findings.push({
        status: "FAIL",
        detail: `GET element ${tenantBadgeElementId(zone.zoneIndex)}: ${badge.error}`,
      });
      continue;
    }
    const cmp = compareBadgeElement(badge.element, expectation, tenancy, {
      build: ctx.build,
      label: `zone ${zone.zoneIndex} ${tenantBadgeElementId(zone.zoneIndex)}`,
    });
    for (const f of cmp.findings) {
      const extra = f.check === "presence" && f.status === "FAIL" ? legacyNote(ids) : "";
      findings.push({ status: f.status, detail: f.detail + extra });
    }
  }
  const unboundZones = z.zones.filter((x) => !x.claudeSessionId).map((x) => x.zoneIndex);
  const card = badgeCardinality({ registryIds: [...ids], zones: zonesExp, unboundZones });
  findings.push({
    status: card.ok === true ? "PASS" : card.ok === false ? "FAIL" : "UNKNOWN",
    detail: `cardinality: ${card.detail}`,
  });
  if (card.unboundBadges.length) {
    findings.push({
      status: "UNKNOWN",
      detail:
        `badge(s) ${card.unboundBadges.join(", ")} render on zone(s) whose header carries no ` +
        `data-claude-session-id yet — whether those zones owe a badge is UNKNOWN until they bind`,
    });
  }
  return recordBadgeArm("T2b", T2B_WHAT, findings);
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
  const { identity, name, account, placement, lifecycle, prs, tenancy } = body;
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
    // The ACTING tenant, from the runner's one derived projection
    // (`tenancy.display`, plan 2026-10-17-session-info-tenant-row-renders-
    // unknown-for-a-resolved-tenant) — not the spawn stamp, which is null for
    // every spawn that chose no tenant. `unknown` carries no value.
    case "tenant": {
      const display = tenancy?.display;
      if (!display || display.status === "unknown") return null;
      return display.tenantId ?? null;
    }
    // The spawn stamp, on its own row.
    case "spawn-stamp":
      return tenancy?.row?.tenantId ?? null;
    case "task-run":
      return identity?.taskRunId ?? null;
    case "working-dir":
      return placement?.workingDir ?? null;
    // The RECORD's placement. `pageId` follows the ordinary unknown contract
    // (empty ⇒ never observed), but `zoneIndex` does NOT: `-1` is a value the
    // record genuinely holds, so it is compared as the raw string "-1" rather
    // than as an absence — only a missing/non-integer index is unknown.
    case "recorded-page":
      return placement?.pageId ? String(placement.pageId) : null;
    case "recorded-zone":
      return Number.isInteger(placement?.zoneIndex) ? String(placement.zoneIndex) : null;
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
  // Both passes need this, not just the pre-restart one: after a restart the
  // tabs re-acquire their `claudeSessionId` through the same periodic backfill,
  // so a post-restore pass sampled immediately reports triggers and panels
  // absent for sessions that simply had not re-bound yet.
  await waitForTriggersToMount(ctx?.fixtures?.length ?? 0);
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
async function waitForTriggersToMount(expected = 0, budgetMs = TRIGGER_MOUNT_BUDGET_MS) {
  const deadline = Date.now() + budgetMs;
  while (Date.now() < deadline) {
    const z = await collectZones();
    const bound = (z.zones || []).filter((x) => x.claudeSessionId);
    // Waiting only for the triggers of ALREADY-bound zones is satisfied
    // trivially while most zones are still unbound: the tab acquires its
    // `claudeSessionId` on the same periodic backfill, so a zone that has not
    // bound yet is simply absent from `bound`. That let a pass run against ONE
    // bound zone out of three -- T1 saw 3 headers, T2 saw 1 subject, and T5
    // SKIPped for want of a second session to compare (measured 2026-08-21).
    // So wait for the fixtures to become bound, not just for what is bound now.
    if (bound.length >= expected && bound.length > 0) {
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

  // T1/T2 read the zone headers and triggers directly, BEFORE runDropdownPass
  // gets its own wait, so they need this too. Idempotent: it returns at once
  // when everything is already mounted.
  await waitForTriggersToMount(ctx?.fixtures?.length ?? 0);

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

  // --- T2b -------------------------------------------------------------
  await runMultiZoneBadgeArm(ctx, z, info);

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

/**
 * The PowerShell interpreter on PATH (`powershell`, falling back to `pwsh`),
 * or null. Resolved once, lazily; the SAME value proves T10's door runnable
 * before the kill and runs the restart after it, so the two cannot disagree.
 */
let resolvedInterpreter;
function powershellInterpreter() {
  if (resolvedInterpreter === undefined) {
    resolvedInterpreter = resolvePowerShellInterpreter({
      pathEnv: process.env.PATH ?? process.env.Path ?? "",
      platform: process.platform,
      exists: existsSync,
    });
  }
  return resolvedInterpreter;
}

/** Run a PowerShell command, returning `{ok, stdout, stderr, code}`. */
function powershell(args, { timeoutMs = 600_000 } = {}) {
  const interpreter = powershellInterpreter();
  if (!interpreter) {
    return {
      ok: false,
      code: null,
      stdout: "",
      stderr: "",
      error: "no PowerShell interpreter (powershell, pwsh) on PATH",
    };
  }
  const r = spawnSync(interpreter, ["-NoProfile", "-ExecutionPolicy", "Bypass", ...args], {
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

// ===========================================================================
// T6-T9 — restore
// ===========================================================================

/** Skip the whole post-restore dropdown re-run with one stated reason. */
function skipT9Pass(why) {
  skip("T9.T3", "the panel opens and renders every required field (post-restore)", why);
  skip("T9.T4", "rendered raw value == backend raw value, field by field (post-restore)", why);
  skip("T9.T5", "account and name are correct and distinct across sessions (post-restore)", why);
}

/**
 * Wait for RESTORE to finish, not merely for the runner to answer /health.
 *
 * `waitForHealthy` returns as soon as the process serves requests, but restore
 * runs on after that: the runner recreates each terminal and re-launches its
 * provider with `--resume`, and a session sits in the census's `missing[]` as
 * `resume-failed` until its provider comes back. Sampling at `healthy` catches
 * that in flight and reports a restore that had not finished as one that had
 * FAILED.
 *
 * Measured 2026-08-21: T8 read `verdict='mismatch'` with all six sessions
 * `resume-failed`; the same census a few minutes later read `match`, 6 restored,
 * 0 missing, every session confirmed with a transcript. Nothing was wrong except
 * when it was asked.
 *
 * Settles on the census reaching a terminal shape (no `resume-failed` still
 * pending) or the budget expiring. It hides nothing: a restore that genuinely
 * fails still fails, having been given time to succeed first.
 */
async function waitForRestoreToSettle(budgetMs = RESTORE_SETTLE_BUDGET_MS) {
  const deadline = Date.now() + budgetMs;
  let last = null;
  while (Date.now() < deadline) {
    const c = await fetchCensus();
    last = c;
    if (c.ok) {
      const missing = c.census?.missing ?? [];
      const pending = missing.filter((m) => String(m.reason ?? "").includes("resume"));
      if (pending.length === 0) return c;
    }
    await sleep(FIXTURE_POLL_INTERVAL_MS);
  }
  return last ?? (await fetchCensus());
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

  const gate = await gatesAllowMutation("T7 pre-restart", ctx);
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
  // Restore continues after /health answers — wait for it before judging it.
  const post = await waitForRestoreToSettle();
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
    if (missing.length) {
      problems.push(
        `missing[]: ${missing.map((m) => `${m.claudeSessionId}(${m.reason})`).join(", ")}`,
      );
      // Say WHY, not just THAT. A `resume-failed` row means the frontend never
      // typed a resume into the terminal restore rebuilt for it, and
      // `classifyRestoreAction` decides that from exactly two record fields:
      // `confirmedAt == null` or `transcriptExists === false` demote the row to
      // `terminal-only`, which by design types nothing. Reporting those two
      // alongside the failure turns "one session did not come back" into a
      // diagnosis, and distinguishes a genuine dispatch bug from the documented
      // fail-closed transcript probe (`probe_transcript_exists` demotes when it
      // cannot resolve the path, and the record carries the PTY's spawn cwd
      // while the transcript follows the PROVIDER's cwd).
      const rh = await fetchRestoreHealth();
      if (rh.ok) {
        for (const m of missing) {
          const row = rh.sessions.find((x) => x.claudeSessionId === m.claudeSessionId);
          problems.push(
            row
              ? `  ${String(m.claudeSessionId).slice(0, 8)} deciding inputs: confirmed=${row.confirmed} ` +
                  `transcriptExists=${row.transcriptExists} origin=${row.origin} restorable=${row.restorable} ` +
                  `workingDir=${row.workingDir}`
              : `  ${String(m.claudeSessionId).slice(0, 8)}: no restore-health row — the record is gone, not merely unresumed`,
          );
        }
      } else {
        problems.push(`  (could not read restore-health for the deciding inputs: ${rh.error})`);
      }
    }
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
    if (surprise.length) {
      // Name the CAUSE, because these two look identical in the output and only
      // one is a restore defect.
      //
      // A session restored that this run never created is almost always a
      // PRECONDITION violation, not a restore bug: `pty-exit` / `poll-dead`
      // closes stay restorable for 10 minutes by design
      // (RESTORABLE_PTY_EXIT_MS), and `restore-health` reports OPEN rows only —
      // so T0.1 can pass on a genuinely empty runner while restorable-CLOSED
      // rows from a previous run sit invisible, waiting to be resurrected by the
      // very restart this test performs. Measured 2026-08-22: three back-to-back
      // runs, the middle one inheriting two such rows and failing T1, T4, T8 and
      // T10 while T0.1 reported `0 session(s), 0 not ours`.
      //
      // Reporting that as "the correct sessions were restored" failing invites
      // exactly the wrong conclusion — it reads as a restore defect, and it was
      // twice mistaken for one.
      const ownFixtures = new Set(
        (ctx.fixtures ?? []).map((x) => x.session?.identity?.claudeSessionId).filter(Boolean),
      );
      const inherited = surprise.filter((id) => !ownFixtures.has(id));
      const ours = surprise.filter((id) => ownFixtures.has(id));
      if (ours.length) {
        problems.push(
          `restored but absent from the oracle's expected set (created by THIS run — a real ` +
            `discrepancy): ${ours.join(", ")}`,
        );
      }
      if (inherited.length) {
        problems.push(
          `PRECONDITION, not a restore defect — ${inherited.length} session(s) this run never ` +
            `created were resurrected: ${inherited.join(", ")}. They are restorable-CLOSED rows ` +
            `left by an earlier run (pty-exit/poll-dead stays restorable for 10 minutes) and are ` +
            `INVISIBLE to T0.1, which sees open rows only. Leave >10 minutes between runs that ` +
            `use --allow-restart, or start from a runner whose store has no recent fixture rows.`,
        );
      }
    }
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
      "--allow-restart is OFF by default. T10 crashes the --port target runner (its one pid, SIGKILL, no tree flag) " +
        "and that is inside the plan's §0 carve-out ONLY for an empty runner the implementer itself emptied.",
    );
  } else {
    const gate = await gatesAllowMutation("T10 pre-crash", ctx);
    if (!gate.ok) {
      fail(
        "T10",
        "a hard-crash boot reports verdict 'unknown', not 'match'",
        `ABORTED — ${gate.detail}`,
      );
    } else {
      // Only the pid the gate just resolved, re-resolved and planned again
      // immediately before the kill (plan D4); `killTargetRunner` enumerates
      // nothing else on the box.
      // The restart door must be PROVEN runnable before the kill — an
      // interpreter on PATH and the script on disk — or the kill would leave
      // the target dead.
      const interpreter = powershellInterpreter();
      const k = await killTargetRunner(gate.pid, (pid, signal) => process.kill(pid, signal), {
        port: opts.port,
        devStartPort: DEFAULT_RUNNER_PORT,
        devStartExplicit: opts.devStartExplicit,
        platform: process.platform,
        interpreter,
        devStart: opts.devStart,
        doorRunnable: interpreter !== null && existsSync(opts.devStart),
      });
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
// Teardown — state-neutral, verified by read-back
// ===========================================================================

/**
 * Leave the target as found: DELETE every terminal this run created, wait for
 * each owned record to leave the open set (residue is NAMED), restore a
 * `--seed-roster` change to the exact prior roster and read it back, and
 * remove a fixture cwd this run created. Recorded as T0.6 whenever this run
 * changed anything, so residue is a FAIL row rather than a log line.
 */
async function teardown(ctx) {
  const touched =
    ctx.spawnedTerminals.size > 0 ||
    (ctx.fixtures ?? []).length > 0 ||
    ctx.rosterSeed !== null ||
    ctx.ownedCwd !== null ||
    ctx.residue.length > 0;
  if (!touched) return;
  section("Teardown");
  const what = "teardown: fixtures deleted, their records left the open set, the roster restored";
  const problems = [];
  const done = [];

  // ORDER MATTERS: the fixture terminals are deleted and their records awaited
  // out of the open set BEFORE the roster is restored, because a restore that
  // removes an account while a session spawned under it is still open strands
  // that session under an account the runner no longer lists.
  const sessionIds = (ctx.fixtures ?? [])
    .map((f) => f.session?.identity?.claudeSessionId)
    .filter(Boolean);
  /** Everything the retire step could not confirm gone — named, never dropped. */
  const unretired = [];
  if (ctx.spawnedTerminals.size > 0 || sessionIds.length > 0) {
    const t = await deleteAndAwaitRetire(ctx.spawnedTerminals, sessionIds);
    done.push(`${t.deleted} terminal(s) DELETEd; open set re-read for ${Math.round(t.ms / 1000)}s`);
    problems.push(...t.deleteErrors);
    unretired.push(...t.deleteErrors);
    if (t.readError) {
      problems.push(`restore-health unreadable at the end: ${t.readError}`);
      unretired.push(`retirement UNKNOWN (restore-health unreadable: ${t.readError})`);
    }
    if (t.residue.length) {
      problems.push(`RESIDUE still open: ${t.residue.join(", ")}`);
      unretired.push(...t.residue);
    }
  }
  if (ctx.residue.length) {
    problems.push(`RESIDUE from failed fixtures: ${ctx.residue.join(", ")}`);
    unretired.push(...ctx.residue);
  }

  if (ctx.rosterSeed) {
    const r = await putRoster(ctx.rosterSeed.prior);
    if (!r.ok) problems.push(`roster restore: ${r.error}`);
    else if (!sameRoster(r.dirs, ctx.rosterSeed.prior)) {
      problems.push(
        `roster restore read back ${r.dirs.join(", ") || "EMPTY"}, not the prior ` +
          `${ctx.rosterSeed.prior.join(", ") || "EMPTY"}`,
      );
    } else if (unretired.length) {
      // Done, but NOT clean: say which fixtures it was done around.
      problems.push(
        `roster restored to ${ctx.rosterSeed.prior.join(", ") || "EMPTY"} (read back) ` +
          `WITH RESIDUE — these fixtures were not confirmed retired first, so they may now ` +
          `sit under an account the restored roster no longer lists: ${unretired.join(", ")}`,
      );
    } else {
      done.push(
        `roster restored to ${ctx.rosterSeed.prior.join(", ") || "EMPTY"} (read back, ` +
          `after every fixture retired)`,
      );
    }
  }

  if (ctx.ownedCwd) {
    try {
      rmSync(ctx.ownedCwd, { recursive: true, force: true });
      done.push(`removed fixture cwd ${ctx.ownedCwd}`);
    } catch (e) {
      problems.push(`could not remove fixture cwd ${ctx.ownedCwd}: ${e?.message ?? e}`);
    }
  }

  if (problems.length) fail("T0.6", what, [...problems, ...done].join("\n"));
  else pass("T0.6", what, done.join("; "));
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

  const ctx = {
    opts,
    build: String(buildId),
    gates: { empty: false, notSelfHosted: false },
    oracle: null,
    // Every terminal this run POSTed, and residue no cleanup could retire.
    spawnedTerminals: new Set(),
    residue: [],
    rosterSeed: null,
    fixtureCwd: null,
    ownedCwd: null,
  };

  try {
    await runT0(ctx);
    await runDropdownAssertions(ctx);
    await runRestoreAssertions(ctx);
    await runHonestyAssertions(ctx);
  } finally {
    await teardown(ctx);
  }

  return report(opts);
}

main()
  .then((code) => process.exit(code))
  .catch((err) => {
    console.error(`\nFATAL: the harness itself threw — ${err?.stack ?? err}`);
    process.exit(3);
  });
