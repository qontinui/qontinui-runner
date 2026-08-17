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

// ===========================================================================
// Configuration
// ===========================================================================

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
    port: Number(process.env.RUNNER_PORT || 9876),
    allowRestart: false,
    fixture: false,
    readOnly: false,
    strict: false,
    accounts: [],
    devStart: process.env.QONTINUI_DEV_START || "C:/qontinui-root/dev-start.ps1",
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
      case "--dev-start":
        [v, i] = take(i, "--dev-start");
        opts.devStart = v;
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
    const kind = err?.name === "TimeoutError" ? `timeout after ${timeoutMs}ms` : String(err?.message ?? err);
    return { ok: false, status: 0, json: null, text: "", error: kind, ms: Date.now() - started, url };
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
  if (!terms.ok) return { empty: false, evaluable: false, detail: `GET /terminals: ${terms.error}` };
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
 * The §0.1 self-hosting gate: is THIS process's own Claude session hosted in a
 * terminal of the runner it is about to restart?
 *
 * The only reliable check is looking for our own `CLAUDE_CODE_SESSION_ID` in
 * `GET /control/sessions/restore-health`. Checking for `QONTINUI_*` env vars is
 * NOT a substitute: a `claude` started inside a runner PTY without the runner's
 * env injection registers as `origin: "observed"` and carries none of them, yet
 * is every bit as runner-hosted — killing the runner still kills it mid-test.
 *
 * Fails CLOSED: no `CLAUDE_CODE_SESSION_ID` in the environment means we cannot
 * prove we are outside, which is not the same as being outside.
 */
async function measureSelfHosting(health) {
  const own = (process.env.CLAUDE_CODE_SESSION_ID || "").trim();
  if (!own) {
    return {
      safe: false,
      evaluable: false,
      detail:
        "CLAUDE_CODE_SESSION_ID is unset — cannot prove this process is outside the runner. " +
        "Failing closed: absence of the id is not evidence of being outside it.",
    };
  }
  const h = health ?? (await fetchRestoreHealth());
  if (!h.ok) return { safe: false, evaluable: false, detail: `restore-health: ${h.error}` };
  const hit = h.sessions.find((s) => String(s.claudeSessionId).trim() === own);
  if (hit) {
    return {
      safe: false,
      evaluable: true,
      detail:
        `SELF-HOSTED: own CLAUDE_CODE_SESSION_ID ${own} is bound to runner terminal ` +
        `${hit.terminalId} (origin: ${hit.origin ?? "?"}). Restarting this runner would kill ` +
        `the test process mid-run (plan §0.1).`,
    };
  }
  return {
    safe: true,
    evaluable: true,
    detail: `own session ${own} is not among the runner's ${h.sessions.length} record(s)`,
  };
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

/**
 * Spawn one fixture session through `POST /sessions/spawn` and wait for it to
 * appear in the durable registry.
 *
 * `/sessions/spawn` answers with a `taskRunId`, not a terminal id, so the new
 * session is discovered by DIFFING `/control/sessions/info` against the set of
 * ids seen before the spawn. That is also the honest way round: it proves the
 * session reached the durable registry, which is what every later assertion
 * reads.
 */
async function spawnFixtureSession({ taskName, account, prompt }, knownIds) {
  const body = { task_name: taskName, prompt };
  if (account) body.account = account;
  const spawned = await getJson("/sessions/spawn", { method: "POST", body });
  if (!spawned.ok) return { ok: false, error: `POST /sessions/spawn: ${spawned.error}` };

  const deadline = Date.now() + FIXTURE_APPEAR_BUDGET_MS;
  while (Date.now() < deadline) {
    const info = await fetchSessionsInfo();
    if (info.ok && info.status === "ok") {
      const fresh = info.sessions
        .filter((s) => s.available && s.identity)
        .find((s) => !knownIds.has(s.identity.claudeSessionId));
      if (fresh) return { ok: true, session: fresh };
    }
    await sleep(HEALTH_POLL_INTERVAL_MS);
  }
  return {
    ok: false,
    error: `spawned '${taskName}' but no new session appeared in /control/sessions/info within ${FIXTURE_APPEAR_BUDGET_MS}ms`,
  };
}

async function createFixtures(ctx) {
  const { opts } = ctx;
  const accounts = opts.accounts;
  if (accounts.length < 2) {
    skip(
      "T0.2",
      "three fixture sessions across >=2 Claude accounts, in distinct zones",
      "--fixture needs --accounts with at least two names; the roster is machine-global and this script will not guess account names",
    );
    skip("T0.3", "a trivial prompt in each fixture session (transcript + confirmed_at)", "no fixtures");
    skip("T0.4", "one operator-renamed session and one Claude-derived one", "no fixtures");
    return;
  }

  const before = await fetchSessionsInfo();
  const known = new Set(
    (before.sessions || []).filter((s) => s.identity).map((s) => s.identity.claudeSessionId),
  );

  const specs = [
    { taskName: "uibridge-fixture-a", account: accounts[0], prompt: "echo fixture-a" },
    { taskName: "uibridge-fixture-b", account: accounts[1], prompt: "echo fixture-b" },
    { taskName: "uibridge-fixture-c", account: accounts[0], prompt: "echo fixture-c" },
  ];
  const created = [];
  const problems = [];
  for (const spec of specs) {
    const r = await spawnFixtureSession(spec, known);
    if (!r.ok) {
      problems.push(r.error);
      continue;
    }
    known.add(r.session.identity.claudeSessionId);
    created.push({ spec, session: r.session });
  }
  ctx.fixtures = created;

  const zones = new Set(created.map((c) => c.session.placement?.zoneIndex));
  const accountLabels = new Set(created.map((c) => c.session.account?.label).filter(Boolean));
  if (problems.length || created.length !== 3) {
    fail(
      "T0.2",
      "three fixture sessions across >=2 Claude accounts, in distinct zones",
      [`created ${created.length}/3`, ...problems].join("\n"),
    );
  } else if (zones.size !== 3) {
    fail("T0.2", "three fixture sessions across >=2 Claude accounts, in distinct zones", `zone indices were ${[...zones].join(", ")} — not three distinct zones`);
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
  const promptFindings = [];
  for (const c of created) {
    const tid = c.session.identity.terminalId;
    const r = await getJson(`/terminals/${encodeURIComponent(tid)}/submit-prompt`, {
      method: "POST",
      body: { message: "echo uibridge-fixture-ready" },
    });
    promptFindings.push({
      ok: r.ok,
      detail: r.ok ? "" : `submit-prompt to ${tid}: ${r.error}`,
    });
  }
  assertAll("T0.3", "a trivial prompt in each fixture session (transcript + confirmed_at)", promptFindings);

  // Step 4 — one operator `/rename`, one left Claude-derived, so BOTH branches
  // of the name-provenance split are covered rather than only the default one.
  if (created.length === 0) {
    skip("T0.4", "one operator-renamed session and one Claude-derived one", "no fixtures were created");
    return;
  }
  const renameTarget = created[0];
  const r = await getJson(
    `/terminals/${encodeURIComponent(renameTarget.session.identity.terminalId)}/submit-prompt`,
    { method: "POST", body: { message: "/rename uibridge-fixture-operator-named" } },
  );
  if (!r.ok) {
    fail("T0.4", "one operator-renamed session and one Claude-derived one", `/rename submit failed: ${r.error}`);
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
    return { ok: false, detail: `${label}: data-session-info-field='${fieldAttr.value}', expected '${field}'` };
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
    return { ok: false, detail: `${label}: carries a value AND data-session-info-unknown="true" — contradictory` };
  }
  return { ok: true, detail: "" };
}

/**
 * T3/T4/T5 as one pass, parameterised by phase so T9 can re-run the identical
 * assertions against the restarted runner rather than a near-copy of them.
 */
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
      t3.push({ ok: false, detail: `zone ${zone.zoneIndex}: click ${triggerId} failed — ${click.error}` });
      continue;
    }
    await sleep(PANEL_SETTLE_MS);
    const reg = await fetchElements();
    if (!reg.ok) {
      t3.push({ ok: false, detail: `zone ${zone.zoneIndex}: elements re-read failed — ${reg.error}` });
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
    for (const field of INFO_FIELDS) {
      const id = infoElementId(field, zone.zoneIndex);
      const el = await fetchElement(id);
      if (!el.ok) {
        t4.push({ ok: false, detail: `zone ${zone.zoneIndex}: GET element ${id} — ${el.error}` });
        continue;
      }
      t4.push(compareRow(el.element, field, backendValueForField(session, field), `zone ${zone.zoneIndex} ${field}`));
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
      t5.push({
        ok: false,
        detail: `all opened sessions report the same account label(s): ${labels.join(", ")} — session A's account must differ from session B's`,
      });
    }
    const sources = projections.map((p) => p.name?.source ?? "unknown");
    if (!sources.includes("operator")) {
      t5.push({ ok: false, detail: `no opened session reports name.source='operator' (saw: ${sources.join(", ")})` });
    }
    if (!sources.includes("derived")) {
      t5.push({ ok: false, detail: `no opened session reports name.source='derived' (saw: ${sources.join(", ")})` });
    }
    if (t5.length === 0) t5.push({ ok: true, detail: "" });
    assertAll(ids.t5, `account and name are correct and distinct across sessions${label}`, t5);
  }
  return { info };
}

async function runDropdownAssertions(ctx) {
  section("T1-T5 — dropdown");

  const info = await fetchSessionsInfo();
  const z = await collectZones();

  // --- T1 --------------------------------------------------------------
  if (!z.ok) {
    fail("T1", "each zone header is discoverable and bound to the expected session", `GET /ui-bridge/control/elements: ${z.error}`);
  } else if (z.zones.length === 0) {
    skip("T1", "each zone header is discoverable and bound to the expected session", "the UI Bridge registry contains no terminal-zone-header-<n> element");
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
      const found = await findElements({ text: zone.headerId });
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
      if (ctx.fixtures && !ctx.fixtures.some((f) => f.session.identity.claudeSessionId === zone.claudeSessionId)) {
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
    skip("T2", "the info trigger exists for every session-bound zone and does NOT self-hide", `elements read failed: ${z.error}`);
  } else if (bound.length === 0) {
    skip("T2", "the info trigger exists for every session-bound zone and does NOT self-hide", "no zone header carries a data-claude-session-id");
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
    assertAll("T2", "the info trigger exists for every session-bound zone and does NOT self-hide", findings);
  }

  // --- T0.5 (PR arm honesty) + T3/T4/T5 -----------------------------------
  const withPrs = info.ok
    ? info.sessions.filter((s) => s.available && s.prs?.status === "ok" && s.prs.openCount > 0)
    : [];
  const degradedPrs = info.ok
    ? info.sessions.filter((s) => s.available && s.prs?.status && s.prs.status !== "ok")
    : [];
  if (!info.ok) {
    skip("T0.5", "either a populated PR ledger OR the prs.status/reason path is exercised", `/control/sessions/info: ${info.error}`);
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
    return { ok: false, detail: `no process named '${RUNNER_PROCESS_NAME}' is running — nothing to crash` };
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
    if (!k.ok) return { ok: false, detail: `Stop-Process -Id ${pid} failed: ${k.stderr || k.error}` };
    killed.push(pid);
  }
  return { ok: true, detail: `stopped runner pid(s) ${killed.join(", ")} (no tree flag, no node/powershell touched)` };
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
    fail("T6", "the pre-restart census is complete, and is saved as the oracle", `GET /control/sessions/restore-census: ${c.error}`);
  } else {
    const census = c.census ?? {};
    const expected = Array.isArray(census.expected) ? census.expected : [];
    const problems = [];
    if (census.status !== "ok") {
      problems.push(`census status='${census.status}' reason='${census.reason ?? "<none>"}'`);
    }
    if (ctx.fixtures) {
      const fixtureIds = new Set(ctx.fixtures.map((f) => f.session.identity.claudeSessionId));
      const expectedIds = new Set(expected.map((e) => e.claudeSessionId));
      const missing = [...fixtureIds].filter((id) => !expectedIds.has(id));
      const extra = [...expectedIds].filter((id) => !fixtureIds.has(id));
      if (missing.length) problems.push(`expected[] omits fixture session(s): ${missing.join(", ")}`);
      if (extra.length) problems.push(`expected[] carries non-fixture session(s): ${extra.join(", ")}`);
      const unrestorable = expected.filter((e) => e.restorable !== true);
      if (unrestorable.length) {
        problems.push(`expected[] rows not restorable: ${unrestorable.map((e) => e.claudeSessionId).join(", ")}`);
      }
    } else if (expected.length === 0) {
      problems.push(
        "expected[] is empty and no fixtures were created, so there is nothing to compare a restore against — " +
          "this is a genuine UNKNOWN, not a completed census",
      );
    }
    if (problems.length === 0) {
      try {
        mkdirSync(dirname(opts.oracle), { recursive: true });
        writeFileSync(opts.oracle, JSON.stringify({ savedAt: Date.now(), census }, null, 2));
        ctx.oracle = census;
        pass(
          "T6",
          "the pre-restart census is complete, and is saved as the oracle",
          `expected=${expected.length}, cleanShutdown=${census.cleanShutdown}, verdict=${census.verdict}; oracle -> ${opts.oracle}`,
        );
      } catch (e) {
        fail("T6", "the pre-restart census is complete, and is saved as the oracle", `could not write the oracle to ${opts.oracle}: ${e?.message ?? e}`);
      }
    } else {
      fail("T6", "the pre-restart census is complete, and is saved as the oracle", problems.join("\n"));
    }
  }

  // --- T7: the restart -----------------------------------------------------
  if (!opts.allowRestart) {
    const why =
      "--allow-restart is OFF by default. The plan's §0 restart authorization is conditional on an empty " +
      "runner the implementer itself emptied, and §0.1 measured both premises FALSE; the default-off flag is " +
      "that condition made mechanical.";
    skip("T7", "the runner restarts cleanly (clean shutdown marker, then healthy)", why);
    skip("T8", "the correct sessions were restored — verdict match, oracle set equality, placement held", why + " T7 never ran, so there is no post-restart state.");
    skipT9Pass(why + " T7 never ran.");
    skip("T9.tier", "restore-tier is populated and honest ('resumed' implies transcriptExists)", why + " T7 never ran.");
    return;
  }

  const gate = await gatesAllowMutation("T7 pre-restart");
  if (!gate.ok) {
    fail("T7", "the runner restarts cleanly (clean shutdown marker, then healthy)", `ABORTED — ${gate.detail}`);
    skip("T8", "the correct sessions were restored — verdict match, oracle set equality, placement held", "T7 aborted at its gate re-check");
    skipT9Pass("T7 aborted at its gate re-check");
    skip("T9.tier", "restore-tier is populated and honest ('resumed' implies transcriptExists)", "T7 aborted at its gate re-check");
    return;
  }

  const stop = powershell(["-File", opts.devStart, "-StopRunner"]);
  if (!stop.ok) {
    fail("T7", "the runner restarts cleanly (clean shutdown marker, then healthy)", `dev-start.ps1 -StopRunner failed (${stop.code}): ${stop.stderr || stop.error}`);
    skip("T8", "the correct sessions were restored — verdict match, oracle set equality, placement held", "the restart did not complete");
    skipT9Pass("the restart did not complete");
    skip("T9.tier", "restore-tier is populated and honest ('resumed' implies transcriptExists)", "the restart did not complete");
    return;
  }
  const start = powershell(["-File", opts.devStart, "-Runner"]);
  const healthy = await waitForHealthy();
  if (!healthy.ok) {
    fail("T7", "the runner restarts cleanly (clean shutdown marker, then healthy)", `${healthy.detail}\n-Runner exit ${start.code}: ${start.stderr || "(no stderr)"}`);
    skip("T8", "the correct sessions were restored — verdict match, oracle set equality, placement held", "the runner never came back healthy");
    skipT9Pass("the runner never came back healthy");
    skip("T9.tier", "restore-tier is populated and honest ('resumed' implies transcriptExists)", "the runner never came back healthy");
    return;
  }
  // "A clean shutdown marker was written" is asserted through the census's own
  // report of the boundary it read, rather than by guessing the marker's path.
  const post = await fetchCensus();
  const clean = post.ok && post.census?.cleanShutdown === true && post.census?.shutdownAt != null;
  if (clean) {
    pass("T7", "the runner restarts cleanly (clean shutdown marker, then healthy)", `${healthy.detail}; cleanShutdown=true, shutdownAt=${post.census.shutdownAt}`);
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
    skip("T8", "the correct sessions were restored — verdict match, oracle set equality, placement held", "T6 never saved an oracle");
  } else if (!post.ok) {
    fail("T8", "the correct sessions were restored — verdict match, oracle set equality, placement held", `GET /control/sessions/restore-census: ${post.error}`);
  } else {
    const census = post.census ?? {};
    const problems = [];
    if (census.verdict !== "match") {
      problems.push(`verdict='${census.verdict}' (reason='${census.reason ?? "<none>"}'), expected 'match'`);
    }
    const missing = Array.isArray(census.missing) ? census.missing : [];
    const unexpected = Array.isArray(census.unexpected) ? census.unexpected : [];
    if (missing.length) problems.push(`missing[]: ${missing.map((m) => `${m.claudeSessionId}(${m.reason})`).join(", ")}`);
    if (unexpected.length) {
      problems.push(`unexpected[]: ${unexpected.map((u) => `${u.claudeSessionId}(${u.reason})`).join(", ")}`);
    }
    const oracleIds = new Set((ctx.oracle.expected || []).map((e) => e.claudeSessionId));
    const restoredIds = new Set((census.restored || []).map((r) => r.claudeSessionId));
    const notBack = [...oracleIds].filter((id) => !restoredIds.has(id));
    const surprise = [...restoredIds].filter((id) => !oracleIds.has(id));
    if (notBack.length) problems.push(`in the oracle's expected set but not restored: ${notBack.join(", ")}`);
    if (surprise.length) problems.push(`restored but absent from the oracle's expected set: ${surprise.join(", ")}`);
    // Placement: zoneIndex AND pageId must survive the restart.
    const byId = new Map((ctx.oracle.expected || []).map((e) => [e.claudeSessionId, e]));
    const info = await fetchSessionsInfo();
    for (const r of census.restored || []) {
      const was = byId.get(r.claudeSessionId);
      if (!was) continue;
      if (Number(r.zoneIndex) !== Number(was.zoneIndex)) {
        problems.push(`${r.claudeSessionId}: zoneIndex ${was.zoneIndex} -> ${r.zoneIndex}`);
      }
      const now = info.ok ? info.sessions.find((s) => s.identity?.claudeSessionId === r.claudeSessionId) : null;
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
      fail("T8", "the correct sessions were restored — verdict match, oracle set equality, placement held", problems.join("\n"));
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
    const restored = info2.sessions.filter((s) => s.available && s.lifecycle?.restoredFromBootAt != null);
    if (restored.length === 0) {
      skip("T9.tier", "restore-tier is populated and honest ('resumed' implies transcriptExists)", "no session carries a restoredFromBootAt stamp after the restart");
    } else {
      const findings = restored.map((s) => {
        const id = s.identity.claudeSessionId;
        const tier = s.lifecycle.restoreTier;
        const h = rh.sessions.find((x) => x.claudeSessionId === id);
        if (!tier) return { ok: false, detail: `${id}: restoredFromBootAt is set but restoreTier is null — the tier must be populated` };
        if (tier === "resumed" && h && h.transcriptExists !== true) {
          return { ok: false, detail: `${id}: claims restoreTier='resumed' but restore-health reports transcriptExists=false` };
        }
        if (tier === "terminal-only" && s.lifecycle.restoreTier === "resumed") {
          return { ok: false, detail: `${id}: a terminal-only restore must not claim 'resumed'` };
        }
        return { ok: true, detail: "" };
      });
      assertAll("T9.tier", "restore-tier is populated and honest ('resumed' implies transcriptExists)", findings);
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
  if (!repoDir) return { evaluable: false, detail: "--repo-dir not given, so the head-object precondition could not be checked" };
  if (allowFetch) {
    const f = powershell(["-Command", `git -C '${repoDir}' fetch --quiet origin ${base}`], { timeoutMs: 180_000 });
    if (!f.ok) return { evaluable: false, detail: `git fetch origin ${base} failed: ${f.stderr || f.error}` };
  }
  const baseRef = powershell([
    "-Command",
    `git -C '${repoDir}' rev-parse --verify --quiet 'origin/${base}^{commit}'`,
  ]);
  if (!baseRef.stdout) {
    return { evaluable: false, detail: `origin/${base} does not resolve in ${repoDir} (no_base_ref)` };
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
      fail("T10", "a hard-crash boot reports verdict 'unknown', not 'match'", `ABORTED — ${gate.detail}`);
    } else {
      const k = killRunnerProcessOnly();
      if (!k.ok) {
        fail("T10", "a hard-crash boot reports verdict 'unknown', not 'match'", k.detail);
      } else {
        const start = powershell(["-File", opts.devStart, "-Runner"]);
        const healthy = await waitForHealthy();
        if (!healthy.ok) {
          fail("T10", "a hard-crash boot reports verdict 'unknown', not 'match'", `${k.detail}\n${healthy.detail}\n-Runner exit ${start.code}: ${start.stderr || "(no stderr)"}`);
        } else {
          const c = await fetchCensus();
          if (!c.ok) {
            fail("T10", "a hard-crash boot reports verdict 'unknown', not 'match'", `census unreadable after the crash boot: ${c.error}`);
          } else if (c.census?.verdict === "unknown" && c.census?.reason) {
            pass("T10", "a hard-crash boot reports verdict 'unknown', not 'match'", `${k.detail}; verdict='unknown' reason='${c.census.reason}' cleanShutdown=${c.census.cleanShutdown}`);
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
    skip("T11b", "an unevaluable land signal reports 'land-unknown' WITH a reason, never a confident not-landed", why);
    return;
  }
  if (info.status !== "ok") {
    const why = `/control/sessions/info reported status='${info.status}' reason='${info.reason}'`;
    skip("T11", "an ff-landed PR is reported LANDED with landSignal 'ff-land'", why);
    skip("T11b", "an unevaluable land signal reports 'land-unknown' WITH a reason, never a confident not-landed", why);
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
        return { ok: false, detail: `${key}: appears in BOTH prs.landed and prs.unknown — the verdict cannot be both proved and unevaluable` };
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
  console.log(`  timeouts      request ${REQUEST_TIMEOUT_MS}ms, health poll ${HEALTH_POLL_BUDGET_MS}ms`);

  const health = await http("/health", { timeoutMs: HEALTH_PROBE_TIMEOUT_MS });
  if (!health.ok) {
    console.error(`\nFATAL: the runner did not answer GET ${BASE}/health — ${health.error}`);
    console.error("Nothing below could be evaluated, so nothing is reported as passing.");
    return 3;
  }
  const buildId = health.json?.buildId ?? health.json?.data?.buildId ?? "unknown";
  const behind = health.json?.data?.buildDrift?.commitsBehind;
  console.log(`  runner build  ${buildId}${behind === undefined ? "" : ` (${behind} commits behind origin/main)`}`);
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
