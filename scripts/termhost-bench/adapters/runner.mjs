/**
 * The `runner` setup adapter (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * Phase 3, D4, D12, D13). The runner under test is ALWAYS an ephemeral instance
 * with a proven build — never the primary on :9876, which is neither driven,
 * restarted nor rebuilt (served policy `production-and-cost` `runner-lifecycle`).
 *
 * Two launch modes (D12):
 *
 *   supervisor   a box with a supervisor: `POST :9875/runners/spawn-test` with
 *                `rebuild: true, frontend_strict: true, async: true` and exactly one
 *                of `git_ref: <sha>` / `worktree_path: <checkout>`; the build is
 *                polled (`GET /build/{id}/status`), its `source` must read
 *                `worktree` / `worktree_path`, and its `build_sha` must equal the
 *                requested head — an absent sha makes the build `unknown` (row label
 *                `unknown_build`, never a baseline). Linux passes the harness's Xvfb
 *                display in `extra_env.DISPLAY`. Teardown: `POST /runners/{id}/stop`,
 *                read back through `GET /runners`.
 *   subject      a box with NO supervisor (GitHub-hosted CI): the built exe under
 *                `QONTINUI_INSTANCE_ROOT=<unit>/runner-root` on a free port, named
 *                `termhost-<run>`, `QONTINUI_ALLOW_NO_DB=1` — everything the runner
 *                owns lives under the root (`src-tauri/src/instance_env.rs`). Refused
 *                whenever :9875 answers or anything listens on :9876, so it can never
 *                become the hand-rolled launcher a dev box forbids. A refused start
 *                is read from `<root>/startup-refusal.log` (exit 78). Readiness is
 *                `/health` `uiBridgeIpcObserved: true` (`scripts/contract-smoke.ps1`).
 *
 * Before either spawn the build must contain the #1756 fix commits `5e24b941c` and
 * `90cc00b1d` (a child built without them renames the primary's `paired_user.json`).
 *
 * Driving the Terminal page — the operator's path, through the UI Bridge:
 *   - `POST /ui-bridge/control/tab/activate {tabId: "terminal"}`;
 *   - sessions through the launch menu's own `create-with-command {count, command}`
 *     (`terminalLaunchMenuActions.ts`), in chunks of at most 24 (`MAX_SPAWN_COUNT`,
 *     `useTerminalCommands.ts`) — never `POST /terminals {page_id}`, which creates
 *     terminals on an inactive page where they never mount;
 *   - spawn ADMISSION: a CRITICAL resource-guard refusal (`resource_guard:critical:`)
 *     opens the runner's blocking "Start anyway?" dialog and holds the action open.
 *     The harness detects it (the runner log's refusal line, then the dialog's own
 *     buttons through `discover`), answers it with the DECLINE button only — never
 *     "Start anyway", because overriding a machine owner's floor is not
 *     state-neutral — and records `admission_refused {lane, n_reached,
 *     threadCeilings}`, which ends this setup's ladder;
 *   - `focus`: `maximize-zone` on the zone where the probe SAW the session's marker
 *     (row-major marker order, mapped through the `terminal-zone-header-<i>` boxes
 *     when they line up), verified by the probe then seeing only that session —
 *     otherwise `unknown(zone_unresolved)`; `show(k)`: `restore-zone`, then
 *     `maximize-zone` on k's zone; `tiled`: whatever auto-grow chose, its layout id
 *     read from `get-zone-view-state`;
 *   - `focusObserved()`: `focus` on `terminal-input-<id>`; a refusal
 *     (`TERMINAL_NO_MOUNTED_VIEW`) makes the input arm `unknown(pane_not_mounted)`;
 *   - `closeOne`: `DELETE /terminals/{id}`. `GET /terminals/{id}/buffer` is never
 *     called during a soak (it resets flow control); the post-soak fidelity read is
 *     `GET /ui-bridge/sdk/terminal/sessions/{id}/text`.
 *
 * A coord device drain gates only `POST /terminals`, not the UI path, so the
 * adapter reads the drain itself (`GET /restart-readiness` `drain.coordDrain`) and
 * refuses a `drained` or `unknown` device (`unknown(device_drained)`); an unpaired
 * subject reads `not_enrolled`, which is allowed.
 */

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import net from "node:net";
import { homedir } from "node:os";
import path, { basename, dirname, join } from "node:path";

import { HttpError, PRIMARY_RUNNER_PORT, RunnerClient, RunnerNotListed, SupervisorClient, httpJson, sleep, unwrap } from "../../lib/runner-clients.mjs";
import { ECHO_REGION, MARKER_KINDS, markerOverlay, markerSeq } from "../marker.mjs";
import { platformDirs } from "../neutrality.mjs";
import { listProcesses, startKeyCompare, terminatePid, terminateTree } from "../proc.mjs";
import { assertInside, commandLine } from "../rundir.mjs";
import { unprovenBuild } from "../results.mjs";
import { percentile } from "../stats.mjs";
import { AdapterRefusal, rowMajorOrder } from "./index.mjs";

/** The #1756 fix commits a build under test must contain (D12). */
export const REQUIRED_FIX_COMMITS = Object.freeze(["5e24b941c", "90cc00b1d"]);

/** One `create-with-command` call creates at most this many sessions (`MAX_SPAWN_COUNT`). */
export const MAX_SPAWN_COUNT = 24;

/** The UI Bridge components and actions the adapter drives. */
export const LAUNCH_MENU = "terminal-launch-menu";
export const TERMINAL_PAGE = "terminal-page";
/** How long the zone layout is given to repaint after a restore-zone before it is looked at. */
export const ZONE_SETTLE_MS = 500;

/** The resource-guard dialog's decline labels (`resourceGuardCopy.ts`), and what is never clicked. */
export const DECLINE_LABELS = Object.freeze(["Don't start", "Start none"]);
export const NEVER_CLICK = /start\s+(all\s+)?anyway/i;

/** The runner log line a CRITICAL spawn refusal writes (`resource_guard.rs`). */
export const REFUSAL_LOG = /resource_guard: refusing|resource_guard:critical:/;

/** The supervisor's default URL. */
export const SUPERVISOR_URL = "http://127.0.0.1:9875";

const hrtime = () => process.hrtime.bigint();

/**
 * The runner's documented `/health` tail on a loaded box is about 10 s (CLAUDE.md
 * "Service Architecture & Ports"), so a probe that must not mistake a slow server
 * for an absent one waits longer than that.
 */
export const PROBE_TIMEOUT_MS = 12000;

/**
 * Does something accept TCP connections on 127.0.0.1:`port`? Three answers: true (a
 * connection was accepted), false (refused, ECONNREFUSED: nothing listens), null
 * (neither within `timeoutMs`, or another error: a full accept backlog, a firewall).
 * A caller that refuses to run beside a live primary treats null as "may be there".
 *
 * @returns {Promise<boolean | null>}
 */
export function isPortListening(port, { timeoutMs = PROBE_TIMEOUT_MS, connect = net.connect } = {}) {
  return new Promise((resolve) => {
    let settled = false;
    const sock = connect({ host: "127.0.0.1", port });
    const done = (v) => {
      if (settled) return;
      settled = true;
      sock.destroy();
      resolve(v);
    };
    sock.setTimeout?.(timeoutMs, () => done(null));
    sock.once("connect", () => done(true));
    sock.once("error", (e) => done(e?.code === "ECONNREFUSED" ? false : null));
  });
}

/** A free TCP port on 127.0.0.1 at or above `from`, never the primary's. */
export async function freePort(from = 9877, { tries = 200, listening = isPortListening } = {}) {
  for (let p = from; p < from + tries; p += 1) {
    if (p === PRIMARY_RUNNER_PORT) continue;
    // Only a bind decides: a port whose probe is refused or unanswered is tried.
    if ((await listening(p, { timeoutMs: 300 })) !== true) {
      const free = await new Promise((resolve) => {
        const srv = net.createServer();
        srv.once("error", () => resolve(false));
        srv.listen(p, "127.0.0.1", () => srv.close(() => resolve(true)));
      });
      if (free) return p;
    }
  }
  throw new Error(`no free port in ${from}..${from + tries - 1}`);
}

/**
 * Does a supervisor answer at `url`? Three answers: true (any HTTP answer to
 * `/health`: a supervisor is there, healthy or not), false (connection refused:
 * nothing listens), null (no answer within `timeoutMs`, or another failure: a slow
 * or wedged supervisor is not an absent one). D12 lets a hand-rolled subject runner
 * start only on a definite false.
 *
 * @returns {Promise<boolean | null>}
 */
export async function supervisorAnswers(url = SUPERVISOR_URL, { fetch, timeoutMs = PROBE_TIMEOUT_MS } = {}) {
  try {
    await httpJson(`${url.replace(/\/+$/, "")}/health`, { timeoutMs, ...(fetch ? { fetch } : {}) });
    return true;
  } catch (e) {
    if (e instanceof HttpError) return true;
    const code = e?.cause?.code ?? e?.code;
    return code === "ECONNREFUSED" ? false : null;
  }
}

/** The subject-launch verdict from the two probes (D12): only "nothing there" passes. */
export function subjectLaunchVerdict(supervisor, primary) {
  if (supervisor === false && primary === false) {
    return { state: "OPERATIVE", reason: "subject_instance_root: no supervisor on :9875 and nothing on :9876" };
  }
  if (supervisor === true || primary === true) {
    return {
      state: "INOPERATIVE-ON-THIS-MACHINE",
      reason: `subject launch refused: ${supervisor === true ? "a supervisor answers on :9875" : "something listens on :9876"}; use supervisor mode (D12)`,
    };
  }
  return {
    state: "UNKNOWN",
    reason: `subject launch refused: ${supervisor === null ? "whether a supervisor answers on :9875" : "whether anything listens on :9876"} could not be told in time; an unknown is never read as absent (D12)`,
  };
}

/**
 * Is every #1756 fix commit an ancestor of `commit`? `git merge-base --is-ancestor`:
 * exit 0 = ancestor, 1 = not, anything else (unknown object) = UNKNOWN.
 *
 * @returns {{state: "OPERATIVE" | "INOPERATIVE-ON-THIS-MACHINE" | "UNKNOWN", reason: string}}
 */
export function checkFixCommits(commit, { repoRoot, run = spawnSync } = {}) {
  if (!commit) return { state: "UNKNOWN", reason: "the build's commit is not known, so the #1756 fixes cannot be checked" };
  for (const fix of REQUIRED_FIX_COMMITS) {
    const res = run("git", ["-C", repoRoot, "merge-base", "--is-ancestor", fix, commit], { encoding: "utf8", timeout: 30000 });
    if (res.status === 1) {
      return {
        state: "INOPERATIVE-ON-THIS-MACHINE",
        reason: `build ${commit.slice(0, 12)} lacks #1756 fix ${fix}: a child built without it renames the primary's paired_user.json (D12)`,
      };
    }
    if (res.status !== 0) {
      return {
        state: "UNKNOWN",
        reason: `git merge-base --is-ancestor ${fix} ${commit.slice(0, 12)} answered ${res.status ?? res.error?.message}: ${String(res.stderr ?? "").trim().slice(0, 200)}`,
      };
    }
  }
  return { state: "OPERATIVE", reason: `build ${commit.slice(0, 12)} contains ${REQUIRED_FIX_COMMITS.join(" and ")}` };
}

/**
 * The subject runner's environment: the harness's own, minus anything that would
 * point the runner outside its instance root (the runner refuses such overrides
 * with exit 78) or tie it to the harness's sessions, plus the D12 launch contract.
 *
 * On Linux the subject's XDG directories are its own, but they sit BESIDE the
 * root (`<unit>/runner-xdg/…`), never inside it: `dirs::config_dir` and
 * `dirs::data_local_dir` resolve from XDG_CONFIG_HOME / XDG_DATA_HOME, and the
 * runner refuses (exit 78) a root that CONTAINS a machine-global directory
 * (`instance_env.rs` `machine_global_dirs`, `root_problem`). Putting them inside
 * the root is exactly what the first CI run hit.
 *
 * @param {{base: object, root: string, port: number, instanceName: string,
 *   platform?: string, display?: string | null, extra?: object, xdgDir?: string}} o
 */
export function subjectEnv({ base, root, port, instanceName, platform = process.platform, display = null, extra = {}, xdgDir = null }) {
  const env = {};
  for (const [k, v] of Object.entries(base)) {
    if (/^QONTINUI_.*_DIR$/.test(k)) continue;
    if (
      [
        "QONTINUI_HOME",
        "WEBVIEW2_USER_DATA_FOLDER",
        "QONTINUI_INSTANCE_ROOT",
        "QONTINUI_INSTANCE_NAME",
        "QONTINUI_PORT",
        "QONTINUI_PRIMARY_PORT",
        "QONTINUI_RUNNER_CONTEXT",
        "QONTINUI_TERMINAL_ID",
        "QONTINUI_PINNED_SESSION_ID",
        "CLAUDECODE",
        "CLAUDE_CODE_CHILD_SESSION",
      ].includes(k)
    ) {
      continue;
    }
    env[k] = v;
  }
  Object.assign(env, {
    QONTINUI_INSTANCE_ROOT: root,
    QONTINUI_PORT: String(port),
    QONTINUI_INSTANCE_NAME: instanceName,
    QONTINUI_PRIMARY_PORT: String(port),
    QONTINUI_ALLOW_NO_DB: "1",
    QONTINUI_SETUP_WIZARD_BYPASS: "1",
  });
  if (platform === "linux") {
    if (display) env.DISPLAY = display;
    // dirs::config_dir / data_local_dir / cache follow XDG on Linux: keep the
    // subject's WebKit storage, dev logs and caches out of the harness user's
    // home — beside the root, because the root may not contain them.
    const xdg = xdgDir ?? subjectXdgDir(root);
    env.XDG_CONFIG_HOME = join(xdg, "config");
    env.XDG_DATA_HOME = join(xdg, "data");
    env.XDG_CACHE_HOME = join(xdg, "cache");
    env.XDG_STATE_HOME = join(xdg, "state");
  }
  return Object.assign(env, extra);
}

/**
 * A UI Bridge component action's own return value. The runner's route answers
 * `ApiResponse{data}`, and `data` is the SDK's action envelope
 * (`executeComponentAction`: `{success, result, durationMs, …}`), so the handler's
 * return is `.result`. Read flat, `get-zone-view-state` had no `zoneCount` or
 * `layoutId` at all (CI run 38345475088 recorded both as null).
 */
export function actionResult(v) {
  if (v && typeof v === "object" && "success" in v && "result" in v) {
    if (v.success === false) throw new Error(`UI Bridge action failed: ${v.error ?? JSON.stringify(v).slice(0, 200)}`);
    return v.result;
  }
  return v;
}

/** Where a subject's own XDG directories live: beside its root, in the unit dir. */
export function subjectXdgDir(root) {
  return join(dirname(root), "runner-xdg");
}

/**
 * The location overrides the runner confines to its instance root
 * (`instance_env.rs` `ROOT_CONFINED_ENV_KEYS`; a test reads that list from the
 * source so the two cannot drift).
 */
export const ROOT_CONFINED_ENV_KEYS = Object.freeze([
  "QONTINUI_CAPABILITY_STATE_DIR",
  "QONTINUI_CONFIG_DIR",
  "QONTINUI_EMBEDDED_PG_DIR",
  "QONTINUI_HOME",
  "QONTINUI_PANIC_LOG_DIR",
  "QONTINUI_PROMPTS_DIR",
  "QONTINUI_RUNNER_LOG_DIR",
  "QONTINUI_SECURE_STORAGE_DIR",
  "QONTINUI_SESSION_NAMES_DIR",
  "WEBVIEW2_USER_DATA_FOLDER",
]);

/**
 * The runner's startup contract for a subject (`instance_env.rs`
 * `validate_instance_root` + `root_problem`), judged on the environment the
 * adapter is about to pass — so a launch the runner would refuse with exit 78 is
 * refused here first, naming every problem. Lexical, like the Rust:
 *
 * - the root is absolute, has no `..`, is not a filesystem root, and contains
 *   none of the machine-global directories (`machine_global_dirs`: the data-local
 *   and config directories as `dirs` resolves them from THIS env, `~/.qontinui`
 *   and the home directory);
 * - QONTINUI_PORT is set, parses as a port, is not 0 and not the primary's;
 * - every non-blank ROOT_CONFINED_ENV_KEYS value is absolute, has no `..`, and
 *   lies inside the root.
 *
 * @param {object} env the subject's environment
 * @param {{platform?: string}} [o]
 * @returns {string[]} problems (empty = the runner accepts the launch)
 */
export function subjectContractProblems(env, { platform = process.platform } = {}) {
  const p = platform === "win32" ? path.win32 : path.posix;
  const fold = platform === "win32" || platform === "darwin";
  const norm = (x) => {
    const n = p.normalize(x);
    return fold ? n.toLowerCase() : n;
  };
  const hasDotDot = (x) => x.split(/[\\/]+/).includes("..");
  const inside = (child, parent) => {
    const rel = p.relative(norm(parent), norm(child));
    return rel === "" || (!rel.startsWith("..") && !p.isAbsolute(rel));
  };
  const root = env.QONTINUI_INSTANCE_ROOT;
  if (typeof root !== "string" || root === "") return ["QONTINUI_INSTANCE_ROOT is unset"];
  if (!p.isAbsolute(root)) return [`QONTINUI_INSTANCE_ROOT must be an absolute path, got ${JSON.stringify(root)}`];
  if (hasDotDot(root)) return [`QONTINUI_INSTANCE_ROOT must not contain \`..\`, got ${JSON.stringify(root)}`];
  if (norm(root) === norm(p.parse(root).root)) return [`QONTINUI_INSTANCE_ROOT ${JSON.stringify(root)} is a filesystem root`];
  const home = platform === "win32" ? env.USERPROFILE : env.HOME;
  if (typeof home === "string" && home !== "") {
    const { configDir, dataLocalDir } = platformDirs({ platform, env, home });
    for (const dir of [dataLocalDir, configDir, p.join(home, ".qontinui"), home]) {
      if (inside(dir, root)) return [`QONTINUI_INSTANCE_ROOT ${JSON.stringify(root)} contains the machine-global directory ${JSON.stringify(dir)}`];
    }
  }
  const problems = [];
  const port = env.QONTINUI_PORT;
  if (port === undefined || port === "") problems.push("QONTINUI_PORT is unset");
  else if (!/^\d+$/.test(port) || Number(port) > 65535) problems.push(`QONTINUI_PORT=${JSON.stringify(port)} is not a valid port`);
  else if (Number(port) === 0) problems.push("QONTINUI_PORT=0 is not a fixed port");
  else if (Number(port) === PRIMARY_RUNNER_PORT) problems.push(`QONTINUI_PORT=${port} is the primary runner's port`);
  for (const key of ROOT_CONFINED_ENV_KEYS) {
    const v = env[key];
    if (typeof v !== "string" || v.trim() === "") continue;
    if (!p.isAbsolute(v) || hasDotDot(v) || !inside(v, root)) problems.push(`${key}=${JSON.stringify(v)} is outside the instance root`);
  }
  return problems;
}

/**
 * The spawn-test body (D12): rebuild, strict frontend, async, and exactly one
 * provenance selector.
 */
export function spawnTestBody({ gitRef = null, worktreePath = null, requesterId, display = null, platform = process.platform }) {
  if (Boolean(gitRef) === Boolean(worktreePath)) {
    throw new RangeError("supervisor mode needs exactly one of a git ref (sha) or a worktree path");
  }
  const body = { rebuild: true, frontend_strict: true, async: true, wait: false, requester_id: requesterId };
  if (gitRef) body.git_ref = gitRef;
  else body.worktree_path = worktreePath;
  if (platform === "linux" && display) body.extra_env = { DISPLAY: display };
  return body;
}

/** Does a runner's reported sha (often the 12-char `--short=12`) name `commit`? */
export function shaMatches(reported, commit) {
  if (typeof reported !== "string" || typeof commit !== "string") return false;
  const r = reported.trim().toLowerCase();
  const c = commit.trim().toLowerCase();
  return /^[0-9a-f]{7,40}$/.test(r) && /^[0-9a-f]{7,40}$/.test(c) && (c.startsWith(r) || r.startsWith(c));
}

/**
 * Judge a spawn-test outcome (D12). What decides the build is the RUNNING child's
 * own sha — `git_sha`, which the supervisor probes from the child's `/health`
 * (qontinui-supervisor `src/routes/runners.rs`) — never the tree the build was
 * asked for (`git_ref_resolved_sha` is that worktree's HEAD). A last-known-good
 * fallback (`used_lkg`) is not the requested build and is refused; a response with
 * no probed sha is an unknown build (labelled `unknown_build`, never a baseline).
 *
 * @returns {{ok: boolean, detail: string, buildSha: string | null, port: number | null}}
 */
export function judgeSpawnOutcome(outcome, { gitRef, worktreePath, expectedCommit }) {
  const body = outcome?.body ?? {};
  if (outcome?.httpStatus !== 200) {
    return { ok: false, detail: `spawn-test answered ${outcome?.httpStatus}: ${JSON.stringify(body).slice(0, 400)}`, buildSha: null, port: null };
  }
  const wantSource = gitRef ? "worktree" : "worktree_path";
  if (body.source !== wantSource && !(gitRef && body.source === "origin_main" && /^origin\/main$/.test(gitRef))) {
    return { ok: false, detail: `spawn-test built source "${body.source}", expected "${wantSource}" (D12)`, buildSha: null, port: null };
  }
  if (body.used_lkg === true) {
    return { ok: false, detail: "spawn-test fell back to a last-known-good exe (used_lkg): not the requested build (D12)", buildSha: null, port: null };
  }
  const probed = typeof body.git_sha === "string" && /^[0-9a-f]{7,40}$/i.test(body.git_sha) ? body.git_sha.toLowerCase() : null;
  const port = Number.isInteger(body.port) ? body.port : (outcome.port ?? null);
  if (probed && expectedCommit && !shaMatches(probed, expectedCommit)) {
    return { ok: false, detail: `the running child reports git_sha ${probed}, not the requested ${expectedCommit} (D12)`, buildSha: null, port: null };
  }
  if (!probed || !expectedCommit || !/^[0-9a-f]{40}$/.test(expectedCommit)) {
    return { ok: true, detail: `built from ${body.source}; the running child's sha is UNKNOWN (never a baseline)`, buildSha: null, port };
  }
  return { ok: true, detail: `the running child reports ${probed}, the requested ${expectedCommit}, built from ${body.source}`, buildSha: expectedCommit, port };
}

/** `terminal_spawn.<span>` close times (ms) from runner log lines. */
export function parseSpawnSpans(text) {
  const unit = { ns: 1e-6, "µs": 1e-3, us: 1e-3, ms: 1, s: 1000 };
  const out = new Map();
  for (const line of String(text).split("\n")) {
    const m = /terminal_spawn\.([a-z_]+)[^\n]*?close time\.busy=([0-9.]+)(ns|µs|us|ms|s)\b/.exec(line);
    if (!m) continue;
    const ms = Number(m[2]) * unit[m[3]];
    if (!out.has(m[1])) out.set(m[1], []);
    out.get(m[1]).push(ms);
  }
  return out;
}

/** The refusal lane from a refusal log line (`metric=<lane>` or the wire token). */
export function refusalLane(line) {
  const m = /metric[=:]\s*"?([a-z_]+)"?/.exec(line) ?? /resource_guard:critical:([a-z_]+):/.exec(line);
  return m ? m[1] : "unknown";
}

/**
 * The WARN notices the resource guard logs for a spawn it lets through past the
 * session WARN limit (`resource_guard.rs`: "resource_guard: spawning past the
 * session warn limit", with `lane`, `metric`, `observed` and `limit` fields). They
 * are a diagnostic, never a refusal.
 *
 * @returns {{metric: string, lane: string | null, observed: number | null, limit: number | null}[]}
 */
export function parseGuardWarnings(text) {
  const out = [];
  for (const line of String(text).split(/\r?\n/)) {
    if (!/resource_guard: spawning past the session warn limit/.test(line)) continue;
    const field = (k) => {
      const m = new RegExp(`\\b${k}=("?)([^\\s"]+)\\1`).exec(line);
      return m ? m[2] : null;
    };
    const num = (k) => {
      const v = Number(field(k));
      return field(k) !== null && Number.isFinite(v) ? v : null;
    };
    out.push({ metric: field("metric") ?? "unknown", lane: field("lane"), observed: num("observed"), limit: num("limit") });
  }
  return out;
}

/** WARN notices as one diagnostic: how many, per metric, and the highest reading. */
export function summarizeGuardWarnings(warnings) {
  const byMetric = {};
  for (const w of warnings) {
    const m = (byMetric[w.metric] ??= { count: 0, max_observed: null, limit: w.limit });
    m.count += 1;
    if (w.observed !== null && (m.max_observed === null || w.observed > m.max_observed)) m.max_observed = w.observed;
  }
  return { count: warnings.length, by_metric: byMetric };
}

/** A device JWT's payload, or null (the payload is base64, not encrypted). */
export function jwtPayload(token) {
  try {
    const part = String(token).split(".")[1];
    return JSON.parse(Buffer.from(part.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8"));
  } catch {
    return null;
  }
}

export class RunnerAdapter {
  static hostIdKey = "QONTINUI_TERMINAL_ID";

  /**
   * @param {{
   *   variant?: "default" | "ghostty" | "pty-holder",
   *   mode: "subject" | "supervisor",
   *   build: {role: "head" | "baseline" | "single", commit: string | null, exe?: string | null,
   *     gitRef?: string | null, worktreePath?: string | null, source?: string},
   *   supervisorUrl?: string, repoRoot: string, log?: (s: string) => void,
   *   extraEnv?: object, platform?: string, env?: object, readyTimeoutMs?: number,
   *   buildTimeoutMs?: number, fetch?: typeof fetch, spawnFn?: typeof spawn, run?: typeof spawnSync,
   *   list?: () => Map<number, object>, coordUrl?: string,
   * }} o
   */
  constructor(o) {
    this.id = "runner";
    this.hostIdKey = RunnerAdapter.hostIdKey;
    this.variant = o.variant ?? "default";
    this.mode = o.mode;
    this.build = { ...o.build };
    this.supervisorUrl = o.supervisorUrl ?? SUPERVISOR_URL;
    this.repoRoot = o.repoRoot;
    this.log = o.log ?? (() => {});
    this.extraEnv = o.extraEnv ?? {};
    this.platform = o.platform ?? process.platform;
    this.baseEnv = o.env ?? process.env;
    this.readyTimeoutMs = o.readyTimeoutMs ?? 180000;
    this.buildTimeoutMs = o.buildTimeoutMs ?? 45 * 60000;
    this.fetch = o.fetch;
    this.spawnFn = o.spawnFn ?? spawn;
    this.run = o.run ?? spawnSync;
    this.list = o.list ?? (() => listProcesses({ platform: this.platform }));
    this.portListening = o.portListening ?? isPortListening;
    // Tests hand in a recording signal function; by default proc.mjs's own door.
    this.kill = o.kill;
    this.sleepFn = o.sleepFn ?? sleep;
    this.now = o.now ?? Date.now;
    this.coordUrl = o.coordUrl ?? "https://coord.qontinui.io";
    this.client = null;
    this.port = null;
    this.pid = null;
    this.startKey = null;
    this.child = null;
    this.childExit = null;
    this.root = null;
    this.supervisorRunnerId = null;
    this.buildSha = null;
    this.terminals = [];
    this.coordSessions = new Map();
    this.spawned = [];
    this.slots = new Map();
    this.zoneOf = new Map();
    this.focusSession = null;
    this.arm = null;
    this.health = null;
    this.drain = null;
    this.logFile = null;
    this.spawnLogStart = null;
    this.spawnSpans = null;
    this.admission = null;
    this.guardWarnings = [];
    this.facts = {};
    this.notes = [];
    if (!["subject", "supervisor"].includes(this.mode)) throw new RangeError(`runner mode must be subject or supervisor, got ${this.mode}`);
    if (!["default", "ghostty", "pty-holder"].includes(this.variant)) throw new RangeError(`unknown runner variant ${this.variant}`);
    if (this.variant !== "default" && this.mode !== "subject") {
      throw new RangeError(`the runner/${this.variant} variant changes a runner setting, so it runs only in subject mode (a private config, D4)`);
    }
  }

  async check() {
    const mechanisms = {};
    const fixes = checkFixCommits(this.build.commit, { repoRoot: this.repoRoot, run: this.run });
    mechanisms["runner.build_fixes"] = fixes;
    if (this.mode === "subject") {
      const sup = await supervisorAnswers(this.supervisorUrl, { fetch: this.fetch });
      const primary = await this.portListening(PRIMARY_RUNNER_PORT);
      mechanisms["runner.launch_mode"] = subjectLaunchVerdict(sup, primary);
      const exe = this.build.exe;
      mechanisms["setup.runner"] =
        exe && existsSync(exe)
          ? { state: "OPERATIVE", reason: `runner exe ${exe}` }
          : { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `no runner exe at ${exe ?? "(none given: --runner-exe)"}` };
    } else {
      const sup = await supervisorAnswers(this.supervisorUrl, { fetch: this.fetch });
      mechanisms["runner.launch_mode"] =
        sup === true
          ? { state: "OPERATIVE", reason: `supervisor_spawn_test via ${this.supervisorUrl}` }
          : sup === false
            ? { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `no supervisor listens at ${this.supervisorUrl}` }
            : { state: "UNKNOWN", reason: `the supervisor at ${this.supervisorUrl} did not answer /health in time` };
      mechanisms["setup.runner"] =
        this.build.gitRef || this.build.worktreePath
          ? { state: sup ? "OPERATIVE" : "UNKNOWN", reason: `spawn-test ${this.build.gitRef ? `git_ref ${this.build.gitRef}` : `worktree_path ${this.build.worktreePath}`}` }
          : { state: "INOPERATIVE-ON-THIS-MACHINE", reason: "supervisor mode needs --head-ref <sha> or --head-worktree <path>" };
    }
    return { mechanisms };
  }

  /**
   * @param {{runDir: string, unitDir: string, genRunId: string, runId: string,
   *   env: {env: object, display?: string}}} ctx
   */
  async launch(ctx) {
    this.ctx = ctx;
    this.abortSignal = ctx.signal ?? null;
    const fixes = checkFixCommits(this.build.commit, { repoRoot: this.repoRoot, run: this.run });
    if (fixes.state !== "OPERATIVE") throw new AdapterRefusal("harness_error", `refusing the build: ${fixes.reason}`);
    if (this.mode === "subject") await this.launchSubject(ctx);
    else await this.launchSupervisor(ctx);
    this.client = new RunnerClient(`http://127.0.0.1:${this.port}`, this.log, this.fetch ? { fetch: this.fetch } : {});
    const alive = () => (this.child ? this.childExit === null : true);
    try {
      this.health = await this.client.waitUiBridgeReady(this.readyTimeoutMs, { alive, signal: this.abortSignal });
    } catch (e) {
      throw new AdapterRefusal("harness_error", `${e.message}${this.refusalLogText()}`);
    }
    if (this.mode === "subject") {
      // The build is proven by the binary that is RUNNING: its /health gitSha.
      const sha = typeof this.health?.gitSha === "string" ? this.health.gitSha : null;
      if (sha && /^[0-9a-f]{7,40}$/i.test(sha) && this.build.commit && !shaMatches(sha, this.build.commit)) {
        throw new AdapterRefusal("harness_error", `the running subject reports gitSha ${sha}, not the build's ${this.build.commit} (D12)`);
      }
      this.buildProven = Boolean(sha && this.build.commit && shaMatches(sha, this.build.commit));
      // The start key is the only identity proof teardown has; take it again when the
      // first read (racing a slow process table) came back empty.
      if (!this.startKey && this.childExit === null && Number.isInteger(this.pid)) this.startKey = this.list().get(this.pid)?.startKey ?? null;
    }
    await this.checkDrain();
    await this.client.activateTab("terminal");
    try {
      const sink = await this.client.logSink();
      this.logFile = sink?.current_file ?? null;
    } catch (e) {
      this.notes.push(`runner log sink unreadable: ${e.message}`);
    }
    if (this.variant === "ghostty") await this.setBackend("ghostty");
    this.facts.renderBackend = await this.readRenderBackend();
    this.holderSetting = this.readPtyHolder();
    this.facts.ptyHolder = this.holderSetting;
    return { ok: true };
  }

  async launchSubject(ctx) {
    const verdict = subjectLaunchVerdict(
      await supervisorAnswers(this.supervisorUrl, { fetch: this.fetch }),
      await this.portListening(PRIMARY_RUNNER_PORT),
    );
    if (verdict.state !== "OPERATIVE") throw new AdapterRefusal("harness_error", verdict.reason);
    const exe = this.build.exe;
    if (!exe || !existsSync(exe)) throw new AdapterRefusal("harness_error", `no runner exe at ${exe}`);
    this.root = assertInside(ctx.runDir, join(ctx.unitDir, "runner-root"));
    mkdirSync(this.root, { recursive: true });
    if (this.variant === "pty-holder") {
      const cfg = join(this.root, "config");
      mkdirSync(cfg, { recursive: true });
      writeFileSync(assertInside(ctx.runDir, join(cfg, "settings.json")), `${JSON.stringify({ terminal: { pty_holder: true } }, null, 2)}\n`);
    }
    const xdgDir = assertInside(ctx.runDir, subjectXdgDir(this.root));
    if (this.platform === "linux") {
      for (const sub of ["config", "data", "cache", "state"]) mkdirSync(join(xdgDir, sub), { recursive: true });
    }
    this.port = await freePort(9877);
    const env = subjectEnv({
      base: this.baseEnv,
      root: this.root,
      port: this.port,
      instanceName: `termhost-${ctx.genRunId}`.slice(0, 64),
      platform: this.platform,
      display: ctx.env?.env?.DISPLAY ?? null,
      extra: this.extraEnv,
      xdgDir,
    });
    // The runner's own startup contract, checked before it is spawned: a launch it
    // would refuse with exit 78 is refused here, with every reason named.
    const contract = subjectContractProblems(env, { platform: this.platform });
    if (contract.length > 0) {
      throw new AdapterRefusal("harness_error", `the subject launch would break the runner's instance-root contract: ${contract.join("; ")}`);
    }
    this.stderrPath = assertInside(ctx.runDir, join(ctx.unitDir, "runner-stderr.log"));
    const out = openSync(assertInside(ctx.runDir, join(ctx.unitDir, "runner-stdout.log")), "a");
    const err = openSync(assertInside(ctx.runDir, join(ctx.unitDir, "runner-stderr.log")), "a");
    try {
      this.child = this.spawnFn(exe, [], { cwd: this.root, env, stdio: ["ignore", out, err], windowsHide: false });
    } finally {
      closeSync(out);
      closeSync(err);
    }
    this.pid = this.child.pid;
    this.child.on?.("exit", (code, signal) => {
      this.childExit = { code, signal };
    });
    this.startKey = this.list().get(this.pid)?.startKey ?? null;
    this.log(`runner: subject pid ${this.pid} on :${this.port} under ${this.root}`);
  }

  /**
   * Why a subject that never became ready did not: its startup-refusal.log (written
   * only when the root itself was usable), its exit, and the tail of its stderr —
   * where a refusal the runner could not log into its root (exit 78 on the root
   * itself) is printed. Runner-only text, so it may carry detail (D7).
   */
  refusalLogText() {
    if (!this.root) return "";
    const parts = [];
    try {
      const text = readFileSync(join(this.root, "startup-refusal.log"), "utf8").trim();
      if (text) parts.push(`startup-refusal.log: ${text.slice(-600)}`);
    } catch {
      // no refusal log: the root itself may have been refused
    }
    if (this.childExit) {
      parts.push(`the runner exited ${JSON.stringify(this.childExit)}`);
      if (this.stderrPath) {
        try {
          const tail = readFileSync(this.stderrPath, "utf8").trim();
          if (tail) parts.push(`stderr: ${tail.slice(-800)}`);
        } catch {
          // no stderr file
        }
      }
    }
    return parts.map((x) => ` — ${x}`).join("");
  }

  async launchSupervisor(ctx) {
    const sup = new SupervisorClient(this.supervisorUrl, this.log, this.fetch ? { fetch: this.fetch } : {});
    this.supervisor = sup;
    try {
      await sup.health();
    } catch (e) {
      throw new AdapterRefusal("harness_error", `the supervisor at ${this.supervisorUrl} does not answer: ${e.message}`);
    }
    this.requesterId = `termhost-bench-${ctx.genRunId}`.slice(0, 64);
    const body = spawnTestBody({
      gitRef: this.build.gitRef ?? null,
      worktreePath: this.build.worktreePath ?? null,
      requesterId: this.requesterId,
      display: ctx.env?.env?.DISPLAY ?? null,
      platform: this.platform,
    });
    let queued = null;
    try {
      queued = await sup.spawnTestAsync(body);
    } catch (e) {
      // The answer was lost (a timeout, a dropped connection): the supervisor may
      // still have minted a runner. Find it, or at least remember it for teardown.
      const found = await this.recoverLostSpawn(sup);
      if (!found?.submissionId) {
        throw new AdapterRefusal("harness_error", `spawn-test failed (${e.message.slice(0, 200)})${found ? `; temp runner ${found.id} found by requester id and will be stopped` : ""}`);
      }
      queued = { submission_id: found.submissionId, id: found.id, port: found.port };
      this.notes.push(`spawn-test answer lost (${e.message.slice(0, 120)}); recovered submission ${found.submissionId} from /runners/spawn-test/in-flight`);
    }
    this.supervisorRunnerId = queued.id;
    this.port = queued.port;
    this.log(`runner: spawn-test queued ${queued.submission_id} -> ${queued.id} on :${queued.port}`);
    let outcome;
    try {
      outcome = await sup.awaitSpawn(queued.submission_id, { timeoutMs: this.buildTimeoutMs, signal: this.abortSignal });
    } catch (e) {
      const found = await this.recoverLostSpawn(sup);
      throw new AdapterRefusal("harness_error", `${e.message.slice(0, 300)}${found ? `; temp runner ${found.id} will be stopped` : ""}`);
    }
    const verdict = judgeSpawnOutcome(outcome, {
      gitRef: this.build.gitRef,
      worktreePath: this.build.worktreePath,
      expectedCommit: this.build.commit,
    });
    if (!verdict.ok) throw new AdapterRefusal("harness_error", verdict.detail);
    this.buildSha = verdict.buildSha;
    if (verdict.port) this.port = verdict.port;
    this.log(`runner: ${verdict.detail}`);
    try {
      const rows = await httpJson(`${this.supervisorUrl}/runners`, { timeoutMs: 20000, ...(this.fetch ? { fetch: this.fetch } : {}) });
      const list = Array.isArray(rows) ? rows : (rows?.runners ?? []);
      const row = list.find((r) => r?.id === this.supervisorRunnerId);
      if (Number.isInteger(row?.pid)) {
        this.pid = row.pid;
        this.startKey = this.list().get(this.pid)?.startKey ?? null;
      }
    } catch (e) {
      this.notes.push(`GET /runners for the temp runner's pid failed: ${e.message}`);
    }
  }

  /**
   * After a lost spawn-test answer: the runner the supervisor reserved for OUR
   * requester id. The in-flight index answers while the build runs (with its
   * submission id, so the spawn can still be awaited); once the build ends the key
   * is released, and `GET /runners` filtered by `requester_id` finds the runner.
   * Whatever is found is remembered, so teardown stops it.
   *
   * @returns {Promise<{id: string, port: number | null, submissionId?: string} | null>}
   */
  async recoverLostSpawn(sup) {
    try {
      const inflight = await sup.spawnInFlight({ requesterId: this.requesterId });
      const hit = (inflight?.spawns ?? []).find((x) => x?.requester_id === this.requesterId && typeof x?.id === "string");
      if (hit) {
        this.supervisorRunnerId = hit.id;
        return { id: hit.id, port: Number.isInteger(hit.port) ? hit.port : null, submissionId: hit.submission_id ?? hit.build_id };
      }
    } catch (e) {
      this.notes.push(`GET /runners/spawn-test/in-flight failed: ${e.message.slice(0, 160)}`);
    }
    try {
      const row = (await sup.runners()).find((r) => r?.requester_id === this.requesterId && typeof r?.id === "string");
      if (row) {
        this.supervisorRunnerId = row.id;
        return { id: row.id, port: Number.isInteger(row.port) ? row.port : null };
      }
    } catch (e) {
      this.notes.push(`GET /runners failed while recovering a lost spawn: ${e.message.slice(0, 160)}`);
    }
    return null;
  }

  /**
   * The device drain as the runner under test reads it (`/restart-readiness`
   * `drain.coordDrain`, `coord_drain_state.rs`). A backstop to the doctor's own
   * read before the first unit: `clear` and `not_enrolled` pass; `drained` and
   * `unknown` refuse; a boot read still pending is waited for, and a wait that ends
   * pending is refused like any other unknown.
   */
  async checkDrain({ waitMs = 30000, sleepFn = this.sleepFn, now = this.now } = {}) {
    const deadline = now() + waitMs;
    let cd = null;
    let lastErr = null;
    while (now() < deadline) {
      try {
        const rr = unwrap(await this.client.restartReadiness());
        cd = rr?.drain?.coordDrain ?? null;
        if (cd && cd.bootReadPending !== true) break;
      } catch (e) {
        lastErr = e;
      }
      await sleepFn(1000);
    }
    this.drain = cd;
    if (!cd) {
      throw new AdapterRefusal(
        "device_drained",
        `the coord drain state could not be read (${lastErr?.message ?? "no drain.coordDrain in /restart-readiness"}); an unknown drain is refused`,
      );
    }
    if (cd.bootReadPending === true) {
      throw new AdapterRefusal("device_drained", `the runner's first coord drain read was still pending after ${Math.round(waitMs / 1000)} s; an unknown drain is refused`);
    }
    if (cd.state !== "clear" && cd.state !== "not_enrolled") {
      throw new AdapterRefusal("device_drained", `coord drain state ${cd.state}${cd.cause ? `: ${cd.cause}` : ""}${cd.reason ? ` (${cd.reason})` : ""}`);
    }
  }

  async setBackend(backend) {
    const key = `terminal-backend:${this.port}`;
    await this.client.evaluate(`(() => { localStorage.setItem(${JSON.stringify(key)}, ${JSON.stringify(backend)}); return localStorage.getItem(${JSON.stringify(key)}); })()`, 10000);
  }

  async readRenderBackend() {
    const key = this.port === PRIMARY_RUNNER_PORT ? "terminal-backend" : `terminal-backend:${this.port}`;
    try {
      const v = await this.client.evaluate(`(() => localStorage.getItem(${JSON.stringify(key)}) || "xterm")()`, 10000);
      return typeof v === "string" && /^[a-z][a-z0-9_]*$/.test(v)
        ? v
        : { state: "unknown", reason: "harness_error", detail: `the backend read answered ${JSON.stringify(v)}` };
    } catch (e) {
      return { state: "unknown", reason: "harness_error", detail: `page/evaluate failed: ${e.message.slice(0, 200)}` };
    }
  }

  /** The settings.json the runner under test reads `terminal.pty_holder` from. */
  settingsPath() {
    if (this.mode === "subject") return this.root ? join(this.root, "config", "settings.json") : null;
    if (this.supervisorRunnerId) {
      return join(platformDirs({ platform: this.platform, env: this.baseEnv }).configDir, "com.qontinui.runner", "instances", this.supervisorRunnerId, "settings.json");
    }
    return null;
  }

  readPtyHolder() {
    const p = this.settingsPath();
    if (!p) return { state: "unknown", reason: "harness_error", detail: "the runner's settings path is not known" };
    if (!existsSync(p)) {
      return this.mode === "subject" ? false : { state: "unknown", reason: "harness_error", detail: `no settings.json at ${p}` };
    }
    try {
      return JSON.parse(readFileSync(p, "utf8"))?.terminal?.pty_holder === true;
    } catch (e) {
      return { state: "unknown", reason: "harness_error", detail: `settings.json unreadable: ${e.message}` };
    }
  }

  logSize() {
    if (!this.logFile) return 0;
    try {
      return statSync(this.logFile).size;
    } catch {
      return 0;
    }
  }

  logSince(offset) {
    if (!this.logFile) return "";
    try {
      const buf = readFileSync(this.logFile);
      return buf.subarray(Math.min(offset, buf.length)).toString("utf8");
    } catch {
      return "";
    }
  }

  async terminalIds() {
    const list = await this.client.listTerminals();
    return list.map((t) => ({ id: t.id, createdAt: t.createdAt ?? t.created_at ?? 0 }));
  }

  /** The guard dialog's decline button, if the dialog is up. */
  async findGuardDialog() {
    let data;
    try {
      data = await this.client.discover({ force: true });
    } catch {
      return null;
    }
    const els = Array.isArray(data?.elements) ? data.elements : Array.isArray(data) ? data : [];
    const text = (e) => [e.label, e.text, e.accessibleName, e.content].filter((s) => typeof s === "string").map((s) => s.trim());
    const confirm = els.find((e) => text(e).some((t) => NEVER_CLICK.test(t)));
    const decline = els.find((e) => text(e).some((t) => DECLINE_LABELS.includes(t)) && !text(e).some((t) => NEVER_CLICK.test(t)));
    if (!confirm || !decline) return null;
    return { declineId: decline.id, message: text(els.find((e) => text(e).some((t) => /Not starting/i.test(t))) ?? {})[0] ?? null };
  }

  /**
   * One guarded `create-with-command`. Returns the terminals it created (diffed from
   * `GET /terminals`, which a partial failure does not lose) and any refusals met.
   */
  async createWithCommand(count) {
    const before = new Set((await this.terminalIds()).map((t) => t.id));
    const logStart = this.logSize();
    let settled = null;
    // The spawn's start: after the harness's own `GET /terminals`, immediately
    // before the action, so none of the harness's round trips are counted.
    const tCmdNs = hrtime();
    const call = this.client
      .componentAction(LAUNCH_MENU, "create-with-command", { count, command: this.command }, 60000 + 8000 * count)
      .then(
        (value) => {
          settled = { ok: true, value };
        },
        (error) => {
          settled = { ok: false, error };
        },
      );
    const refusals = [];
    const t0 = Date.now();
    while (!settled) {
      await Promise.race([call, sleep(1500)]);
      if (settled) break;
      const lines = this.logSince(logStart)
        .split("\n")
        .filter((l) => REFUSAL_LOG.test(l));
      if (lines.length > refusals.length || Date.now() - t0 > 15000) {
        const dialog = await this.findGuardDialog();
        if (dialog) {
          const line = lines[refusals.length] ?? lines[lines.length - 1] ?? "";
          refusals.push({ lane: refusalLane(line), message: dialog.message ?? line.slice(0, 300) });
          this.log(`runner: resource guard refused a spawn (${refusals.at(-1).lane}); declining — never "Start anyway"`);
          try {
            await this.client.elementAction(dialog.declineId, "click");
          } catch (e) {
            this.notes.push(`declining the guard dialog failed: ${e.message}`);
          }
        }
      }
    }
    await call;
    const after = await this.terminalIds();
    const created = after.filter((t) => !before.has(t.id)).sort((a, b) => a.createdAt - b.createdAt);
    return {
      tabIds: created.map((t) => t.id),
      refusals,
      tCmdNs,
      warnings: parseGuardWarnings(this.logSince(logStart)),
      error: settled.ok ? null : settled.error?.message ?? String(settled.error),
    };
  }

  async spawnSessions(n, argv) {
    this.argv = argv;
    this.command = commandLine(argv, this.platform === "win32" ? "powershell" : "posix");
    this.spawnLogStart = this.logSize();
    const sessions = [];
    let remaining = n;
    while (remaining > 0 && !this.admission) {
      const count = Math.min(MAX_SPAWN_COUNT, remaining);
      const r = await this.createWithCommand(count);
      this.guardWarnings.push(...r.warnings);
      for (const id of r.tabIds) {
        sessions.push({ hostId: id, tCmdNs: r.tCmdNs });
        this.terminals.push(id);
      }
      if (r.refusals.length > 0) {
        let ceilings = null;
        try {
          ceilings = unwrap(await this.client.health())?.threadCeilings ?? null;
        } catch {
          // recorded as null
        }
        this.admission = {
          refused: true,
          lane: r.refusals[0].lane,
          n_reached: this.terminals.length,
          detail: r.refusals[0].message,
          threadCeilings: ceilings,
        };
      } else if (r.tabIds.length < count) {
        throw new Error(`create-with-command created ${r.tabIds.length} of ${count} terminals: ${r.error ?? "no error reported"}`);
      }
      remaining -= count;
    }
    this.spawnSpans = parseSpawnSpans(this.logSince(this.spawnLogStart));
    for (const id of this.terminals) await this.recordCoordSession(id);
    return { sessions, admission: this.admission };
  }

  async recordCoordSession(id) {
    try {
      const r = await this.client.coordSession(id);
      this.coordSessions.set(id, r?.coord_session_id ?? null);
    } catch (e) {
      this.coordSessions.set(id, { error: e.message });
    }
  }

  /** cols x rows of the given sessions' terminals (`GET /terminals`). */
  async paneSizes(sessions) {
    try {
      const list = await this.client.listTerminals();
      const byId = new Map(list.map((t) => [t.id, t]));
      return sessions
        .map((s) => byId.get(this.slots.get(s)))
        .filter((t) => t && Number.isInteger(t.cols) && Number.isInteger(t.rows))
        .map((t) => ({ cols: t.cols, rows: t.rows }));
    } catch {
      return [];
    }
  }

  async zoneViewState() {
    return actionResult(await this.client.componentAction(TERMINAL_PAGE, "get-zone-view-state", {}, 20000));
  }

  async restoreZone() {
    return actionResult(await this.client.componentAction(TERMINAL_PAGE, "restore-zone", {}, 20000));
  }

  async maximizeZone(zoneIndex) {
    return actionResult(await this.client.componentAction(TERMINAL_PAGE, "maximize-zone", { zoneIndex }, 20000));
  }

  /**
   * The visible zone headers (`terminal-zone-header-<i>`, `ZoneLabel.tsx`) in
   * row-major visual order, or null when they cannot be read.
   */
  async zoneHeaders(zoneCount) {
    const out = new Map();
    for (let i = 0; i < zoneCount; i += 1) {
      try {
        const el = await this.client.getElement(`terminal-zone-header-${i}`);
        const b = el?.bbox;
        if (el?.visible !== false && b && Number.isFinite(b.x) && Number.isFinite(b.y)) {
          out.set(i, { x: b.x, y: b.y, w: b.width ?? 0, h: b.height ?? 0 });
        }
      } catch {
        // not mounted (a virtualised flow-grid tile) or not registered
      }
    }
    return out.size > 0 ? rowMajorOrder(out) : null;
  }

  /**
   * Map sessions to zones from where the probe saw their markers: zip the
   * row-major marker order with the row-major header order when both grids have
   * the same shape (robust to scrolling and gaps); otherwise rank = zone index.
   */
  async resolveZones(seen, zoneCount) {
    const markers = rowMajorOrder(seen);
    const headers = await this.zoneHeaders(zoneCount);
    const shape = (rows) => rows.map((r) => r.length).join(",");
    if (headers && shape(headers.rows) === shape(markers.rows)) {
      return {
        zoneOf: new Map(markers.order.map((s, i) => [s, headers.order[i]])),
        method: "header_boxes",
        detail: `markers and zone headers agree on a ${shape(markers.rows)} grid`,
      };
    }
    return {
      zoneOf: new Map(markers.order.map((s, i) => [s, i])),
      method: "row_major_rank",
      detail: headers
        ? `zone headers (${shape(headers.rows)}) and markers (${shape(markers.rows)}) disagree in shape; using row-major rank`
        : "zone headers unreadable; using row-major rank",
    };
  }

  /**
   * @param {"focus" | "tiled"} arm
   * @param {{slots: Map<number, string>, n: number, look: () => Promise<{ok: boolean,
   *   sessions: Map<number, object>, detail: string}>}} actx
   */
  async arrange(arm, actx) {
    this.slots = new Map(actx.slots);
    this.arm = arm;
    await this.restoreZone();
    await sleep(ZONE_SETTLE_MS);
    const state = await this.zoneViewState();
    const seen = await actx.look();
    if (!seen.ok) {
      return { ok: false, vTarget: arm === "focus" ? 1 : Math.min(actx.n, 9), failure: { reason: "capture_unavailable", detail: seen.detail } };
    }
    const zones = await this.resolveZones(seen.sessions, state?.zoneCount ?? 0);
    this.zoneOf = zones.zoneOf;
    const visibleOrder = rowMajorOrder(seen.sessions).order;
    const layout = { layoutId: state?.layoutId ?? null, zoneCount: state?.zoneCount ?? null, zoneMapping: zones.method };
    if (arm === "tiled") {
      this.focusSession = visibleOrder[0] ?? null;
      if (visibleOrder.length === 0) {
        // Nothing to measure: say so here, with the layout the runner reports,
        // rather than soaking a page that shows no session.
        return {
          ok: false,
          vTarget: Math.min(actx.n, 9),
          layout,
          failure: {
            reason: "sessions_not_rendered",
            detail: `after restore-zone the probe saw no session marker (layout ${layout.layoutId ?? "unknown"}, ${layout.zoneCount ?? "unknown"} zone(s), maximized zone ${state?.maximizedZone === undefined ? "unknown" : (state.maximizedZone ?? "none")}; ${seen.detail})`,
          },
        };
      }
      return {
        ok: true,
        vTarget: Math.min(actx.n, 9),
        focusSession: this.focusSession,
        switchable: [],
        layout,
        paneSizes: await this.paneSizes(visibleOrder),
        notes: [zones.detail],
      };
    }
    const focus = visibleOrder[0];
    if (focus === undefined) {
      return { ok: false, vTarget: 1, layout, failure: { reason: "sessions_not_rendered", detail: `no session marker visible before the maximize (${seen.detail})` } };
    }
    await this.maximizeZone(this.zoneOf.get(focus));
    await sleep(700);
    const after = await actx.look();
    const only = after.sessions.size === 1 && after.sessions.has(focus);
    if (!only) {
      return {
        ok: false,
        vTarget: 1,
        layout,
        failure: {
          reason: "zone_unresolved",
          detail: `after maximize-zone ${this.zoneOf.get(focus)} for session ${focus} the probe saw sessions [${[...after.sessions.keys()].join(",")}] (${zones.detail})`,
        },
      };
    }
    this.focusSession = focus;
    return {
      ok: true,
      vTarget: 1,
      focusSession: focus,
      switchable: [...this.zoneOf.keys()],
      layout,
      paneSizes: await this.paneSizes([focus]),
      notes: [zones.detail],
    };
  }

  /**
   * Show session `k` alone: `maximize-zone` on its zone, directly. No `restore-zone`
   * first — maximize moves the maximized zone in one step
   * (`src/components/terminal/zone-grid/maximizeActions.ts`), while a restore would
   * repaint the whole grid with k visible before the maximize, and the switch would
   * time that repaint. The command time is taken immediately before the action.
   */
  async show(k) {
    const zone = this.zoneOf.get(k);
    if (zone === undefined) return { ok: false, detail: `session ${k} has no resolved zone` };
    const tCmdNs = hrtime();
    await this.maximizeZone(zone);
    this.focusSession = k;
    return { ok: true, tCmdNs };
  }

  async spawnOne() {
    const r = await this.createWithCommand(1);
    const tCmdNs = r.tCmdNs;
    this.guardWarnings.push(...r.warnings);
    if (r.refusals.length > 0) {
      return { ok: false, tCmdNs, admission: { refused: true, lane: r.refusals[0].lane, detail: r.refusals[0].message } };
    }
    const hostId = r.tabIds[0];
    if (!hostId) return { ok: false, tCmdNs, detail: r.error ?? "no terminal created" };
    this.spawned.push(hostId);
    this.terminals.push(hostId);
    await this.recordCoordSession(hostId);
    return { ok: true, hostId, tCmdNs };
  }

  async closeOne(hostId) {
    const id = hostId ?? this.spawned.pop();
    if (!id) return { ok: false, detail: "nothing to close" };
    this.spawned = this.spawned.filter((x) => x !== id);
    try {
      await this.client.closeTerminal(id);
      this.terminals = this.terminals.filter((x) => x !== id);
      this.closedTerminals = [...(this.closedTerminals ?? []), id];
      return { ok: true };
    } catch (e) {
      return { ok: false, detail: e.message };
    }
  }

  async focusObserved() {
    const session = this.focusSession;
    const terminalId = this.slots.get(session);
    if (!terminalId) return { ok: false, failure: { reason: "harness_error", detail: `focused session ${session} has no terminal id` } };
    try {
      await this.client.elementAction(`terminal-input-${terminalId}`, "focus");
      return { ok: true, session };
    } catch (e) {
      const text = `${e.message} ${JSON.stringify(e.body ?? "")}`;
      if (/TERMINAL_NO_MOUNTED_VIEW/.test(text)) {
        return { ok: false, failure: { reason: "pane_not_mounted", detail: `terminal-input-${terminalId}: TERMINAL_NO_MOUNTED_VIEW` } };
      }
      return { ok: false, failure: { reason: "harness_error", detail: `focus on terminal-input-${terminalId} failed: ${e.message.slice(0, 300)}` } };
    }
  }

  /**
   * PTY holder processes this runner started: a `qontinui-pty-holder` whose
   * command line names this instance's pane directory (`<root>/.../pty-panes`,
   * `terminal/daemon_pane_io.rs` `runner_pane_dir`). The runner serves no holder
   * census yet (`settings.rs` `pty_holder`: it lands with Phase 5 of the holder
   * plan), so the process table, narrowed to this instance's own pane directory,
   * is the evidence; holders escape the runner's process tree by design.
   */
  holderPids(table = this.list()) {
    if (this.holderSetting !== true || !this.root) return [];
    const out = [];
    for (const p of table.values()) {
      const exe = basename(Array.isArray(p.argv) ? (p.argv[0] ?? "") : p.name);
      if (/^qontinui-pty-holder/.test(exe) && p.cmdline.includes(this.root) && p.cmdline.includes("pty-panes")) out.push(p.pid);
    }
    return out;
  }

  /**
   * `pty_holder` as the runner RAN it, not as the adapter configured it. With the
   * setting on, a pane whose holder cannot start falls back to an in-process PTY and
   * says so in the runner log (`terminal/daemon_pane_io.rs`: "the holder could not
   * be started"). So: setting off -> false; on, no fallback logged and a holder per
   * session found -> true; every pane fell back -> false; a mix, or no holder process
   * found -> unknown with what was seen.
   */
  ptyHolderAsRun({ sessions = this.terminals.length, logText = this.spawnLogStart === null ? "" : this.logSince(this.spawnLogStart), table = this.list() } = {}) {
    const setting = this.holderSetting;
    if (setting === false) return false;
    if (setting !== true) return setting ?? { state: "unknown", reason: "harness_error", detail: "the pty_holder setting could not be read" };
    const fallbacks = (logText.match(/pty holder: terminal\.pty_holder is on but the holder could not be started/g) ?? []).length;
    const holders = this.holderPids(table).length;
    if (fallbacks === 0 && holders >= Math.max(1, sessions)) return true;
    if (fallbacks >= sessions && sessions > 0 && holders === 0) return false;
    return {
      state: "unknown",
      reason: "harness_error",
      detail: `pty_holder is on, but ${fallbacks} of ${sessions} pane(s) logged a fallback to an in-process PTY and ${holders} holder process(es) were found for this instance`,
    };
  }

  processRoots() {
    const roots = [this.pid, ...this.holderPids()].filter((p) => Number.isInteger(p) && p > 0);
    return [...new Set(roots)];
  }

  /**
   * The window the probe measures: the instance's own process. Never a title — on
   * a workstation "Qontinui" also names the operator's own runner windows (D5).
   * With no pid there is no target, and the arm fails as the harness's error.
   */
  windowTarget() {
    return Number.isInteger(this.pid) ? { pid: this.pid } : null;
  }

  /** The session shell's integration, measured from its command line. */
  shellFacts(slots) {
    const table = this.list();
    for (const rec of slots.values()) {
      const shell = table.get(rec.ppid);
      if (!shell) continue;
      const argv0 = Array.isArray(shell.argv) ? shell.argv[0] : shell.name;
      return {
        session_shell: basename(argv0 ?? shell.name).replace(/\.exe$/i, "") + (/powershell/i.test(shell.name) ? ".exe -NoLogo -NoExit" : ""),
        shell_integration: /qontinui-shell-integration/.test(shell.cmdline) || /ZDOTDIR/.test(shell.cmdline) ? "injected" : "not_injected",
      };
    }
    return null;
  }

  describe(slots = new Map()) {
    // Supervisor mode: only the running child's probed sha proves the build (S5);
    // subject mode: the artifact's commit, confirmed by the binary's own /health
    // gitSha at launch.
    const commit = this.mode === "supervisor" ? this.buildSha : this.buildProven ? this.build.commit : null;
    const shell = this.shellFacts(slots);
    return {
      id: "runner",
      variant: this.variant,
      app_version: typeof this.health?.version === "string" && this.health.version
        ? this.health.version
        : typeof this.health?.gitSha === "string"
          ? `qontinui-runner ${this.health.gitSha}`
          : { state: "unknown", reason: "harness_error", detail: "/health named no version" },
      build:
        commit && /^[0-9a-f]{40}$/.test(commit)
          ? { commit, source: this.build.source ?? (this.mode === "supervisor" ? (this.build.gitRef ? "git_ref" : "worktree") : "artifact"), role: this.build.role }
          : unprovenBuild(
              { ...this.build, source: this.build?.source ?? (this.mode === "supervisor" ? (this.build?.gitRef ? "git_ref" : "worktree") : "artifact") },
              "the build's commit could not be proven from the running binary (D12)",
            ),
      settings_digest: this.settingsDigest(),
      pty_host_impl: this.platform === "win32" ? { state: "unknown", reason: "not_instrumented", detail: "the Windows PTY host census arrives with Phase 4" } : "unix_pty",
      render_backend: this.facts.renderBackend ?? { state: "unknown", reason: "harness_error", detail: "not read" },
      pty_holder: this.client ? this.ptyHolderAsRun() : (this.facts.ptyHolder ?? { state: "unknown", reason: "harness_error", detail: "not read" }),
      launch_mode: this.mode === "subject" ? "subject_instance_root" : "supervisor_spawn_test",
      session_shell: shell?.session_shell ?? (this.platform === "win32" ? "powershell.exe -NoLogo -NoExit" : basename(this.baseEnv.SHELL || "bash")),
      shell_integration: shell?.shell_integration ?? { state: "unknown", reason: "harness_error", detail: "no session shell found in the process table" },
    };
  }

  settingsDigest() {
    const p = this.settingsPath();
    if (!p) return { state: "unknown", reason: "harness_error", detail: "settings path unknown" };
    try {
      return `sha256:${createHash("sha256").update(readFileSync(p)).digest("hex")}`;
    } catch {
      return { state: "not_applicable", reason: "not_applicable_to_setup", detail: `no settings.json at ${p}: the runner runs on its defaults` };
    }
  }

  /**
   * Runner-only diagnostics, every one non-comparable (D1).
   *
   * @param {{focusTerminalId?: string, focusSession?: number, genLogPath?: string}} [o]
   */
  async diagnostics(o = {}) {
    const d = {};
    const diag = (source, fields) => ({ comparable: false, source, ...fields });
    try {
      const h = unwrap(await this.client.health());
      d["health.ui_thread"] = h?.uiThread ? diag("GET /health uiThread", { state: "measured", data: h.uiThread }) : diag("GET /health uiThread", { state: "unknown", reason: "not_instrumented", detail: "absent from /health" });
      d["health.thread_census"] = h?.threadCensus ? diag("GET /health threadCensus", { state: "measured", data: h.threadCensus }) : diag("GET /health threadCensus", { state: "unknown", reason: "not_instrumented", detail: "null or absent (the census could not be taken)" });
      d["health.thread_ceilings"] = h?.threadCeilings ? diag("GET /health threadCeilings", { state: "measured", data: h.threadCeilings }) : diag("GET /health threadCeilings", { state: "unknown", reason: "not_instrumented", detail: "absent from /health" });
    } catch (e) {
      d["health.ui_thread"] = diag("GET /health", { state: "unknown", reason: "harness_error", detail: e.message.slice(0, 300) });
    }
    const t0 = Date.now();
    try {
      const v = await this.client.evaluate("1+1", 10000);
      d["ui_bridge.reachability"] = diag("POST /ui-bridge/control/page/evaluate 1+1", { state: "measured", unit: "ms", value: Date.now() - t0, data: { answered: v } });
    } catch (e) {
      d["ui_bridge.reachability"] = diag("POST /ui-bridge/control/page/evaluate 1+1", { state: "unknown", reason: "harness_error", detail: e.message.slice(0, 300) });
    }
    try {
      d["transport.counters"] = diag("GET /terminals/transport-stats", { state: "measured", data: unwrap(await this.client.transportStats()) });
    } catch (e) {
      d["transport.counters"] = diag("GET /terminals/transport-stats", {
        state: "unknown",
        reason: "not_instrumented",
        detail: e instanceof HttpError && e.status === 404 ? "not served by this build (runner#1856)" : e.message.slice(0, 300),
      });
    }
    if (this.spawnSpans && this.spawnSpans.size > 0) {
      const data = {};
      for (const [name, ms] of this.spawnSpans) {
        const p50 = percentile(ms, 50);
        data[name] = { n: ms.length, p50_ms: p50.state === "measured" ? p50.value : null, max_ms: Math.max(...ms) };
      }
      d["spawn.spans"] = diag("runner log terminal_spawn.* span closes", { state: "measured", data });
    } else {
      d["spawn.spans"] = diag("runner log terminal_spawn.* span closes", { state: "unknown", reason: "not_instrumented", detail: "no terminal_spawn.* span close in the runner log (debug spans are off at the default level)" });
    }
    if (Array.isArray(o.visibleTerminalIds) && o.visibleTerminalIds.length > 0) {
      const per = [];
      for (const id of o.visibleTerminalIds.slice(0, 9)) {
        try {
          const snap = await this.client.terminalText(id);
          // `TextSnapshot` (`terminal/grid.rs`): {cols, rows, lines[], text, cursor_row, cursor_col}.
          const lines = Array.isArray(snap?.lines) ? snap.lines : typeof snap?.text === "string" ? snap.text.split(/\r?\n/) : [];
          per.push({
            terminal: id,
            grid: Number.isInteger(snap?.cols) && Number.isInteger(snap?.rows) ? `${snap.cols}x${snap.rows}` : null,
            lines: lines.length,
            non_blank_lines: lines.filter((r) => String(r).trim() !== "").length,
          });
        } catch (e) {
          per.push({ terminal: id, error: e.message.slice(0, 120) });
        }
      }
      d["fidelity.grid_text"] = diag("GET /ui-bridge/sdk/terminal/sessions/{id}/text (post-soak)", { state: "measured", data: per });
    }
    if (o.focusTerminalId && Number.isInteger(o.focusSession) && o.genLogPath) {
      d["echo.backend_latency"] = await this.backendEcho(o.focusTerminalId, o.focusSession, o.genLogPath);
    }
    d["drain.coord"] = this.drain ? diag("GET /restart-readiness drain.coordDrain", { state: "measured", data: this.drain }) : diag("GET /restart-readiness", { state: "unknown", reason: "harness_error", detail: "not read" });
    if (this.admission) d["spawn.admission"] = diag("create-with-command + resource guard", { state: "measured", data: this.admission });
    d["spawn.admission_warnings"] = diag("runner log: resource_guard spawning past the session warn limit", {
      state: "measured",
      data: summarizeGuardWarnings(this.guardWarnings),
    });
    return d;
  }

  /**
   * Backend echo latency over `GET /terminals/{id}/ws` — untouched by visibility
   * tiers, so subtracted from the outside-in input latency it splits a runner
   * number into backend and webview time. Each probe byte is answered by the
   * generator's echo marker; the time until that marker's bytes arrive is one
   * sample.
   */
  async backendEcho(terminalId, session, genLogPath, { count = 30 } = {}) {
    const source = "GET /terminals/{id}/ws input -> echo marker bytes";
    if (typeof globalThis.WebSocket !== "function") {
      return { comparable: false, source, state: "unknown", reason: "not_instrumented", detail: `this Node (${process.version}) has no WebSocket client` };
    }
    const echoes = () => {
      try {
        return readFileSync(genLogPath, "utf8").split("\n").filter((l) => l.includes('"type":"echo"')).length;
      } catch {
        return null;
      }
    };
    const base = echoes();
    if (base === null) return { comparable: false, source, state: "unknown", reason: "harness_error", detail: `cannot read ${genLogPath}` };
    let buf = "";
    const ws = new globalThis.WebSocket(`ws://127.0.0.1:${this.port}/terminals/${encodeURIComponent(terminalId)}/ws`);
    const opened = await new Promise((resolve) => {
      ws.onopen = () => resolve(true);
      ws.onerror = () => resolve(false);
      setTimeout(() => resolve(false), 5000);
    });
    if (!opened) return { comparable: false, source, state: "unknown", reason: "harness_error", detail: "the terminal websocket did not open" };
    ws.onmessage = (ev) => {
      try {
        const m = JSON.parse(String(ev.data));
        if (m.type === "output" && typeof m.data === "string") {
          buf = (buf + Buffer.from(m.data, "base64").toString("latin1")).slice(-131072);
        }
      } catch {
        // a non-JSON frame carries no output
      }
    };
    const samples = [];
    for (let i = 0; i < count; i += 1) {
      const overlay = markerOverlay({ kind: MARKER_KINDS.echo, session, seq: markerSeq(base + i) }, ECHO_REGION);
      const t0 = hrtime();
      ws.send(JSON.stringify({ type: "input", data: Buffer.from("e").toString("base64") }));
      const deadline = Date.now() + 2000;
      while (Date.now() < deadline && !buf.includes(overlay)) await sleep(2);
      if (buf.includes(overlay)) samples.push(Number(hrtime() - t0) / 1e6);
      await sleep(50);
    }
    ws.close();
    const p50 = percentile(samples, 50);
    const p95 = percentile(samples, 95);
    if (p50.state !== "measured" || p95.state !== "measured") {
      return { comparable: false, source, state: "unknown", reason: "insufficient_samples", n: samples.length };
    }
    return { comparable: false, source, state: "measured", unit: "ms", n: samples.length, stats: { p50: p50.value, p95: p95.value } };
  }

  /**
   * After teardown, read back from coord that every terminal's mirrored session is
   * closed (D12 (a)). Moot for an unpaired subject, whose terminals mirror nothing.
   */
  async coordReadback({ graceMs = 30000, pollMs = 5000 } = {}) {
    const ids = [...this.coordSessions.values()].filter((v) => typeof v === "string");
    const source = "GET coord /coord/sessions/fleet?include_closed=false";
    if (ids.length === 0) {
      return {
        diagnostic: { comparable: false, source, state: "not_applicable", reason: "not_applicable_to_setup", detail: "no terminal carried a coord session id (an unpaired runner mirrors none)" },
        incidents: [],
      };
    }
    let token = this.baseEnv.COORD_DEVICE_JWT ?? null;
    if (!token) {
      try {
        token = readFileSync(join(homedir(), ".qontinui", "coord-device-jwt"), "utf8").trim();
      } catch {
        token = null;
      }
    }
    const payload = token ? jwtPayload(token) : null;
    if (!token || !payload || (payload.exp && payload.exp * 1000 < Date.now())) {
      return {
        diagnostic: { comparable: false, source, state: "unknown", reason: "harness_error", detail: "no live coord device JWT ($COORD_DEVICE_JWT or ~/.qontinui/coord-device-jwt) to read the sessions back with" },
        incidents: [],
      };
    }
    const readOpen = async () => {
      const open = new Set();
      let cursor = null;
      for (let page = 0; page < 20; page += 1) {
        const q = new URLSearchParams({ include_closed: "false", limit: "500" });
        if (cursor) q.set("cursor", cursor);
        const res = await (this.fetch ?? globalThis.fetch)(`${this.coordUrl}/coord/sessions/fleet?${q}`, {
          headers: { Authorization: `Bearer ${token}` },
          signal: AbortSignal.timeout(20000),
        });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const body = await res.json();
        for (const s of body.sessions ?? []) if (typeof s.sessionId === "string") open.add(s.sessionId);
        cursor = body.nextCursor ?? null;
        if (!cursor) break;
      }
      return open;
    };
    // A session closes in coord a moment after its terminal does (the mirror's own
    // close, then coord's write): read again until none is open, or the grace ends.
    const deadline = this.now() + graceMs;
    let leftOpen = [];
    let reads = 0;
    try {
      for (;;) {
        const open = await readOpen();
        reads += 1;
        leftOpen = ids.filter((id) => open.has(id));
        if (leftOpen.length === 0 || this.now() >= deadline) break;
        await this.sleepFn(pollMs);
      }
    } catch (e) {
      return { diagnostic: { comparable: false, source, state: "unknown", reason: "harness_error", detail: `coord read failed: ${e.message}` }, incidents: [] };
    }
    const incidents = leftOpen.map((id) => ({
      kind: "coord_session_left_open",
      at: new Date().toISOString(),
      detail: `coord session ${id} (a terminal this run created) is still open after teardown`,
      stops_run: false,
      data: { coord_session_id: id },
    }));
    return {
      diagnostic: { comparable: false, source, state: "measured", data: { checked: ids.length, left_open: leftOpen.length, reads, grace_ms: graceMs } },
      incidents,
    };
  }

  /** The daemonised postmaster(s) of a subject's private cluster, from their own pid files. */
  postmasters(table) {
    if (!this.root) return [];
    const out = [];
    const walk = (dir, depth) => {
      if (depth > 4) return;
      let entries = [];
      try {
        entries = readdirSync(dir, { withFileTypes: true });
      } catch {
        return;
      }
      for (const e of entries) {
        const p = join(dir, e.name);
        if (e.isDirectory()) walk(p, depth + 1);
        else if (e.name === "postmaster.pid") {
          try {
            const pid = Number.parseInt(readFileSync(p, "utf8").split("\n")[0], 10);
            const proc = table.get(pid);
            if (proc && proc.cmdline.includes(this.root)) out.push({ pid, startKey: proc.startKey });
          } catch {
            // unreadable pid file
          }
        }
      }
    };
    walk(join(this.root, "embedded-pg"), 0);
    return out;
  }

  /**
   * A second signal's last act (no full teardown follows): stop the temp runner whose
   * supervisor id is already known, through the same checked stop teardown uses, or
   * signal a subject child whose identity was proven by its start key. Returns what
   * it did, or null when there was nothing known to stop.
   *
   * @returns {Promise<string | null>}
   */
  async emergencyStop() {
    if (this.mode === "supervisor" && this.supervisorRunnerId) {
      try {
        await this.supervisor.stopRunnerChecked(this.supervisorRunnerId, { requesterId: this.requesterId });
      } catch (e) {
        if (e instanceof RunnerNotListed) return `temp runner ${this.supervisorRunnerId} already gone`;
        throw e;
      }
      return `stopped temp runner ${this.supervisorRunnerId}`;
    }
    if (this.mode === "subject" && Number.isInteger(this.pid) && this.childExit === null && this.startKey) {
      const res = await terminateTree({ pid: this.pid, startKey: this.startKey }, { list: this.list, ...(this.kill ? { kill: this.kill } : {}) });
      return `signalled subject runner pid ${this.pid} (${res.signalled.length} process(es))`;
    }
    return null;
  }

  async teardown() {
    const closed = [];
    const survivors = [];
    const incidents = [];
    if (this.client) {
      for (const id of [...this.terminals]) {
        try {
          await this.client.closeTerminal(id);
          closed.push(`terminal ${id}`);
        } catch (e) {
          if (!(e instanceof HttpError && e.status === 404)) survivors.push({ what: `terminal ${id}`, detail: e.message.slice(0, 200) });
        }
      }
      this.terminals = [];
    }
    if (this.mode === "supervisor" && this.supervisorRunnerId) {
      try {
        // Checked against the supervisor's own row first: never a protected runner,
        // never one another requester owns (N8).
        await this.supervisor.stopRunnerChecked(this.supervisorRunnerId, { requesterId: this.requesterId });
        closed.push(`temp runner ${this.supervisorRunnerId}`);
      } catch (e) {
        // No longer listed: the runner is gone (stopped earlier, or reaped), which is
        // what teardown wanted — closed, not a survivor.
        if (e instanceof RunnerNotListed) closed.push(`temp runner ${this.supervisorRunnerId} (already gone: not listed)`);
        else survivors.push({ what: `temp runner ${this.supervisorRunnerId}`, detail: `stop failed: ${e.message.slice(0, 200)}` });
      }
      // Read back: the temp runner is gone, or at least no longer running.
      try {
        const rows = await httpJson(`${this.supervisorUrl}/runners`, { timeoutMs: 20000, ...(this.fetch ? { fetch: this.fetch } : {}) });
        const list = Array.isArray(rows) ? rows : (rows?.runners ?? []);
        const row = list.find((r) => r?.id === this.supervisorRunnerId);
        if (row && (row.running === true || row.liveness?.state === "responding")) {
          survivors.push({ what: `temp runner ${this.supervisorRunnerId}`, detail: "still listed running after stop" });
        }
      } catch (e) {
        survivors.push({ what: `temp runner ${this.supervisorRunnerId}`, detail: `stop read-back failed (UNKNOWN): ${e.message.slice(0, 200)}` });
      }
    } else if (this.mode === "subject" && Number.isInteger(this.pid)) {
      const table = this.list();
      const also = [...this.postmasters(table)];
      for (const pid of this.holderPids(table)) {
        const p = table.get(pid);
        if (p && this.startKey && startKeyCompare(p.startKey, this.startKey) >= 0) also.push({ pid, startKey: p.startKey });
      }
      // Our child, by its own handle: when it has already exited, its PID may belong
      // to something else by now, so the tree walk is skipped (only the processes
      // proven ours by their own start keys are signalled). And a tree walk needs the
      // root's start key: without one, the PID's identity cannot be proven (S8).
      const exited = this.childExit !== null;
      const root = exited ? null : { pid: this.pid, startKey: this.startKey ?? null };
      const res = await terminateTree(root, { list: this.list, alsoPids: also, ...(this.kill ? { kill: this.kill } : {}) });
      if (res.refused) survivors.push({ what: `subject runner pid ${this.pid}`, detail: res.refused });
      closed.push(`subject runner pid ${this.pid}${exited ? " (had already exited)" : ""} (${res.signalled.length} process(es) signalled)`);
      for (const pid of res.survivors) survivors.push({ what: `pid ${pid}`, detail: "survived SIGTERM and SIGKILL" });
      // A daemonised postmaster that appeared after the tree walk is still ours by its pid file.
      for (const pm of this.postmasters(this.list())) {
        terminatePid(pm.pid, { signal: "SIGINT", table: this.list(), expect: (p) => p.startKey === pm.startKey, ...(this.kill ? { kill: this.kill } : {}) });
      }
    }
    return { closed, survivors, incidents, notes: [...this.notes] };
  }
}

