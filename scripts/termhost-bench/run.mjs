/**
 * The termhost-bench run loop (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * Phase 3): doctor -> for every unit: launch -> spawn N sessions (D13, every one
 * `--echo`) -> per arm: arrange, warm up, measure (soak / input / switch / spawn
 * phases, each under its own probe `watch`) -> teardown with read-back -> KPIs from
 * `probe.jsonl` and the generator logs through `stats.mjs` -> one results file,
 * validated before it is written.
 *
 * A UNIT is one host launch for one (run index, N, setup, variant, build role).
 * Units run in the order {@link planUnits} gives: per run index, per N (ascending),
 * per setup, the runner's head and baseline builds back to back — the same-VM
 * interleaving D11 asks for (A, B, A, B …) at the granularity of a whole level.
 * Every unit starts its host fresh and spawns its N sessions fresh, so one level's
 * scrollback, memory and renderer state never leak into the next.
 *
 * Safety, checked rather than assumed:
 *   - D12: the primary runner's `settings.json`, `paired_user.json` and
 *     `auth_tokens.enc` are digested before the run and re-checked before and after
 *     every unit and every arm; a change is an incident that STOPS the run.
 *   - D5/D12 teardown: the stop file ends the generators, the adapter closes what it
 *     started (with read-back), and any generator still carrying this unit's run id
 *     is ended by its slot-file PID, identity re-checked; a survivor is an incident.
 *   - Spawn admission: a resource-guard refusal ends that setup's ladder; the
 *     levels above it are recorded `unknown(admission_refused)`, never dropped.
 *   - Every unknown carries its reason; nothing unknown becomes 0.
 */

import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";

import { AdapterRefusal, createAdapter as defaultCreateAdapter } from "./adapters/index.mjs";
import { ZONE_SETTLE_MS, isPortListening } from "./adapters/runner.mjs";
import { PRIMARY_RUNNER_PORT } from "../lib/runner-clients.mjs";
import { doctorNotes, gatherDoctorFacts, mapDoctor, toCapabilities, blockingMechanisms } from "./doctor.mjs";
import { environmentSpec } from "./envs/index.mjs";
import {
  allUnknown,
  CAPTURE_LOST_SLACK_MS,
  captureInfo,
  captureLosses,
  clockGate,
  computeRowKpis,
  describeStall,
  indexGenerators,
  indexProbeSegment,
  LOSS_DETAIL_MAX,
  lossGroups,
  injectAbortReason,
  nsToMs,
  paneStalls,
  primaryFrames,
  segmentWindow,
  stallCoverage,
  STALL_MIN_MS,
  STALL_VOIDED_KPIS,
  summarizeLosses,
} from "./kpis.mjs";
import { machineBlock, sampleBackgroundLoad } from "./machine.mjs";
import { NeutralityGuard, neutralityVerdict } from "./neutrality.mjs";
import * as probeLib from "./probe.mjs";
import { carriesFlag, listProcesses, terminatePid } from "./proc.mjs";
import { loadProfile } from "./profile.mjs";
import { maskReadings, readingFreeDetail } from "./report.mjs";
import { buildDocument, convertProbeIncident, finalizeLadder, incident, redactRunnerOnly, unprovenBuild, writeResultsOrSetAside } from "./results.mjs";
import { createRunDir, createUnitDir, generatorArgv, generatorRunId, makeRunId } from "./rundir.mjs";
import { echoCount, readGeneratorLog, readGeneratorLogs, readSlots, slotForHostId, waitForGenerators, waitForNewSlot } from "./sessions.mjs";

const GENERATOR_FLAG = "--termhost-bench-generator";

/** Defaults for the measurement phases. */
/** The restore check judges a session only from at least this many kept primary frames showing its marker (with STALL_MIN_MS of span, near the watch end). */
export const RESTORE_CHECK_MIN_FRAMES = 10;

/**
 * Why the restore check cannot judge a session from its {@link stallCoverage}
 * entry over a watch of `windowMs`, or null when it can: >= RESTORE_CHECK_MIN_FRAMES
 * kept frames, spanning >= STALL_MIN_MS, the last within STALL_MIN_MS of the
 * watch's end, and the marker observed ADVANCING within the final STALL_MIN_MS
 * of those frames (its final still run, outside excluded spans, is shorter than
 * that; a backwards step is not an advance). `emitted` is how
 * many output markers the session's generator wrote during that final run, or
 * null without a generator log; it is only reported.
 */
/** Output markers a generator emitted in (a, b] on the probe clock; null without a generator log. */
export function outputMarkersEmitted(gen, a, b) {
  if (!gen) return null;
  return (gen.markers ?? []).filter((m) => m.kind === "output" && typeof m.tEmit === "bigint" && m.tEmit > a && m.tEmit <= b).length;
}

export function restoreCoverageGap(c, windowMs, emitted = null) {
  if (!c || c.frames === 0) return "no kept frame";
  if (c.frames < RESTORE_CHECK_MIN_FRAMES) return `fewer than ${RESTORE_CHECK_MIN_FRAMES} kept frames`;
  if (c.lastMs - c.firstMs < STALL_MIN_MS) return `kept frames span under ${STALL_MIN_MS} ms`;
  if (windowMs - c.lastMs > STALL_MIN_MS) return `last kept frame over ${STALL_MIN_MS} ms before the watch's end`;
  const held = c.finalHeldMs;
  if (held >= STALL_MIN_MS) {
    const wrote = emitted === null ? "no generator log, so how many markers it wrote is unknown" : `its generator wrote ${emitted} marker(s) meanwhile`;
    return `marker did not advance in the final ${Math.round(held)} ms of its kept frames (seq ${c.finalSeq} from +${Math.round(c.finalFromMs)} ms; ${wrote})`;
  }
  return null;
}

export const RUN_DEFAULTS = Object.freeze({
  warmupMs: 10000,
  soakMs: 60000,
  inputKeys: 120,
  inputIntervalMs: 100,
  switchSamples: 20,
  switchSettleMs: 1200,
  spawnSamples: 20,
  spawnTimeoutMs: 30000,
  // The focus arm's restore-after-maximize regression check (gates nothing).
  restoreCheckMs: 5000,
  profile: "claude-tui",
  seed: 1,
});

/** A run that started but produced nothing usable (exit 1, cli.mjs). */
export class RunFailed extends Error {
  constructor(message) {
    super(message);
    this.name = "RunFailed";
  }
}

export class RunAbort extends Error {}

/**
 * The units of a run, in execution order.
 *
 * @param {{setups: string[], ns: number[], runs: number, runnerBuilds?: object[],
 *   runnerVariants?: string[]}} o
 * @returns {{index: number, runIndex: number, n: number, setup: string, variant: string,
 *   role: string, build: object | null}[]}
 */
export function planUnits({ setups, ns, runs, runnerBuilds = [{ role: "single" }], runnerVariants = ["default"] }) {
  const units = [];
  const levels = [...new Set(ns)].sort((a, b) => a - b);
  let index = 0;
  for (let runIndex = 0; runIndex < runs; runIndex += 1) {
    for (const n of levels) {
      for (const setup of setups) {
        const variants = setup === "runner" ? runnerVariants : ["default"];
        const builds = setup === "runner" ? runnerBuilds : [null];
        for (const variant of variants) {
          for (const build of builds) {
            units.push({ index: index++, runIndex, n, setup, variant, role: build?.role ?? "none", build });
          }
        }
      }
    }
  }
  return units;
}

/**
 * The real probe driver; tests substitute a fake with the same calls. Every
 * `watch` and `inject` writes one run per file (`records.rs` `FileMode::New`):
 * `startWatch({label})` sets its `probe.jsonl` aside as `probe-<label>.jsonl` when
 * it stops, and `takeInject(runDir, label)` reads `inject.jsonl` and sets it aside.
 */
export function realProbeDriver({ probePath, envVars = {}, log = () => {} }) {
  const env = { ...process.env, ...envVars };
  return {
    path: probePath,
    startWatch: (o) =>
      new probeLib.WatchProcess({
        probe: probePath,
        args: probeLib.watchArgs({ ...o, stdinStop: true }),
        runDir: o.runDir,
        label: o.label,
        env,
        log,
      }),
    inject: (o) => probeLib.runProbe({ probe: probePath, args: probeLib.injectArgs(o), env, timeoutMs: o.timeoutMs ?? 120000 }),
    takeInject: (runDir, label) => probeLib.takeRunFile(runDir, "inject", label),
    look: (o) => probeLib.look({ probe: probePath, env, ...o }),
    activate: (o) => probeLib.activate({ probe: probePath, env, ...o }),
  };
}

/**
 * End every generator still carrying `genRunId`: first wait for them to leave on
 * the stop file, then signal by slot-file PID with the identity re-checked, then
 * read back the WHOLE process table for anything still carrying the run id.
 *
 * @returns {Promise<{ended: number[], survivors: {pid: number, cmdline: string}[]}>}
 */
export async function reapGenerators(genDir, genRunId, { list = () => listProcesses(), waitMs = 10000, sleepFn = (ms) => new Promise((r) => setTimeout(r, ms)), kill } = {}) {
  const mine = (p) => carriesFlag(p, GENERATOR_FLAG, genRunId);
  const slots = [...readSlots(genDir).values()];
  const deadline = Date.now() + waitMs;
  let alive = [];
  for (;;) {
    const table = list();
    alive = slots.filter((s) => table.has(s.pid) && mine(table.get(s.pid)));
    if (alive.length === 0 || Date.now() >= deadline) break;
    await sleepFn(250);
  }
  const ended = [];
  for (const signal of ["SIGTERM", "SIGKILL"]) {
    if (alive.length === 0) break;
    const table = list();
    for (const s of alive) {
      const r = terminatePid(s.pid, { signal, table, expect: mine, ...(kill ? { kill } : {}) });
      if (r.outcome === "signalled") ended.push(s.pid);
    }
    await sleepFn(1500);
    const after = list();
    alive = alive.filter((s) => after.has(s.pid) && mine(after.get(s.pid)));
  }
  const survivors = [...list().values()].filter(mine).map((p) => ({ pid: p.pid, cmdline: p.cmdline.slice(0, 200) }));
  return { ended: [...new Set(ended)], survivors };
}

/**
 * Labels a row carries (Phase 1 ROW_LABELS). `envLabels` are the ones the
 * environment declares for this setup (`envs/index.mjs` `setupLabels`, e.g.
 * D6's `dom_renderer_fallback` on VS Code rows of a linux-xvfb run).
 */
export function rowLabels({ softwareRendered, setup, smoke, paneSizes, geometry, envLabels = [] }) {
  const labels = [];
  if (softwareRendered) labels.push("software_rendered");
  if (setup.id === "runner" && (setup.build === undefined || setup.build?.state)) labels.push("unknown_build");
  if (smoke) labels.push("assert_only_smoke");
  if ((paneSizes ?? []).some((p) => p.cols < geometry.cols || p.rows < geometry.rows)) labels.push("pane_smaller_than_profile");
  for (const l of envLabels) if (!labels.includes(l)) labels.push(l);
  return labels;
}

/**
 * A free-text detail as the PUBLIC job log may carry it: the runner's verbatim, a
 * third party's with every reading masked (D7, {@link readingFreeDetail}). The
 * incident records keep the verbatim text; redaction drops a third party's from
 * every upload.
 */
export function publicDetail(setupId, detail) {
  return setupId === "runner" ? String(detail) : readingFreeDetail(detail);
}

/**
 * The log lines for a row's KPI notes. A note can quote a reading; public CI
 * logs carry these lines, and a third party's readings never leave the job (D7),
 * so only the runner's notes are logged — every other setup's stay in the file.
 */
export function noteLogLines(setupId, where, notes) {
  return setupId === "runner" ? notes.map((n) => `${where}: ${n}`) : [];
}

/**
 * The run's sleep: it ends at the abort, and after the abort it still YIELDS (one
 * setImmediate turn) rather than resolving as a bare microtask, so a loop that
 * missed the abort cannot spin without the event loop servicing a second signal
 * or the probe's pipes. The real sleep clears its own timer on abort, so nothing
 * lingers after the run; an injected `inner` sleep (a virtual clock) is raced
 * against the abort.
 *
 * @param {AbortSignal} signal
 * @param {((ms: number) => Promise<void>) | null} [inner]
 */
export function abortAwareSleep(signal, inner = null) {
  const yieldOnce = () => new Promise((r) => setImmediate(r));
  if (inner) {
    const abortedPromise = new Promise((r) => signal.addEventListener("abort", () => r(), { once: true }));
    return (ms) => (signal.aborted ? yieldOnce() : Promise.race([inner(ms), abortedPromise]));
  }
  return (ms) =>
    new Promise((r) => {
      if (signal.aborted) return setImmediate(r);
      const done = () => {
        clearTimeout(timer);
        signal.removeEventListener("abort", done);
        r();
      };
      const timer = setTimeout(done, ms);
      signal.addEventListener("abort", done, { once: true });
    });
}

/** The launch mode a setup's unit was planned with, from the run's options. */
export function fallbackLaunchMode(setupId, o = {}) {
  if (setupId === "runner") return o.runnerMode === "supervisor" ? "supervisor_spawn_test" : "subject_instance_root";
  if (setupId === "vscode") return o.vscodeAppPath ? "installed_app_path" : "portable_pinned";
  if (setupId === "windows-terminal") return "portable_pinned";
  if (setupId === "tmux" || setupId === "tmux-wsl") return "private_server";
  // A setup added to SETUP_IDS without a launch mode here is a programming error,
  // not a row to label with a plausible guess.
  throw new Error(`fallbackLaunchMode: no launch mode for setup ${JSON.stringify(setupId)}`);
}

/**
 * A fallback `setup` object when an adapter could not describe itself (or was
 * never launched). What the plan already knows is kept: the runner build the unit
 * was FOR (role, and the requested commit and source), so head and baseline rows
 * stay distinct, and the launch mode the run actually used. The build stays
 * UNPROVEN (D12): nothing ran that could prove it.
 */
export function fallbackSetup(unit, detail, o = {}) {
  const absent = { state: "unknown", reason: "harness_error", detail };
  const b = unit.build;
  const build =
    unit.setup !== "runner"
      ? { state: "not_applicable", reason: "not_applicable_to_setup" }
      : unprovenBuild(
          { ...(b ?? {}), source: b?.source ?? (o.runnerMode === "supervisor" ? "git_ref" : "artifact") },
          `${detail}; the build was not proven (D12)`,
        );
  return {
    id: unit.setup,
    variant: unit.variant,
    app_version: absent,
    build,
    settings_digest: absent,
    pty_host_impl: absent,
    render_backend: absent,
    pty_holder: absent,
    launch_mode: fallbackLaunchMode(unit.setup, o),
    session_shell: "unknown",
    shell_integration: absent,
  };
}

/**
 * Run the bench.
 *
 * @param {object} opts normalised CLI options (see cli.mjs)
 * @param {object} [deps] injection points for tests
 * @returns {Promise<{doc: object, outPath: string, redactedPath: string | null, runDir: string, stopped: object | null}>}
 */
export async function runBench(opts, deps = {}) {
  const log = deps.log ?? ((s) => process.stderr.write(`termhost-bench: ${s}\n`));
  const now = deps.now ?? (() => new Date());
  // SIGINT / SIGTERM (Ctrl+C, a CI step timeout) end the run, never abandon it: every
  // sleep returns at once, the unit in progress skips its remaining phases and arms
  // and tears down, and the units completed so far are written as a valid file whose
  // incident says it is partial. The completed units are also written at the moment
  // the signal arrives, in case a second, harder signal follows (GitHub Actions sends
  // SIGINT, then SIGTERM about 7.5 s later, then SIGKILL); a second signal exits at once.
  const abort = new AbortController();
  const aborted = () => abort.signal.aborted;
  const sleep = abortAwareSleep(abort.signal, deps.sleep ?? null);
  const signals = deps.signals ?? process;
  const exit = deps.exit ?? (signals === process ? (code) => process.exit(code) : null);
  // Adapters launched and not yet torn down: a second signal still stops the temp
  // runners among them whose id is known (`emergencyStop`), bounded in time.
  const live = new Set();
  let hardExiting = false;
  let writePartial = () => {};
  const onSignal = (sig) => {
    if (aborted()) {
      if (hardExiting || !exit) return;
      hardExiting = true;
      log(`received a second signal (${sig}): stopping the temp runners already known, then exiting without the rest of the teardown`);
      const stops = [...live].map((a) =>
        Promise.resolve()
          .then(() => a.emergencyStop?.())
          .then((what) => what && log(`second signal: ${what}`))
          .catch((e) => log(`second signal: emergency stop failed: ${e.message}`)),
      );
      let timer;
      const bound = new Promise((r) => (timer = setTimeout(r, deps.hardExitGraceMs ?? 3000)));
      Promise.race([Promise.all(stops), bound]).finally(() => {
        clearTimeout(timer);
        exit(130);
      });
      return;
    }
    log(`received ${sig}: ending the unit in progress and writing the units completed so far`);
    abort.abort(sig);
    try {
      writePartial(sig);
    } catch (e) {
      log(`could not write the partial results at the signal: ${e.message}`);
    }
  };
  signals.on("SIGINT", onSignal);
  signals.on("SIGTERM", onSignal);
  try {
    return await runBenchInner(opts, deps, { log, now, sleep, aborted, abort, live, setWritePartial: (fn) => (writePartial = fn) });
  } finally {
    signals.off("SIGINT", onSignal);
    signals.off("SIGTERM", onSignal);
  }
}

async function runBenchInner(opts, deps, { log, now, sleep, aborted, abort, live = new Set(), setWritePartial }) {
  const spec = environmentSpec(opts.env);
  if (spec.notBuilt) throw new RunAbort(spec.notBuilt);
  const o = { ...RUN_DEFAULTS, ...opts };
  const { profile, digest: profileDigest } = loadProfile(o.profile);
  const runId = o.runId ?? makeRunId(now());
  const runDir = deps.runDir ?? createRunDir({ runId, base: o.workDir });
  const startedAt = now().toISOString();
  log(`run ${runId} in ${runDir}`);

  const backgroundLoad = await (deps.sampleBackgroundLoad ?? sampleBackgroundLoad)(1000);
  const environment = await (deps.startEnvironment ?? spec.start)({ log });
  let outPath = o.out ?? join(runDir, "results.json");
  try {
    const probeFound = deps.probePath
      ? { path: deps.probePath, source: deps.probeSource ?? "explicit", tried: [deps.probePath] }
      : (deps.locateProbe ?? probeLib.locateProbeBinary)({ explicit: o.probe });
    // Pin it before the doctor runs it: the doctor, every probe call and run.probe
    // all describe this one copy, whatever happens to the original meanwhile.
    let pinned = null;
    if (probeFound.path) {
      try {
        pinned = (deps.pinProbe ?? probeLib.pinProbe)(probeFound.path, runDir);
      } catch (e) {
        throw new RunAbort(`cannot pin the probe binary ${probeFound.path} into the run directory: ${e.message}`);
      }
    }
    const probePath = pinned?.path ?? null;
    const createAdapter = deps.createAdapter ?? defaultCreateAdapter;
    const adapterOptions = (unit) => adapterOptionsFor(unit, o, log);

    // ---- doctor ------------------------------------------------------------
    const checkAdapters = [];
    for (const setup of o.setups) {
      const unit = planUnits({ setups: [setup], ns: [o.ns[0]], runs: 1, runnerBuilds: o.runnerBuilds, runnerVariants: o.runnerVariants })[0];
      checkAdapters.push(await createAdapter(setup, adapterOptions(unit)));
    }
    const facts = await (deps.gatherDoctorFacts ?? gatherDoctorFacts)({
      environment: o.env,
      spec,
      probePath,
      probeTried: probeFound.tried,
      probeSource: probeFound.source,
      probeOriginalPath: probeFound.path,
      adapters: checkAdapters,
      envVars: environment.env,
      display: { started: true, detail: environment.display ?? "started" },
    });
    const mechanisms = mapDoctor(facts);
    // Informational only: a foreground change the probe did not cause never blocks.
    for (const n of doctorNotes(facts)) log(`doctor note (informational): ${n}`);
    const capabilities = toCapabilities(mechanisms, now().toISOString());
    const blocking = blockingMechanisms(mechanisms);
    if (blocking.length > 0) {
      throw new RunAbort(`the doctor reports a run cannot start here: ${blocking.map((b) => `${b.id} ${b.state} (${b.reason})`).join("; ")}`);
    }
    const probe = deps.probe ?? (deps.probeDriver ?? realProbeDriver)({ probePath, envVars: environment.env, log });

    const neutrality = deps.neutrality ?? new NeutralityGuard(o.primaryStatePaths ? { paths: o.primaryStatePaths } : {});
    const base = neutrality.baseline();
    // The guard's baseline goes into the results, and a guard that would watch
    // nothing while a primary may be present is UNKNOWN, never a pass (D12).
    const primaryPresent = await (deps.primaryPresent ?? (() => isPortListening(PRIMARY_RUNNER_PORT)))();
    const nv = neutralityVerdict(base, primaryPresent);
    capabilities["harness.neutrality"] = { state: nv.state, reason: nv.reason, checked_at: now().toISOString() };
    if (nv.state !== "OPERATIVE" && o.runnerMode === "supervisor" && o.setups.includes("runner")) {
      throw new RunAbort(`the primary-state guard cannot protect this supervisor box: ${nv.reason}`);
    }
    const unreadableReported = new Set();
    const noteUnreadable = (res, stage) => {
      for (const label of res.unreadable ?? []) {
        if (unreadableReported.has(label)) continue;
        unreadableReported.add(label);
        incidents.push(incident("harness_error", `the primary-state guard could not read ${label} ${stage}: a change to it would not be seen`, { data: { file: label, stage } }));
      }
    };

    // ---- units --------------------------------------------------------------
    const units = planUnits({ setups: o.setups, ns: o.ns, runs: o.runs, runnerBuilds: o.runnerBuilds, runnerVariants: o.runnerVariants });
    const rows = [];
    const incidents = [];
    // A refusal ends ONE ladder: this subject's, in this run index, from the refused N up.
    const ladderEnded = new Map();
    let stopped = null;
    let completedUnits = 0;
    const refreshSeen = { hz: null, known: false, source: null };
    const ctxBase = {
      runId,
      runDir,
      environment,
      probe,
      log,
      sleep,
      now,
      capabilities,
      profile,
      profileDigest,
      o,
      spec,
      neutrality,
      refreshSeen,
      reapGenerators: deps.reapGenerators,
      aborted,
      abortSignal: abort?.signal,
      live,
    };

    // What the signal interrupted, captured when it arrived.
    let atSignal = null;
    const interruption = (sig) =>
      incident(
        "harness_error",
        `the run was interrupted by ${sig} after ${atSignal?.completed ?? completedUnits} of ${units.length} unit(s) had completed` +
          (atSignal?.inUnit !== null && atSignal?.inUnit !== undefined ? `, during unit ${atSignal.inUnit}, whose arms cut short read unknown` : "") +
          ": this file holds no unit run after it",
        { stopsRun: true, data: { signal: String(sig), units_completed: atSignal?.completed ?? completedUnits, units_planned: units.length } },
      );
    const assemble = (rowsNow, incidentsNow) => {
      const finalRows = structuredClone(rowsNow);
      finalizeLadder(finalRows);
      const machine = (deps.machineBlock ?? machineBlock)({
        environment: o.env,
        screen: environment.screen ? { width: environment.screen.width, height: environment.screen.height, scalePct: environment.screen.scalePct ?? 100 } : null,
        refresh: refreshSeen,
        backgroundLoad,
      });
      return buildDocument({
        run: {
          id: runId,
          started_at: startedAt,
          finished_at: now().toISOString(),
          harness_commit: harnessCommit(o.repoRoot),
          // Which probe binary decoded this run's frames (it may not be this checkout's build).
          probe: probeLib.probeIdentity(probeFound, { pinned, codecVersion: facts.probeDoctor?.record?.codec?.version ?? null }),
          node_version: process.version,
          command: o.command ?? ["node", "scripts/termhost-bench/cli.mjs", "run"],
        },
        machine,
        capabilities,
        rows: finalRows,
        incidents: incidentsNow,
      });
    };
    const aside = join(runDir, "results.invalid.json");
    // At a signal: write what is complete NOW (synchronously), before the unit in
    // progress has torn down, in case a harder signal follows the first.
    let currentUnit = null;
    setWritePartial((sig) => {
      atSignal = { completed: completedUnits, inUnit: currentUnit };
      if (rows.length === 0) return;
      writeResultsOrSetAside(outPath, assemble(rows, [...incidents, interruption(sig)]), aside);
      log(`wrote ${outPath} with the ${completedUnits} unit(s) completed before ${sig}`);
    });

    for (const unit of units) {
      if (stopped) break;
      if (aborted()) break;
      const subject = `${unit.setup}|${unit.variant}|${unit.role}`;
      const ladderKey = `${subject}|r${unit.runIndex}`;
      const ended = ladderEnded.get(ladderKey);
      if (ended && unit.n >= ended.n) {
        rows.push(
          ...unknownRows(
            unit,
            o,
            ctxBase,
            "admission_refused",
            `the ${subject} ladder of run ${unit.runIndex} ended at N = ${ended.n} (${ended.detail})`,
            fallbackSetup(unit, "not launched: the ladder ended", o),
          ),
        );
        completedUnits += 1;
        continue;
      }
      const pre = neutrality.check(`before unit ${unit.index} (${subject}, N = ${unit.n})`);
      noteUnreadable(pre, `before unit ${unit.index}`);
      if (pre.changed) {
        incidents.push(pre.incident);
        stopped = pre.incident;
        break;
      }
      currentUnit = unit.index;
      const res = await runUnit(unit, { ...ctxBase, createAdapter, adapterOptions });
      currentUnit = null;
      rows.push(...res.rows);
      incidents.push(...res.incidents);
      completedUnits += 1;
      if (res.admission) ladderEnded.set(ladderKey, { n: unit.n, detail: res.admission.detail ?? res.admission.lane });
      if (res.stop) {
        stopped = res.stop;
        break;
      }
      const post = neutrality.check(`after unit ${unit.index} (${subject}, N = ${unit.n})`);
      noteUnreadable(post, `after unit ${unit.index}`);
      if (post.changed) {
        incidents.push(post.incident);
        stopped = post.incident;
        break;
      }
    }
    if (aborted() && !stopped) {
      const inc = interruption(abort.signal.reason ?? "a signal");
      incidents.push(inc);
      stopped = inc;
    }

    if (rows.length === 0) throw new RunFailed(`no row was produced${stopped ? `: ${stopped.detail}` : ""}`);
    const doc = assemble(rows, incidents);
    writeResultsOrSetAside(outPath, doc, aside);
    log(`wrote ${outPath} (${doc.rows.length} rows, ${incidents.length} incidents)`);
    let redactedPath = null;
    if (o.redactedOut) {
      const red = redactRunnerOnly(doc);
      if (red) {
        writeResultsOrSetAside(o.redactedOut, red, join(runDir, "redacted.invalid.json"));
        redactedPath = o.redactedOut;
        log(`wrote the redacted (runner rows only) copy ${redactedPath}`);
      }
    }
    return { doc, outPath, redactedPath, runDir, stopped };
  } finally {
    await environment.stop?.();
  }
}

/** Adapter construction options for one unit. */
export function adapterOptionsFor(unit, o, log) {
  if (unit.setup === "runner") {
    return {
      variant: unit.variant,
      mode: o.runnerMode,
      build: unit.build,
      supervisorUrl: o.supervisorUrl,
      repoRoot: o.repoRoot,
      extraEnv: o.runnerEnv ?? {},
      log,
    };
  }
  if (unit.setup === "vscode") {
    return { variant: unit.variant, log, appPath: o.vscodeAppPath ?? null, cacheDir: o.vscodeCacheDir ?? null };
  }
  if (unit.setup === "windows-terminal" || unit.setup === "tmux-wsl") {
    return { variant: unit.variant, log, cacheDir: o.wtCacheDir ?? null, ...(unit.setup === "tmux-wsl" ? { distro: o.wslDistro ?? null } : {}) };
  }
  return { variant: unit.variant, log };
}

/** The harness checkout's commit, or an honest absence. */
export function harnessCommit(repoRoot, run = spawnSync) {
  const res = run("git", ["-C", repoRoot ?? ".", "rev-parse", "HEAD"], { encoding: "utf8", timeout: 10000 });
  const sha = String(res.stdout ?? "").trim();
  return /^[0-9a-f]{40}$/.test(sha) ? sha : { state: "unknown", reason: "harness_error", detail: "git rev-parse HEAD failed" };
}

function scenarioFor(unit, arm, o, ctx, { vTarget, vActual }) {
  return {
    profile: ctx.profile.id,
    profile_calibrated: ctx.profile.calibrated,
    crosscheck_profile: ctx.spec.crosscheckProfile,
    profile_provenance: ctx.profile.provenance.kind,
    profile_digest: ctx.profileDigest,
    n: unit.n,
    arm,
    v_target: Math.min(vTarget, unit.n),
    v_actual: Number.isInteger(vActual) && vActual <= unit.n ? vActual : { state: "unknown", reason: "sessions_not_rendered", detail: vActual === null || vActual === undefined ? "no frame measured V" : `V ${vActual} exceeds N` },
    warmup_ms: o.warmupMs,
    soak_ms: o.soakMs,
    seed: o.seed,
    run_index: unit.runIndex,
    run_count: o.runs,
  };
}

function defaultVTarget(unit, arm) {
  return arm === "focus" ? 1 : Math.min(unit.n, 9);
}

/** Rows for a unit that measured nothing: every KPI unknown with one reason. */
function unknownRows(unit, o, ctx, reason, detail, setup, extraIncidents = []) {
  return o.arms.map((arm) => ({
    setup,
    scenario: scenarioFor(unit, arm, o, ctx, { vTarget: defaultVTarget(unit, arm), vActual: null }),
    kpis: allUnknown(reason, { detail, setupId: unit.setup }),
    diagnostics: {},
    labels: rowLabels({
      softwareRendered: ctx.spec.softwareRendered,
      setup,
      smoke: o.smokeSetups?.includes(unit.setup),
      paneSizes: [],
      geometry: ctx.profile.geometry,
      envLabels: ctx.spec.setupLabels?.[unit.setup] ?? [],
    }),
    incidents: extraIncidents,
  }));
}

/**
 * One unit: launch, spawn, every arm, teardown, then KPIs.
 */
export async function runUnit(unit, ctx) {
  const { o, log, probe, sleep } = ctx;
  const { unitDir, genDir, name } = createUnitDir(ctx.runDir, unit);
  const genRunId = generatorRunId(ctx.runId, unit.index);
  const incidents = [];
  const arms = new Map();
  const armErrors = new Map();
  // An arrangement the harness failed, as a run-level incident (see runArm). The
  // measure and spawn passes arrange each arm twice, so one root cause would be
  // two incidents: the spawn pass's is dropped when its detail repeats the
  // measure pass's.
  const arrangeSeen = new Set();
  const recordArrangeFailure = (arm, f, pass = "") => {
    if (!f) return;
    // A third party's is keyed on its reading-free text (two passes failing the
    // same way with different numbers — a timeout's elapsed ms — are one cause:
    // its numbers never leave the VM, so they cannot tell causes apart for the
    // log either); the runner's on its verbatim text. Never truncated.
    const key = `${arm}\u0000${unit.setup === "runner" ? String(f.detail) : maskReadings(f.detail)}`;
    if (arrangeSeen.has(key)) return;
    arrangeSeen.add(key);
    const where = `unit ${name} arm ${arm}${pass}: arrange: `;
    const detail = `${where}${f.detail}`;
    log(`${where}${publicDetail(unit.setup, f.detail)}`);
    incidents.push(incident("harness_error", detail, { data: { setup: unit.setup, unit: name, arm, phase: `${arm}/arrange` } }));
  };
  let adapter = null;
  let failure = null;
  let admission = null;
  let stop = null;
  let slotMap = new Map();
  const baseSessions = new Set();
  log(`unit ${name}: launching`);
  try {
    adapter = await ctx.createAdapter(unit.setup, ctx.adapterOptions(unit));
    ctx.live?.add(adapter);
    await adapter.launch({ runDir: ctx.runDir, unitDir, genDir, genRunId, runId: ctx.runId, unit, env: ctx.environment, signal: ctx.abortSignal });
    const argv = generatorArgv({ genDir, profile: o.profile, seed: o.seed, genRunId });
    const spawned = await adapter.spawnSessions(unit.n, argv);
    if (spawned.admission?.refused) {
      admission = spawned.admission;
      failure = { reason: "admission_refused", detail: `resource guard refused at ${admission.n_reached} session(s) (${admission.lane}): ${admission.detail ?? ""}`.trim() };
      incidents.push(incident("admission_refused", failure.detail, { data: { setup: unit.setup, unit: name, ...admission } }));
    } else {
      const ready = await waitForGenerators(genDir, unit.n, { timeoutMs: 60000 + 4000 * unit.n, signal: ctx.abortSignal });
      if (!ready.ok) failure = { reason: "harness_error", detail: ready.detail };
    }
    const slots = readSlots(genDir);
    for (const s of spawned.sessions) {
      const slot = slotForHostId(slots, adapter.hostIdKey, s.hostId);
      if (slot !== null) {
        slotMap.set(slot, s.hostId);
        baseSessions.add(slot);
      }
    }
    if (!failure && slotMap.size < unit.n) {
      // A host that exports no terminal id: map by slot order (exact only for N = 1).
      const missing = [...slots.keys()].filter((k) => !slotMap.has(k)).sort((a, b) => a - b);
      const unmapped = spawned.sessions.filter((s) => ![...slotMap.values()].includes(s.hostId));
      unmapped.forEach((s, i) => {
        if (missing[i] !== undefined) {
          slotMap.set(missing[i], s.hostId);
          baseSessions.add(missing[i]);
        }
      });
      if (slotMap.size < unit.n) failure = { reason: "harness_error", detail: `only ${slotMap.size} of ${unit.n} sessions could be matched to a generator slot` };
    }
    if (!failure) {
      // The tiled arm measures first: it is the layout an operator gets, and the
      // focus arm's maximize (and any restore after it) changes it.
      for (const arm of measureOrder(o.arms)) {
        if (ctx.aborted?.()) break;
        const pre = ctx.neutrality.check(`before ${name} arm ${arm}`);
        if (pre.changed) {
          incidents.push(pre.incident);
          stop = pre.incident;
          break;
        }
        // One arm's failure is that arm's alone: the arms that measured keep their
        // rows, and the failed one carries its own reason.
        try {
          const measured = await runArm(unit, arm, { ...ctx, adapter, unitDir, genDir, genRunId, slotMap, name }, { only: "measure" });
          arms.set(arm, measured);
          recordArrangeFailure(arm, measured.arrangeFailure);
        } catch (e) {
          const f = e instanceof AdapterRefusal ? { reason: e.reason, detail: e.detail } : { reason: "harness_error", detail: e?.message ?? String(e) };
          armErrors.set(arm, f);
          log(`unit ${name} arm ${arm}: ${f.reason}: ${publicDetail(unit.setup, f.detail)}`);
          if (!(e instanceof AdapterRefusal) || e.reason === "harness_error") {
            incidents.push(incident("harness_error", `unit ${name} arm ${arm}: ${f.detail}`, { data: { setup: unit.setup, unit: name, arm } }));
          }
          if (e instanceof AdapterRefusal && e.incident) incidents.push(e.incident);
        }
        const post = ctx.neutrality.check(`after ${name} arm ${arm}`);
        if (post.changed) {
          incidents.push(post.incident);
          stop = post.incident;
          break;
        }
      }
      // Each arm's spawn phase, only after EVERY arm has measured: spawned sessions
      // change the runner's layout, which a later arm's soak would otherwise watch.
      for (const arm of o.spawnSamples > 0 && !stop ? o.arms : []) {
        const measured = arms.get(arm);
        if (!measured || ctx.aborted?.()) continue;
        try {
          const sp = await runArm(unit, arm, { ...ctx, adapter, unitDir, genDir, genRunId, slotMap, name }, { only: "spawn" });
          recordArrangeFailure(arm, sp.arrangeFailure, " (spawn pass)");
          // A spawn measures the NEW session's first marker, so an arrangement that
          // shows no earlier session does not void it; it is noted, not fatal.
          measured.spawns = sp.spawns ?? (sp.armFailure ? { unavailable: sp.armFailure } : null);
          if (sp.armFailure) measured.arrangeNotes.push(`spawn pass arrange: ${sp.armFailure.reason}: ${sp.armFailure.detail}`);
          if (sp.spawnAdmission) {
            measured.spawnAdmission = sp.spawnAdmission;
            measured.diagnostics = { ...measured.diagnostics, ...sp.diagnostics };
          }
          measured.incidents.push(...sp.incidents);
          if (sp.interrupted) measured.interrupted = true;
        } catch (e) {
          const f = e instanceof AdapterRefusal ? { reason: e.reason, detail: e.detail } : { reason: "harness_error", detail: e?.message ?? String(e) };
          measured.spawns = { unavailable: f };
          log(`unit ${name} arm ${arm} spawn: ${f.reason}: ${publicDetail(unit.setup, f.detail)}`);
          incidents.push(incident("harness_error", `unit ${name} arm ${arm} spawn: ${f.detail}`, { data: { setup: unit.setup, unit: name, arm } }));
        }
        const post = ctx.neutrality.check(`after ${name} arm ${arm} spawn`);
        if (post.changed) {
          incidents.push(post.incident);
          stop = post.incident;
          break;
        }
      }
    }
  } catch (e) {
    failure = e instanceof AdapterRefusal ? { reason: e.reason, detail: e.detail } : { reason: "harness_error", detail: e?.stack ? String(e.message) : String(e) };
    log(`unit ${name}: ${failure.reason}: ${publicDetail(unit.setup, failure.detail)}`);
    if (!(e instanceof AdapterRefusal) || e.reason === "harness_error") {
      incidents.push(incident("harness_error", `unit ${name}: ${failure.detail}`, { data: { setup: unit.setup, unit: name } }));
    }
    if (e instanceof AdapterRefusal && e.incident) incidents.push(e.incident);
  }

  // ---- teardown (always) ----------------------------------------------------
  let setup = null;
  let unitDiagnostics = {};
  try {
    writeFileSync(join(genDir, "stop"), "");
  } catch {
    // the gen dir exists by construction
  }
  try {
    setup = adapter ? adapter.describe(readSlots(genDir)) : null;
  } catch (e) {
    setup = fallbackSetup(unit, `describe() failed: ${e.message}`, o);
  }
  if (adapter) {
    try {
      const td = await adapter.teardown();
      ctx.live?.delete(adapter);
      // An adapter's own teardown findings (tmux-wsl: a WSL clock drift).
      incidents.push(...(td.incidents ?? []).map((i) => ({ ...i, data: { ...(i.data ?? {}), setup: unit.setup, unit: name } })));
      for (const s of td.survivors ?? []) {
        incidents.push(incident("harness_error", `unit ${name}: teardown left ${s.what ?? `pid ${s.pid}`}: ${s.detail ?? "still running"}`, { data: { setup: unit.setup, unit: name } }));
      }
      if (unit.setup === "runner" && typeof adapter.coordReadback === "function") {
        const rb = await adapter.coordReadback();
        unitDiagnostics["coord_sessions.readback"] = rb.diagnostic;
        incidents.push(...rb.incidents.map((i) => ({ ...i, data: { ...(i.data ?? {}), setup: unit.setup, unit: name } })));
      }
    } catch (e) {
      incidents.push(incident("harness_error", `unit ${name}: teardown failed: ${e.message}`, { data: { setup: unit.setup, unit: name } }));
    }
  }
  const reaped = await (ctx.reapGenerators ?? reapGenerators)(genDir, genRunId);
  if (reaped.survivors.length > 0) {
    incidents.push(
      incident("generator_survived_teardown", `unit ${name}: ${reaped.survivors.length} generator(s) carrying ${genRunId} survived teardown: ${reaped.survivors.map((s) => s.pid).join(", ")}`, {
        data: { setup: unit.setup, unit: name, survivors: reaped.survivors },
      }),
    );
  }
  setup = setup ?? fallbackSetup(unit, failure?.detail ?? "the adapter could not be created", o);

  // ---- rows -------------------------------------------------------------------
  if (failure) {
    return { rows: unknownRows(unit, o, ctx, failure.reason, failure.detail, setup, []), incidents, admission, stop };
  }
  const gens = arms.size > 0 ? indexGenerators(readGeneratorLogs(genDir)) : new Map();
  const baseGens = new Map([...gens].filter(([s]) => baseSessions.has(s)));
  // tmux-wsl: the WSL-side and WSL VM samples its adapter recorded (wsl.mjs).
  const wslSamples = typeof adapter?.wslSamples === "function" ? adapter.wslSamples() : null;
  const rows = [];
  for (const arm of o.arms) {
    const a = arms.get(arm);
    const armError = armErrors.get(arm);
    if (armError) {
      rows.push(...unknownRows({ ...unit }, { ...o, arms: [arm] }, ctx, armError.reason, armError.detail, setup));
      continue;
    }
    if (!a) {
      const why = stop ? `the run stopped: ${stop.detail}` : ctx.aborted?.() ? "the run was interrupted before this arm" : "the arm did not run";
      rows.push(...unknownRows({ ...unit }, { ...o, arms: [arm] }, ctx, "harness_error", why, setup));
      continue;
    }
    if (a.interrupted) {
      rows.push(...unknownRows({ ...unit }, { ...o, arms: [arm] }, ctx, "harness_error", "the run was interrupted during this arm: its phases were cut short", setup));
      continue;
    }
    const computed = computeRowKpis({
      environment: o.env,
      crosscheckProfile: ctx.spec.crosscheckProfile,
      setupId: unit.setup,
      n: unit.n,
      arm,
      vTarget: a.vTarget,
      capabilities: ctx.capabilities,
      gens: baseGens,
      soak: a.soak,
      armFailure: a.armFailure,
      input: a.input,
      switches: a.switches,
      spawns: a.spawns ? { ...a.spawns } : null,
      switchTimeoutMs: 5000,
      spawnTimeoutMs: o.spawnTimeoutMs,
      wsl: wslSamples,
    });
    noteRefresh(ctx.refreshSeen, a.soak);
    // What the marker KPIs actually carry, read from the row (a wedge does not
    // replace every reason: clock_unaligned, for one, keeps precedence).
    const markerKpis = STALL_VOIDED_KPIS.map((id) => {
      const v = computed.kpis[id];
      return `${id} ${v?.state === "measured" ? "measured" : `${v?.state ?? "missing"}(${v?.reason ?? "?"})`}`;
    }).join(", ");
    for (const st of computed.stalls ?? []) {
      // Each verdict reads the marker KPIs off the row: a stall that voids nothing
      // still leaves them as they were (a capture-health unknown, say), never
      // "measured" by assumption.
      const verdict = st.recovered
        ? `a hitch the pane recovered from (not voided); its marker KPIs read ${markerKpis}`
        : st.voids
          ? `the pane did not repaint after that; its marker KPIs read ${markerKpis}`
          : `still at the soak's end on too short a frameless tail to call a wedge (not voided); its marker KPIs read ${markerKpis}`;
      a.incidents.push(
        incident("pane_stalled", `unit ${name} arm ${arm}: soak: ${describeStall(st)}: ${verdict}`, {
          data: { setup: unit.setup, unit: name, arm, session: st.session, recovered: st.recovered, voids: st.voids },
        }),
      );
    }
    for (const line of noteLogLines(unit.setup, `unit ${name} arm ${arm}`, computed.notes)) log(line);
    const diagnostics = unit.setup === "runner" ? { ...a.diagnostics, ...unitDiagnostics, ...zoneDiagnostic(a) } : {};
    const kpis = computed.kpis;
    rows.push({
      setup,
      scenario: scenarioFor(unit, arm, o, ctx, { vTarget: a.vTarget, vActual: computed.vActual }),
      kpis,
      diagnostics,
      labels: rowLabels({
        softwareRendered: ctx.spec.softwareRendered,
        setup,
        smoke: o.smokeSetups?.includes(unit.setup),
        paneSizes: a.paneSizes,
        geometry: ctx.profile.geometry,
        envLabels: ctx.spec.setupLabels?.[unit.setup] ?? [],
      }),
      incidents: a.incidents,
    });
  }
  return { rows, incidents, admission, stop };
}

function zoneDiagnostic(a) {
  if (!a.layout) return {};
  return {
    "zones.layout": { comparable: false, source: "get-zone-view-state + probe marker positions", state: "measured", data: { ...a.layout, notes: a.arrangeNotes ?? [] } },
  };
}

function noteRefresh(seen, soak) {
  if (seen.known || !soak?.records) return;
  const cap = soak.records.find((r) => r?.type === "capture" && r.state === "started" && r.refresh_hz !== undefined);
  if (!cap) return;
  const assumed = cap.refresh_assumed === true;
  seen.hz = typeof cap.refresh_hz === "number" ? cap.refresh_hz : null;
  seen.source = cap.refresh_source ?? null;
  seen.known = !assumed && seen.hz > 0;
}

/**
 * One arm of one unit: arrange, warm up, then the four measurement phases.
 */
/**
 * The order the arms are MEASURED in: tiled before focus, whatever order they
 * were given in (rows keep the given order).
 */
export function measureOrder(arms) {
  return [...arms].sort((a, b) => (a === "tiled" ? 0 : 1) - (b === "tiled" ? 0 : 1));
}

/**
 * One arm of a unit: arrange, then its phases. `only` splits the arm into two
 * passes — `measure` (soak, input, switch, diagnostics) and `spawn` — so runUnit
 * can measure every arm before any arm spawns: a spawned session changes the
 * runner's zone layout, and an arm measured after a spawn phase watched that
 * damage instead of its own layout (CI run 38345475088: the tiled soak, run after
 * the focus arm's spawns, saw no session at all). Omitted, the arm runs whole.
 *
 * @param {{only?: "measure" | "spawn"}} [pass]
 */
export async function runArm(unit, arm, ctx, { only } = {}) {
  const { o, log, probe, sleep, adapter, genDir, unitDir, genRunId, slotMap, name } = ctx;
  const measure = only !== "spawn";
  const spawnPass = only !== "measure";
  let looks = 0;
  const target = () => adapter.windowTarget?.() ?? { pid: adapter.processRoots()[0] };
  const look = () => probe.look({ lookDir: join(unitDir, `look-${arm}${only === "spawn" ? "-spawn" : ""}-${looks++}`), target: target(), durationMs: 2500 });
  const arr = await adapter.arrange(arm, { slots: slotMap, n: unit.n, look });
  const out = {
    vTarget: arr.vTarget ?? defaultVTarget(unit, arm),
    armFailure: arr.ok ? null : arr.failure ?? { reason: "harness_error", detail: "arrange failed" },
    arrangeFailure: null,
    paneSizes: arr.paneSizes ?? [],
    layout: arr.layout ?? null,
    arrangeNotes: arr.notes ?? [],
    incidents: [],
    soak: null,
    input: null,
    switches: null,
    spawns: null,
    diagnostics: {},
  };
  // An arrangement the harness failed (not one the host could not realise) is the
  // harness's error like any other. It is handed back as `arrangeFailure` for the
  // caller to record as a RUN-level incident (data.setup set, so redaction keeps
  // a third party's off the uploaded copy), like an arrangement that threw: a
  // row-level incident is not counted in the run's `N incidents`, and CI run
  // 38362046020's `0 incidents` could not rule this path out.
  out.arrangeFailure = out.armFailure?.reason === "harness_error" ? out.armFailure : null;
  const act = await probe.activate({ runDir: join(unitDir, `activate-${arm}${only === "spawn" ? "-spawn" : ""}`), target: target() });
  if (!act.ok) out.arrangeNotes.push(`activation: ${act.detail}`);
  await sleep(o.warmupMs);

  const watchOpts = (phase, extra = {}) => ({
    runDir: genDir,
    label: `${arm}-${phase}`,
    target: target(),
    hostPids: adapter.processRoots(),
    runId: genRunId,
    frames: "all",
    // The environment's cross-check profile, told to the probe (never inferred).
    crosscheckProfile: ctx.spec.crosscheckProfile,
    ...extra,
  });
  const probeIncidents = (records, phase) =>
    records.filter((r) => r?.type === "incident").map((r) => convertProbeIncident(r, { setup: unit.setup, unit: name, phase: `${arm}/${phase}` }));
  // A phase's watch, started and confirmed. One that never wrote its start record
  // (refused, or crashed) is recorded as the harness's error, with its stderr.
  const startPhaseWatch = async (phase, extra) => {
    const w = probe.startWatch(watchOpts(phase, extra));
    if (!(await w.ready())) {
      const why = `(exit ${JSON.stringify(w.exit ?? null)}): ${String(w.stderr ?? "").slice(-300)}`;
      const detail = `unit ${name} arm ${arm}: the ${phase} watch did not start ${why}`;
      log(`unit ${name} arm ${arm}: the ${phase} watch did not start ${publicDetail(unit.setup, why)}`);
      out.incidents.push(incident("harness_error", detail, { data: { setup: unit.setup, unit: name, phase: `${arm}/${phase}` } }));
    }
    return w;
  };
  const stopPhaseWatch = async (w, phase) => {
    const seg = await w.stop();
    if (seg.error) {
      out.incidents.push(incident("harness_error", `unit ${name} arm ${arm}: ${phase}: ${seg.error}`, { data: { setup: unit.setup, unit: name, phase: `${arm}/${phase}` } }));
    }
    out.incidents.push(...probeIncidents(seg.records, phase));
    return seg;
  };

  // ---- soak -------------------------------------------------------------------
  if (measure) {
    log(`unit ${name} arm ${arm}: soak ${o.soakMs} ms`);
    const soakWatch = await startPhaseWatch("soak");
    await sleep(o.soakMs);
    const soak = await stopPhaseWatch(soakWatch, "soak");
    out.soak = { records: soak.records };
  }
  const interrupted = () => ctx.aborted?.() === true;
  // A soak a signal cut short measured a shorter window than its row will say.
  if (interrupted()) out.interrupted = true;

  // ---- input (D9) ---------------------------------------------------------------
  const inputSafe = ctx.capabilities["input.safe"];
  if (!measure) {
    // the spawn pass: this arm's input was measured in its measure pass
  } else if (interrupted()) {
    out.input = { unavailable: { reason: "harness_error", detail: "the run was interrupted before the input phase" } };
  } else if (inputSafe?.state !== "OPERATIVE") {
    out.input = { unavailable: { reason: "input_unsafe", detail: `input.safe is ${inputSafe?.state ?? "UNKNOWN"}: ${inputSafe?.reason ?? "no verdict"}` } };
  } else if (o.inputKeys <= 0) {
    out.input = { unavailable: { reason: "insufficient_samples", detail: "the input arm was disabled (--input-keys 0)" } };
  } else {
    const f = await adapter.focusObserved();
    if (!f.ok) {
      out.input = { unavailable: f.failure ?? { reason: "harness_error", detail: "focusObserved failed" } };
    } else {
      const echoBase = echoCount(readGeneratorLog(genDir, f.session));
      // watch and inject share the gen dir (inject.jsonl announces each key) and,
      // for Windows, one tag, so the watch knows the probe's keys from an operator's.
      const tag = probeLib.freshTag();
      const w = await startPhaseWatch("input", { tag });
      const res = await probe.inject({
        runDir: genDir,
        target: target(),
        count: o.inputKeys,
        intervalMs: o.inputIntervalMs,
        startDelayMs: 300,
        settleMs: 800,
        tag,
        timeoutMs: 60000 + o.inputKeys * (o.inputIntervalMs + 50),
      });
      const seg = await stopPhaseWatch(w, "input");
      const taken = probe.takeInject(genDir, `${arm}-input`);
      if (taken.error) out.incidents.push(incident("harness_error", `unit ${name} arm ${arm}: input: ${taken.error}`, { data: { setup: unit.setup, unit: name } }));
      const injectRecords = taken.records;
      const injects = injectRecords.filter((r) => r?.type === "inject");
      const abort = injectRecords.find((r) => r?.type === "inject_abort") ?? null;
      out.incidents.push(...probeIncidents(injectRecords, "inject"));
      if (injects.length === 0) {
        out.input = {
          unavailable: {
            reason: abort ? injectAbortReason(abort.reason) : "input_unsafe",
            detail: abort
              ? `inject refused or aborted at key ${abort.index} (${abort.reason}): ${abort.detail}`
              : `inject sent no key (exit ${res.code}): ${`${res.stdout}${res.stderr}`.slice(-300)}`,
          },
        };
      } else {
        out.input = { records: seg.records, injects, focusSession: f.session, echoBase, abort };
      }
      out.focusSession = f.session;
    }
  }

  // ---- switch (focus arm) --------------------------------------------------------
  if (!measure) {
    // the spawn pass
  } else if (interrupted()) {
    if (arm === "focus" && unit.n > 1) out.switches = { unavailable: { reason: "harness_error", detail: "the run was interrupted before the switch phase" } };
  } else if (arm === "focus" && unit.n > 1 && o.switchSamples > 0) {
    const candidates = (arr.switchable ?? []).filter((s) => slotMap.has(s));
    if (candidates.length < 2) {
      out.switches = {
        unavailable: {
          reason: unit.setup === "runner" ? "zone_unresolved" : "sessions_not_rendered",
          detail: `only ${candidates.length} session(s) can be shown (resolved zones: ${candidates.join(",") || "none"})`,
        },
      };
    } else {
      const w = await startPhaseWatch("switch");
      const list = [];
      let current = arr.focusSession;
      for (let j = 0, c = 0; j < o.switchSamples; j += 1) {
        if (interrupted()) break;
        let k = candidates[c++ % candidates.length];
        if (k === current) k = candidates[c++ % candidates.length];
        const r = await adapter.show(k);
        if (r.ok) {
          list.push({ k, tCmdNs: r.tCmdNs });
          current = k;
        }
        await sleep(o.switchSettleMs);
      }
      const seg = await stopPhaseWatch(w, "switch");
      out.switches = { records: seg.records, list };
      if (arr.focusSession !== undefined && current !== arr.focusSession) await adapter.show(arr.focusSession);
    }
  }

  // ---- spawn ---------------------------------------------------------------------
  if (!spawnPass) {
    // the measure pass: runUnit runs this arm's spawn phase after every arm measured
  } else if (interrupted()) {
    out.spawns = { unavailable: { reason: "harness_error", detail: "the run was interrupted before the spawn phase" } };
  } else if (o.spawnSamples > 0) {
    const w = await startPhaseWatch("spawn");
    const list = [];
    const known = new Set(readSlots(genDir).keys());
    let refused = null;
    for (let j = 0; j < o.spawnSamples; j += 1) {
      if (interrupted()) break;
      const r = await adapter.spawnOne();
      if (!r.ok) {
        if (r.admission) {
          refused = r.admission;
          break;
        }
        continue;
      }
      const slot = await waitForNewSlot(genDir, known, { key: adapter.hostIdKey, hostId: r.hostId, tCmdNs: r.tCmdNs, timeoutMs: o.spawnTimeoutMs, signal: ctx.abortSignal });
      if (slot !== null) {
        known.add(slot);
        // Let its first marker reach the screen before closing it.
        const until = Date.now() + Math.min(o.spawnTimeoutMs, 15000);
        while (Date.now() < until && !interrupted()) {
          if (readGeneratorLog(genDir, slot).some((x) => x?.type === "marker")) break;
          await sleep(100);
        }
        await sleep(800);
      }
      list.push({ session: slot, tCmdNs: r.tCmdNs, hostId: r.hostId });
      await adapter.closeOne(r.hostId);
      await sleep(300);
    }
    const seg = await stopPhaseWatch(w, "spawn");
    out.spawns = refused && list.length === 0
      ? { unavailable: { reason: "admission_refused", detail: `the resource guard refused session N + 1 (${refused.lane}): ${refused.detail ?? ""}` } }
      : { records: seg.records, list };
    if (refused) out.spawnAdmission = refused;
  }

  // A signal that arrived at any point in this arm cut a phase short: the arm's
  // measurements are not the ones its row would claim.
  if (interrupted()) out.interrupted = true;

  // ---- diagnostics (runner only) ----------------------------------------------
  if (measure && typeof adapter.diagnostics === "function") {
    try {
      const focusSession = out.focusSession ?? arr.focusSession;
      out.diagnostics = await adapter.diagnostics({
        focusTerminalId: slotMap.get(focusSession),
        focusSession,
        genLogPath: Number.isInteger(focusSession) ? join(genDir, `gen-${focusSession}.jsonl`) : undefined,
        visibleTerminalIds: [...slotMap.values()],
      });
    } catch (e) {
      out.diagnostics = { "harness.diagnostics": { comparable: false, source: "adapter.diagnostics()", state: "unknown", reason: "harness_error", detail: e.message.slice(0, 300) } };
    }
  }
  if (out.spawnAdmission) {
    out.diagnostics["spawn.admission_n_plus_1"] = { comparable: false, source: "spawnOne", state: "measured", data: out.spawnAdmission };
  }

  // ---- restore check (focus arm, runner) ---------------------------------------
  // A regression check that gates nothing: restore the maximized zone and watch a
  // few seconds for a pane that does not repaint. CI run 38351621760 found the pane
  // frozen after restore-zone; this keeps the restore path observable in every
  // scheduled run. It runs after the diagnostics, which read the focus layout.
  // Its outcome is the `focus.restore_check` diagnostic: measured (observed, with
  // what was seen) or unknown with why — never an absence that reads as clean.
  if (measure && arm === "focus" && o.restoreCheckMs > 0 && typeof adapter.restoreZone === "function") {
    out.diagnostics["focus.restore_check"] = await restoreCheck();
  }
  return out;

  async function restoreCheck() {
    const source = "restore-zone + a watch of the restored layout";
    const data = { setup: unit.setup, unit: name, arm, phase: "focus/restore-check" };
    // A check that could not look is UNKNOWN, and says so: never a silent clean.
    const unknownCheck = (why) => {
      const detail = `restore check UNKNOWN: ${why}; this is not a clean result`;
      out.incidents.push(incident("harness_error", `unit ${name} arm ${arm}: ${detail}`, { data }));
      log(`unit ${name} arm ${arm}: ${detail}`);
      return { comparable: false, source, state: "unknown", reason: "harness_error", detail };
    };
    try {
      if (interrupted()) return unknownCheck("the run was interrupted before it");
      if (out.armFailure) return unknownCheck(`the focus arm failed (${out.armFailure.reason}), so no zone was maximized to restore`);
      // Emission times (node) against frame times (probe): only on an aligned clock.
      const clock = clockGate({ capabilities: ctx.capabilities }, "output_latency_ms");
      if (clock) return unknownCheck(clock.detail);
      // restore-zone ALWAYS answers maximizedZone: null (TerminalPage.tsx); its
      // `changed` says whether anything was maximized, and the layout is read back.
      const restored = await adapter.restoreZone();
      if (restored?.changed !== true) {
        return unknownCheck(
          restored?.changed === false
            ? "restore-zone reported changed: false (nothing was maximized), so there is no restored pane"
            : "restore-zone's result carries no changed field",
        );
      }
      await sleep(ZONE_SETTLE_MS);
      if (typeof adapter.zoneViewState !== "function") return unknownCheck("the adapter cannot read the zone view state back");
      const view = await adapter.zoneViewState();
      if (view?.maximizedZone !== null) {
        return unknownCheck(`after restore-zone the zone view state reads ${view?.maximizedZone === undefined ? "no maximizedZone" : `maximized zone ${view.maximizedZone}`}`);
      }
      const w = await startPhaseWatch("restore-check");
      await sleep(o.restoreCheckMs);
      const seg = await stopPhaseWatch(w, "restore-check");
      if (interrupted()) return unknownCheck("the run was interrupted during its watch");
      const idx = indexProbeSegment(seg.records ?? []);
      const win = segmentWindow(idx);
      if (!idx.summary) return unknownCheck("its watch wrote no summary");
      if (!win) return unknownCheck("its watch has no measurement window");
      const primary = captureInfo(idx).primary;
      if (!primary) return unknownCheck("no primary capture source opened");
      // The restored (tiled) layout should show the arm's sessions — the tiled
      // arm's V target of them — whatever the probe reported seeing: a pane that
      // came back blank is absent from sessions_seen, and must not pass unjudged.
      const expected = [...slotMap.keys()].filter(Number.isInteger).sort((x, y) => x - y);
      const need = defaultVTarget(unit, "tiled");
      if (expected.length === 0) return unknownCheck("the arm has no session to expect on screen");
      const gens = indexGenerators(readGeneratorLogs(genDir));
      const stalls = paneStalls(idx, gens, win, new Set(expected));
      for (const st of stalls) {
        const verdict = st.recovered
          ? "a hitch the restored pane recovered from"
          : st.voids
            ? "the restored pane did not repaint"
            : "the restored pane sent no later frame, on too short a tail to call a wedge";
        out.incidents.push(
          incident("pane_stalled", `unit ${name} arm ${arm}: after restore-zone: ${describeStall(st)}: ${verdict}`, {
            data: { ...data, session: st.session, recovered: st.recovered, voids: st.voids },
          }),
        );
      }
      const wedged = stalls.filter((x) => !x.recovered && x.voids).length;
      // A still marker on a short frameless tail is unresolved: neither a wedge nor clean.
      const unresolved = new Set(stalls.filter((x) => !x.recovered && !x.voids).map((x) => x.session));
      // An empty stall list proves nothing about a session the rule barely saw:
      // a clean verdict needs each judged session's KEPT frames (outside excluded
      // intervals) to number RESTORE_CHECK_MIN_FRAMES, span STALL_MIN_MS, reach
      // within STALL_MIN_MS of the watch's end, and show the marker advancing
      // within the final STALL_MIN_MS of them (observed repainting at the end: a
      // pane frozen while its generator is blocked emits too few markers to be a
      // stall, and is not clean either). A frozen screen that stops sending frames
      // fails the end term.
      const cov = stallCoverage(idx, win, expected);
      const windowMs = nsToMs(win.t1 - win.t0);
      const emittedInFinal = (k) => {
        const c = cov.get(k);
        if (c?.finalFromMs === null || c?.finalFromMs === undefined) return null;
        return outputMarkersEmitted(gens?.get?.(k), win.t0 + BigInt(Math.round(c.finalFromMs * 1e6)), win.t0 + BigInt(Math.round(c.lastMs * 1e6)));
      };
      // A primary capture loss after a session's last kept frame: the read that
      // could have shown its repaint failed or never came, so it cannot be judged.
      const pframes = primaryFrames(idx, primary, win);
      const lastPrimaryT = pframes.length > 0 ? pframes.at(-1).t : null;
      const losses = captureLosses(idx, pframes).filter((x) => x.t === null || (x.t >= win.t0 && x.t <= win.t1));
      const lossText = losses.length > 0 ? `; ${summarizeLosses(losses, lastPrimaryT, win)}` : "";
      const slackNs = BigInt(CAPTURE_LOST_SLACK_MS) * 1_000_000n;
      const lossAfter = (k) => {
        const c = cov.get(k);
        if (!c || c.lastMs === null) return null;
        const last = win.t0 + BigInt(Math.round(c.lastMs * 1e6));
        return losses.find((x) => x.t === null || x.t > last - (x.exact ? 0n : slackNs)) ?? null;
      };
      const whyNot = (k) =>
        restoreCoverageGap(cov.get(k), windowMs, emittedInFinal(k)) ??
        (lossAfter(k) ? `the primary capture was lost after its last kept frame (${lossAfter(k).detail.slice(0, LOSS_DETAIL_MAX)})` : null) ??
        (unresolved.has(k) ? "still at the watch's end on a short frameless tail" : null);
      const judged = expected.filter((k) => whyNot(k) === null);
      const seen = expected
        .map((k) => {
          const c = cov.get(k);
          const why = whyNot(k);
          const mark = why === null ? "judged" : `not judged: ${why}`;
          return c.frames === 0
            ? `${k}: no kept frame (${mark})`
            : `${k}: ${c.frames} kept frame(s) at +${Math.round(c.firstMs)}..+${Math.round(c.lastMs)} ms, ${c.seqs} seq(s) (${mark})`;
        })
        .join(", ");
      // The session the focus arm had maximized is the one restore-zone restored:
      // the check is about it, so it must be among the judged.
      const restoredSession = arr.focusSession;
      if (wedged === 0 && !Number.isInteger(restoredSession)) {
        return unknownCheck(`which session the focus arm maximized is not known, so the restored pane cannot be judged (${seen})`);
      }
      if (wedged === 0 && judged.length < need) {
        return unknownCheck(
          `${judged.length} of the ${need} session(s) the restored layout should show could be judged ` +
            `(>= ${RESTORE_CHECK_MIN_FRAMES} kept frames spanning >= ${STALL_MIN_MS} ms, reaching within ${STALL_MIN_MS} ms of the end of a ${Math.round(windowMs)} ms watch, advancing within their final ${STALL_MIN_MS} ms; ${seen}${lossText})`,
        );
      }
      if (wedged === 0 && !judged.includes(restoredSession)) {
        const why = expected.includes(restoredSession) ? whyNot(restoredSession) : "it is not one of the arm's sessions";
        return unknownCheck(`the restored session ${restoredSession} could not be judged (${why}); ${seen}${lossText}`);
      }
      const outcome = wedged > 0 ? "not repainting" : stalls.some((x) => x.recovered) ? "recovered from a hitch" : "clean";
      const total = primaryFrames(idx, primary, win).length;
      const unresolvedList = [...unresolved].sort((x, y) => x - y);
      const shownOutcome = unresolvedList.length > 0 && wedged === 0 ? `${outcome} (${unresolvedList.length} session(s) unresolved: ${unresolvedList.join(",")})` : outcome;
      const note = `restore check: observed, ${shownOutcome} (${total} primary frame(s); ${seen}${lossText})`;
      log(`unit ${name} arm ${arm}: ${note}`);
      return {
        comparable: false,
        source,
        state: "measured",
        data: {
          outcome,
          frames: total,
          sessions: expected,
          judged,
          unresolved: unresolvedList,
          restored: restoredSession,
          stalls,
          losses: { count: losses.length, groups: lossGroups(losses, lastPrimaryT, win) },
        },
        notes: [note],
      };
    } catch (e) {
      return unknownCheck(e.message);
    }
  }
}

