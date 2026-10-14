/**
 * Settle perf-harness's inactive-page inference (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3):
 *
 *   "With the probe watching, run perf-harness at S = 10 against a temp runner with
 *   `--generator` set to the AI-session generator (so its terminals carry markers).
 *   If the markers of terminals that should be on screen never appear, the
 *   inference is confirmed."
 *
 * The inference: a perf-harness that creates its terminals with `POST /terminals
 * {page_id: "perf-harness"}` puts them on a page of their own, which the frontend
 * does NOT activate while the active page is occupied — so they are unwatched, get
 * no webview output, and its frontend numbers describe an idle page. On a FRESH
 * runner the premise fails: `computeVisiblePages` (useTerminalPages.ts) hides
 * `default` while no terminal is on it, and the keep-active-valid effect then
 * switches to the first visible page — the harness's own, which renders. The
 * inference holds only on an occupied active page (an operator's box), so the
 * settle SEATS one marker-free terminal on the active page first, through the
 * operator's path (the adapter's guarded `create-with-command`, count 1, command
 * `true`), and waits until it is listed. A seat that fails is `unknown`.
 *
 * What this does, reusing the run path rather than a second copy of it: start the
 * environment (`envs/`), launch a supervisor-free SUBJECT runner through the
 * runner adapter exactly as a `run` unit does (`adapters/runner.mjs`, which also
 * activates the Terminal tab), build the D13 generator command for the subject's
 * shell (`rundir.mjs`), then spawn the perf-harness under test as a child (an argv
 * array, never a shell string) against the subject's URL with `--generator` set to
 * that command. Once all S generators have claimed a slot in the gen dir — proof
 * the command ran in each PTY, independent of rendering — the probe takes short
 * looks (`probe.mjs` `look`) during the harness's soak and records which sessions'
 * markers it SAW. After the harness exits, the subject's terminals are listed to
 * count what the harness left behind; teardown is the adapter's own (every
 * terminal, the subject by its own PID handle), then the generators are reaped as
 * a unit's are (`run.mjs` `reapGenerators`).
 *
 * Verdicts — an unknown is never read as either answer:
 *   rendered       at least one session's markers were seen while the harness ran;
 *   not_rendered   S of S generators ran, every look was healthy and taken while
 *                  the harness ran, and none of them saw a marker: the inference
 *                  is CONFIRMED;
 *   unknown        any precondition failed (the launch, the seat, the harness
 *                  spawn, the generators, a look, an interruption) — with the reason.
 *
 * Every process here is started with an argv array; nothing is signalled by name.
 */

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { closeSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

import { createAdapter as defaultCreateAdapter } from "./adapters/index.mjs";
import { PRIMARY_RUNNER_PORT } from "../lib/runner-clients.mjs";
import { environmentSpec } from "./envs/index.mjs";
import * as probeLib from "./probe.mjs";
import { realProbeDriver, reapGenerators as defaultReapGenerators } from "./run.mjs";
import { commandLine, createRunDir, createUnitDir, generatorArgv, generatorRunId, makeRunId } from "./rundir.mjs";
import { abortableSleep, readSlots } from "./sessions.mjs";

export const SETTLE_SCHEMA = "termhost-bench/perf-harness-settle/v1";

/** The command the seated terminal's shell is given: a no-op that paints no marker. */
export const SEAT_COMMAND = "true";

export const VERDICTS = Object.freeze(["rendered", "not_rendered", "unknown"]);

/** Defaults: S = 10 as the plan says, and a soak long enough to hold every look. */
export const SETTLE_DEFAULTS = Object.freeze({
  sessions: 10,
  soakMs: 45000,
  settleMs: 3000,
  looks: 3,
  lookMs: 2500,
  lookIntervalMs: 4000,
  generatorTimeoutMs: 240000,
  seatTimeoutMs: 30000,
  profile: "claude-tui",
  seed: 1,
});

/**
 * The perf-harness child's argv. Refuses the primary runner's port: the harness
 * would too, but this tool never hands it one.
 *
 * @param {{node?: string, harness: string, runnerUrl: string, sessions: number, generator: string,
 *   soakMs: number, settleMs: number, label: string, out: string}} o
 * @returns {string[]}
 */
export function harnessArgv({ node = process.execPath, harness, runnerUrl, sessions, generator, soakMs, settleMs, label, out }) {
  let url;
  try {
    url = new URL(runnerUrl);
  } catch {
    throw new RangeError(`bad runner URL ${JSON.stringify(runnerUrl)}`);
  }
  const port = Number.parseInt(url.port || "0", 10);
  if (port === PRIMARY_RUNNER_PORT) throw new RangeError(`refusing ${runnerUrl}: port ${PRIMARY_RUNNER_PORT} is the primary runner`);
  if (!Number.isInteger(port) || port <= 0) throw new RangeError(`the runner URL ${runnerUrl} names no port`);
  if (!Number.isInteger(sessions) || sessions < 1) throw new RangeError(`sessions must be a positive integer, got ${sessions}`);
  if (typeof generator !== "string" || generator === "") throw new TypeError("the generator command line is empty");
  return [
    node,
    harness,
    "--runner-url",
    runnerUrl,
    "--sessions",
    String(sessions),
    "--generator",
    generator,
    "--soak-ms",
    String(soakMs),
    "--settle-ms",
    String(settleMs),
    "--label",
    label,
    "--out",
    out,
    "--quiet",
  ];
}

/**
 * The verdict. Preconditions first, so a failure is never read as an answer.
 *
 * @param {{sessions: number, launched: boolean, launchDetail?: string | null,
 *   seated: boolean, seatDetail?: string | null, harnessStarted: boolean, harnessDetail?: string | null, generatorsStarted: number,
 *   looks: {ok: boolean, sessions: number[], harnessRunning: boolean, detail: string}[],
 *   interrupted?: boolean}} f
 * @returns {{verdict: "rendered" | "not_rendered" | "unknown", reason: string, sessionsSeen: number[]}}
 */
export function settleVerdict(f) {
  const looks = f.looks ?? [];
  // Only a look that ended while the harness still ran watched its terminals.
  const valid = looks.filter((l) => l.harnessRunning);
  const seen = [...new Set(valid.filter((l) => l.ok).flatMap((l) => l.sessions))].sort((a, b) => a - b);
  const unknown = (reason) => ({ verdict: "unknown", reason, sessionsSeen: seen });
  if (f.interrupted) return unknown("the settle run was interrupted by a signal");
  if (!f.launched) return unknown(`the subject runner did not launch: ${f.launchDetail ?? "no detail"}`);
  if (!f.seated) {
    return unknown(`no marker-free terminal could be seated on the active page, so the page the harness's terminals land beside is not an occupied one: ${f.seatDetail ?? "no detail"}`);
  }
  if (!f.harnessStarted) return unknown(`the perf-harness child did not start: ${f.harnessDetail ?? "no detail"}`);
  if (!(f.generatorsStarted >= f.sessions)) {
    return unknown(`only ${f.generatorsStarted} of ${f.sessions} generator(s) claimed a slot: the harness did not run the generator in every terminal`);
  }
  if (seen.length > 0) {
    return { verdict: "rendered", reason: `the probe saw the markers of ${seen.length} of ${f.sessions} session(s) while the generators ran`, sessionsSeen: seen };
  }
  if (valid.length === 0) {
    return unknown(
      looks.length === 0 ? "no probe look was taken" : `none of the ${looks.length} look(s) ended while the perf-harness was still running`,
    );
  }
  const failed = valid.filter((l) => !l.ok);
  if (failed.length > 0) return unknown(`${failed.length} of ${valid.length} probe look(s) failed: ${failed[0].detail}`);
  return {
    verdict: "not_rendered",
    reason: `${f.generatorsStarted} of ${f.sessions} generators ran and ${valid.length} healthy probe look(s) saw no session's marker: the inference is confirmed`,
    sessionsSeen: seen,
  };
}

/**
 * What a perf-harness run JSON says about its levels' frontend, or null when the
 * file is absent or unreadable.
 */
export function summarizeHarnessRun(path) {
  let doc;
  try {
    doc = JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return null;
  }
  return {
    label: doc.label ?? null,
    notes: Array.isArray(doc.notes) ? doc.notes : [],
    levels: (Array.isArray(doc.levels) ? doc.levels : []).map((l) => ({
      sessions: l.sessions ?? null,
      created: l.created ?? null,
      createFailures: Array.isArray(l.createFailures) ? l.createFailures.length : null,
      frontend: l.frontend ?? null,
      frontendError: l.frontendError ?? null,
      webviewWedged: l.webviewWedged ?? null,
    })),
  };
}

/**
 * The real perf-harness child: an argv array, output to files, ended (if it must
 * be) by its own handle.
 *
 * @returns {{pid: number | undefined, exited: Promise<{code: number | null, signal: string | null, error?: string}>,
 *   running: () => boolean, stop: () => Promise<string>}}
 */
export function spawnHarnessProcess(argv, { env = process.env, cwd, stdoutPath, stderrPath, spawnFn = spawn }) {
  const out = openSync(stdoutPath, "a");
  const err = openSync(stderrPath, "a");
  let child;
  try {
    child = spawnFn(argv[0], argv.slice(1), { cwd, env, stdio: ["ignore", out, err], windowsHide: true });
  } finally {
    closeSync(out);
    closeSync(err);
  }
  let exit = null;
  const exited = new Promise((r) => {
    child.on("exit", (code, signal) => {
      exit = { code, signal };
      r(exit);
    });
    child.on("error", (e) => {
      exit = { code: null, signal: null, error: e.message };
      r(exit);
    });
  });
  return { pid: child.pid, exited, running: () => exit === null, stop: () => probeLib.endChild(child, exited) };
}

/**
 * Seat ONE terminal on the active page through the adapter's guarded
 * `create-with-command` (count 1, {@link SEAT_COMMAND}), then wait until
 * `GET /terminals` lists it.
 *
 * @returns {Promise<{ok: boolean, id: string | null, detail: string}>}
 */
export async function seatTerminal(adapter, { timeoutMs = SETTLE_DEFAULTS.seatTimeoutMs, sleep = (ms) => abortableSleep(ms), now = Date.now } = {}) {
  let r;
  try {
    adapter.command = SEAT_COMMAND;
    r = await adapter.createWithCommand(1);
  } catch (e) {
    return { ok: false, id: null, detail: `create-with-command failed: ${e.message}` };
  }
  const ids = r?.tabIds ?? [];
  if ((r?.refusals ?? []).length > 0) {
    return { ok: false, id: ids[0] ?? null, detail: `the resource guard refused the seat (${r.refusals[0].lane}): ${r.refusals[0].message ?? ""}`.trim() };
  }
  if (ids.length !== 1) return { ok: false, id: ids[0] ?? null, detail: `create-with-command created ${ids.length} terminal(s), not 1: ${r?.error ?? "no error reported"}` };
  const id = ids[0];
  const deadline = now() + timeoutMs;
  for (;;) {
    let listed = [];
    try {
      listed = (await adapter.client.listTerminals()).map((t) => t.id);
    } catch {
      // retried until the deadline
    }
    if (listed.includes(id)) return { ok: true, id, detail: `seated terminal ${id} on the active page` };
    if (now() >= deadline) return { ok: false, id, detail: `terminal ${id} was not listed within ${timeoutMs} ms` };
    await sleep(250);
  }
}

/** sha256 of a file, or null. */
export function fileSha256(path) {
  try {
    return createHash("sha256").update(readFileSync(path)).digest("hex");
  } catch {
    return null;
  }
}

/** The one human line. */
export function summaryLine(r) {
  return (
    `settle-perf-harness: ${r.verdict} — ${r.verdictReason}; generators ${r.generatorsStarted}/${r.sessions}; ` +
    `sessions seen [${r.sessionsSeen.join(",")}]; perf-harness exit ${JSON.stringify(r.perfHarnessExit)}; ` +
    `terminals left after the harness ${r.terminalsLeftAfterHarness ?? "UNKNOWN"}`
  );
}

/**
 * Run the settle once.
 *
 * @param {{env: string, runnerExe: string, probe?: string, headCommit: string, harness: string,
 *   sessions?: number, soakMs?: number, settleMs?: number, looks?: number, workDir?: string,
 *   out: string, runnerEnv?: Record<string, string>, runId?: string, repoRoot: string}} opts
 * @param {object} [deps] injection points for tests
 * @returns {Promise<object>} the result document (also written to `opts.out`)
 */
export async function runSettle(opts, deps = {}) {
  const o = { ...SETTLE_DEFAULTS, ...opts };
  const log = deps.log ?? ((s) => process.stderr.write(`termhost-bench: ${s}\n`));
  const now = deps.now ?? Date.now;
  const abort = new AbortController();
  const sleep = deps.sleep ?? ((ms) => abortableSleep(ms, abort.signal));
  const signals = deps.signals ?? process;
  const onSignal = (sig) => {
    log(`received ${sig}: ending the settle run and tearing down`);
    abort.abort(sig);
  };
  signals.on("SIGINT", onSignal);
  signals.on("SIGTERM", onSignal);

  const notes = [];
  const harness = resolve(o.harness);
  const result = {
    schema: SETTLE_SCHEMA,
    harness: o.harness,
    harnessSha256: fileSha256(harness),
    sessions: o.sessions,
    generatorsStarted: 0,
    sessionsSeen: [],
    verdict: "unknown",
    verdictReason: "the settle run did not reach a verdict",
    perfHarnessExit: null,
    perfHarnessRun: null,
    terminalsLeftAfterHarness: null,
    terminalsBeforeHarness: null,
    seatedTerminal: null,
    harnessSessionsArg: null,
    runnerUrl: null,
    runId: null,
    looks: [],
    notes,
  };
  if (result.harnessSha256 === null) notes.push(`the harness ${harness} could not be read`);

  const facts = { sessions: o.sessions, launched: false, launchDetail: null, seated: false, seatDetail: null, harnessStarted: false, harnessDetail: null, generatorsStarted: 0, looks: [], interrupted: false };
  let environment = null;
  let adapter = null;
  let child = null;
  let genDir = null;
  let genRunId = null;
  try {
    const spec = environmentSpec(o.env);
    if (spec.notBuilt) throw new Error(spec.notBuilt);
    const runId = o.runId ?? makeRunId(new Date(now()));
    result.runId = runId;
    const runDir = deps.runDir ?? createRunDir({ runId, base: o.workDir });
    const unit = { index: 0, runIndex: 0, n: o.sessions, setup: "runner", variant: "default", role: "single" };
    const made = createUnitDir(runDir, unit);
    genDir = made.genDir;
    const { unitDir } = made;
    genRunId = generatorRunId(runId, unit.index);
    log(`settle ${runId} in ${runDir}: ${harness} at S = ${o.sessions}`);

    environment = await (deps.startEnvironment ?? spec.start)({ log });
    const probePath = deps.probePath ?? probeLib.resolveProbeBinary({ explicit: o.probe ?? null });
    if (!probePath && !deps.probe) throw new Error("no termhost-probe binary (--probe, $TERMHOST_PROBE, target/*/)");
    const probe = deps.probe ?? realProbeDriver({ probePath, envVars: environment.env, log });

    // ---- the subject, launched as a `run` unit launches it ---------------------
    try {
      adapter = await (deps.createAdapter ?? defaultCreateAdapter)("runner", {
        variant: "default",
        mode: "subject",
        build: { role: "single", commit: o.headCommit ?? null, exe: o.runnerExe ? resolve(o.runnerExe) : null, source: "artifact" },
        repoRoot: o.repoRoot,
        extraEnv: o.runnerEnv ?? {},
        log,
      });
      await adapter.launch({ runDir, unitDir, genDir, genRunId, runId, unit, env: environment, signal: abort.signal });
      facts.launched = true;
    } catch (e) {
      facts.launchDetail = e?.detail ?? e?.message ?? String(e);
      throw e;
    }
    const runnerUrl = `http://127.0.0.1:${adapter.port}`;
    result.runnerUrl = runnerUrl;

    // ---- the generator command, and the harness child ------------------------------
    const argv = generatorArgv({ genDir, profile: o.profile, seed: o.seed, genRunId });
    const flavor = (adapter.platform ?? process.platform) === "win32" ? "powershell" : "posix";
    const generator = commandLine(argv, flavor);
    const act = await probe.activate({ runDir: join(unitDir, "activate"), target: adapter.windowTarget() });
    if (!act.ok) notes.push(`activation: ${act.detail}`);

    try {
      result.terminalsBeforeHarness = (await adapter.client.listTerminals()).map((t) => t.id);
    } catch (e) {
      notes.push(`listing the subject's terminals before the seat and the harness failed: ${e.message}`);
    }

    // ---- seat one marker-free terminal on the active page (the operator's path) ----
    const seat = await seatTerminal(adapter, { timeoutMs: o.seatTimeoutMs, sleep, now });
    facts.seated = seat.ok;
    facts.seatDetail = seat.detail;
    result.seatedTerminal = seat.id;
    if (seat.id !== null) adapter.terminals = [...new Set([...(adapter.terminals ?? []), seat.id])];
    log(`seat: ${seat.detail}`);
    if (!seat.ok) throw new Error(`seating a terminal on the active page failed: ${seat.detail}`);
    // perf-harness tops the runner UP to `--sessions` terminals, counting every
    // terminal already there — the seat included — so it is asked for S on top of
    // them, or it would create (and run the generator in) fewer than S.
    if (result.terminalsBeforeHarness === null) {
      throw new Error("the subject's terminals could not be listed before the seat, so the harness's top-up cannot be sized");
    }
    const present = new Set([...result.terminalsBeforeHarness, seat.id]).size;
    result.harnessSessionsArg = o.sessions + present;
    const harnessOut = join(unitDir, "perf-harness-run.json");
    const hArgv = harnessArgv({
      harness,
      runnerUrl,
      sessions: result.harnessSessionsArg,
      generator,
      soakMs: o.soakMs,
      settleMs: o.settleMs,
      label: `settle-${runId}`.slice(0, 64),
      out: harnessOut,
    });
    try {
      child = (deps.spawnHarness ?? spawnHarnessProcess)(hArgv, {
        cwd: o.repoRoot,
        env: process.env,
        stdoutPath: join(unitDir, "perf-harness-stdout.log"),
        stderrPath: join(unitDir, "perf-harness-stderr.log"),
      });
      facts.harnessStarted = Number.isInteger(child.pid);
      if (!facts.harnessStarted) facts.harnessDetail = "spawn returned no pid";
    } catch (e) {
      facts.harnessDetail = e.message;
    }
    log(`perf-harness pid ${child?.pid ?? "none"} against ${runnerUrl}`);

    // ---- the generators, then the looks ---------------------------------------------
    if (facts.harnessStarted) {
      const deadline = now() + o.generatorTimeoutMs;
      for (;;) {
        facts.generatorsStarted = readSlots(genDir).size;
        if (facts.generatorsStarted >= o.sessions || abort.signal.aborted || !child.running() || now() >= deadline) break;
        await sleep(250);
      }
      // A late slot from the last poll's race still counts.
      facts.generatorsStarted = readSlots(genDir).size;
      log(`${facts.generatorsStarted} of ${o.sessions} generator(s) claimed a slot`);
      if (facts.generatorsStarted >= o.sessions) {
        for (let i = 0; i < o.looks && !abort.signal.aborted; i += 1) {
          if (i > 0) await sleep(o.lookIntervalMs);
          const lk = await probe.look({ lookDir: join(unitDir, `look-${i}`), target: adapter.windowTarget(), durationMs: o.lookMs });
          const rec = { ok: Boolean(lk.ok), sessions: [...(lk.sessions?.keys?.() ?? [])].sort((a, b) => a - b), harnessRunning: child.running(), frames: lk.frames ?? null, detail: lk.detail ?? "" };
          facts.looks.push(rec);
          log(`look ${i}: ${rec.detail}; sessions [${rec.sessions.join(",")}]${rec.harnessRunning ? "" : " (the harness had exited)"}`);
        }
      }
    }

    // ---- the harness's exit, and what it left -------------------------------------
    if (child && facts.harnessStarted) {
      const bound = o.harnessTimeoutMs ?? o.soakMs + o.settleMs + 300000;
      const raced = abort.signal.aborted ? "timeout" : await probeLib.raceTimeout(child.exited, bound);
      if (raced === "timeout") {
        const how = await child.stop();
        notes.push(`the perf-harness child was ended by its handle (${how}) ${abort.signal.aborted ? "at the signal" : `after ${bound} ms`}`);
      }
      result.perfHarnessExit = await probeLib.raceTimeout(child.exited, 15000).then((x) => (x === "timeout" ? null : x));
      result.perfHarnessRun = summarizeHarnessRun(harnessOut);
      try {
        const after = (await adapter.client.listTerminals()).map((t) => t.id);
        const before = new Set(result.terminalsBeforeHarness ?? []);
        const left = after.filter((id) => !before.has(id) && id !== result.seatedTerminal);
        result.terminalsLeftAfterHarness = result.terminalsBeforeHarness === null ? null : left.length;
        // Teardown closes every terminal the subject still holds, the harness's included.
        adapter.terminals = [...new Set([...(adapter.terminals ?? []), ...after])];
      } catch (e) {
        notes.push(`listing the subject's terminals after the harness failed: ${e.message}`);
      }
    }
  } catch (e) {
    const detail = e?.detail ?? e?.message ?? String(e);
    if (!facts.launched && !facts.launchDetail) facts.launchDetail = detail;
    notes.push(`settle failed: ${detail}`);
    log(`settle failed: ${detail}`);
  } finally {
    facts.interrupted = abort.signal.aborted;
    if (child?.running?.()) {
      try {
        notes.push(`the perf-harness child was ended by its handle at teardown (${await child.stop()})`);
      } catch (e) {
        notes.push(`ending the perf-harness child failed: ${e.message}`);
      }
    }
    if (genDir) {
      try {
        writeFileSync(join(genDir, "stop"), "");
      } catch {
        // the gen dir exists by construction
      }
    }
    if (adapter) {
      try {
        const td = await adapter.teardown();
        for (const s of td.survivors ?? []) notes.push(`teardown left ${s.what ?? `pid ${s.pid}`}: ${s.detail ?? "still running"}`);
      } catch (e) {
        notes.push(`teardown failed: ${e.message}`);
      }
    }
    if (genDir && genRunId) {
      try {
        const reaped = await (deps.reapGenerators ?? defaultReapGenerators)(genDir, genRunId);
        if (reaped.survivors.length > 0) notes.push(`${reaped.survivors.length} generator(s) survived teardown: ${reaped.survivors.map((s) => s.pid).join(", ")}`);
      } catch (e) {
        notes.push(`reaping the generators failed: ${e.message}`);
      }
    }
    try {
      await environment?.stop?.();
    } catch (e) {
      notes.push(`stopping the environment failed: ${e.message}`);
    }
    signals.off("SIGINT", onSignal);
    signals.off("SIGTERM", onSignal);
  }

  const v = settleVerdict(facts);
  result.generatorsStarted = facts.generatorsStarted;
  result.looks = facts.looks;
  result.sessionsSeen = v.sessionsSeen;
  result.verdict = v.verdict;
  result.verdictReason = v.reason;
  if (o.out) writeFileSync(resolve(o.out), `${JSON.stringify(result, null, 2)}\n`);
  return result;
}
