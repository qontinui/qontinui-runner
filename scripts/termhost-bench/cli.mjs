#!/usr/bin/env node
/**
 * termhost-bench — how a terminal host holds up under N concurrent AI sessions,
 * measured outside-in (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * Phase 3: the orchestrator).
 *
 *   node scripts/termhost-bench/cli.mjs doctor  --env <id> [--setups runner,tmux] [--json]
 *   node scripts/termhost-bench/cli.mjs run     --env linux-xvfb --setups runner,tmux
 *            --n 1,5,15 --arms focus,tiled --runs 2 [--ab-baseline <sha>] ...
 *   node scripts/termhost-bench/cli.mjs report  <results.json> [--format md|json]
 *            [--redact-to <file>] [--require-measured k1,k2 --at-n 15] [--require-rows-n 1,5,15]
 *   node scripts/termhost-bench/cli.mjs report  <a.json> <b.json> ...   (one environment's files, one report)
 *   node scripts/termhost-bench/cli.mjs compare <results.json> [<baseline-results.json>]
 *            [--baseline baselines/<env>.json [--verdict-json <file>] [--propose-baseline <file>]]
 *
 * Exit: 0 success; 1 a failure (an invalid file, a failed assertion, a run that
 * left no row, a run that stopped on an incident, a `compare --baseline` verdict
 * of regression — and of nothing else: an unknown verdict exits 0); 2 usage; 3 the
 * doctor says a run cannot start here (`run` refuses before any unit; `doctor`
 * reports a blocking mechanism).
 *
 * Node built-ins only; no package.json change (plan D8).
 */

import { spawnSync } from "node:child_process";
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { applyVerdict, compareBuilds, proposeBaseline, readBaselineFile, renderComparison, unknownVerdict, verdictFails } from "./compare.mjs";
import { blockingMechanisms, doctorNotes, doctorReport, gatherDoctorFacts, mapDoctor, renderDoctor } from "./doctor.mjs";
import { environmentSpec } from "./envs/index.mjs";
import { ARMS, KPI_CATALOG, SETUP_IDS } from "./kpi-catalog.mjs";
import { locateProbeBinary } from "./probe.mjs";
import { HEADLINE_LINE, checkAssertions, headlineStatus, oneEnvironment, prepareReport, renderReport } from "./report.mjs";
import { InvalidResults } from "./results.mjs";
import { validateResults } from "./validate.mjs";
import { RUN_DEFAULTS, RunAbort, RunFailed, planUnits, runBench } from "./run.mjs";
import { createAdapter } from "./adapters/index.mjs";
import { supervisorAnswers } from "./adapters/runner.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(HERE, "..", "..");

export class UsageError extends Error {}

export const HELP = `termhost-bench — terminal-host performance under N AI sessions, measured outside-in

  doctor   --env <linux-xvfb|windows-hosted|windows-desktop> [--setups runner,tmux] [--json]
  run      --env linux-xvfb --setups runner,tmux --n 1,5,15 --arms focus,tiled --runs 2
  report   <results.json> [more.json ...] [--format md|json] [--redact-to <file>]
           (several files: one environment's, rendered as one report; the gate options take one)
           [--require-measured <kpi,...> --at-n <n,...>] [--require-rows-n <n,...>]
           [--require-measured-if-operative <mechanism>:<kpi,...>]
           [--setups ...] [--arms ...] [--roles head,single]
           [--ab-baseline <sha>]   (an A/B run: every runner row at --at-n must have a proven build)
  compare  <results.json> [<baseline.json>] [--setup runner] [--variant default] [--format md|json]
           [--baseline <baselines/env.json>]  draw the regression verdict (exit 1 on regression only;
                                              a missing baseline file is unknown("no baseline"), exit 0)
           [--verdict-json <file>]            write the verdict as JSON
           [--propose-baseline <file>]        write a baseline proposed from this run (runner rows only)

run options:
  --profile <id|path>        load profile (default claude-tui)       --seed <n>       (default 1)
  --warmup-ms <ms>           (default ${RUN_DEFAULTS.warmupMs})       --soak-ms <ms>   (default ${RUN_DEFAULTS.soakMs})
  --input-keys <n>           keys in the input arm (default ${RUN_DEFAULTS.inputKeys})
  --input-interval-ms <ms>   (default ${RUN_DEFAULTS.inputIntervalMs})
  --switch-samples <n>       focus-arm switches (default ${RUN_DEFAULTS.switchSamples})
  --spawn-samples <n>        spawn/close cycles per arm (default ${RUN_DEFAULTS.spawnSamples})
  --out <file>               results file (default <run-dir>/results.json)
  --redacted-out <file>      also write the runner-rows-only copy public CI may upload (D7)
  --smoke-setups <a,b>       label these setups' rows assert_only_smoke (public CI, D7)
  --probe <path>             termhost-probe binary (default: $TERMHOST_PROBE, then target/{debug,release},
                             target-agent/{debug,release}, and from a linked worktree the primary
                             checkout's target-agent/{debug,release}, where cargo-guard.sh builds)
  --work-dir <dir>           parent of the run directory (default: the OS temp dir)
  --run-id <id>              explicit run id

runner options (D12):
  --runner-mode <auto|subject|supervisor>  auto = supervisor when :9875 answers, else subject
  --runner-exe <path>        subject mode: the built head exe        --head-commit <sha>
  --ab-baseline <sha>        also measure the baseline build, interleaved in the same VM (D11)
  --baseline-exe <path>      subject mode: the baseline build's exe
  --head-ref <sha>           supervisor mode: spawn-test git_ref     --head-worktree <path>
  --runner-variants <a,b>    default | ghostty | pty-holder (variants need subject mode)
  --runner-env K=V           extra environment for a subject runner (repeatable)
  --supervisor-url <url>     (default http://127.0.0.1:9875)

vscode options (D5):
  --vscode-cache-dir <dir>   where the pinned archive is cached (outside any repository;
                             default %LOCALAPPDATA%\\termhost-bench\\cache\\vscode or ~/.cache/termhost-bench/vscode)
  --vscode-app-path <path>   measure an installed VS Code "as configured" instead of the pinned build

windows-terminal and tmux-wsl options (D5, Phase 5):
  --wt-cache-dir <dir>       where the pinned portable Windows Terminal ZIP is cached (outside any
                             repository; default %LOCALAPPDATA%\\termhost-bench\\cache\\windows-terminal)
  --wsl-distro <name>        tmux-wsl: the WSL 2 distro (default: WSL's default distro)

report options:
  --require-measured-if-operative <mechanism>:<kpi,...>
                             these KPIs must read measured at --at-n only where the results'
                             capabilities report <mechanism> OPERATIVE (e.g. capture.wgc:output_latency_ms)
  --accept-capture-health-unknown
                             third-party smokes on windows-hosted only: a third-party row's capture KPI
                             may read unknown for capture health (capture_disagreement, or the probe fell
                             behind); every other unknown still fails, runner rows are never excused (D6)
`;

const LIST = (s) =>
  String(s)
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);

const FLAGS_WITH_VALUES = new Set([
  "env",
  "setups",
  "n",
  "arms",
  "runs",
  "profile",
  "seed",
  "warmup-ms",
  "soak-ms",
  "input-keys",
  "input-interval-ms",
  "switch-samples",
  "spawn-samples",
  "out",
  "redacted-out",
  "smoke-setups",
  "probe",
  "work-dir",
  "run-id",
  "runner-mode",
  "runner-exe",
  "head-commit",
  "ab-baseline",
  "baseline-exe",
  "head-ref",
  "head-worktree",
  "runner-variants",
  "runner-env",
  "supervisor-url",
  "format",
  "redact-to",
  "require-measured",
  "at-n",
  "require-rows-n",
  "roles",
  "setup",
  "variant",
  "vscode-app-path",
  "vscode-cache-dir",
  "wt-cache-dir",
  "wsl-distro",
  "require-measured-if-operative",
  "baseline",
  "verdict-json",
  "propose-baseline",
]);
const BOOLEAN_FLAGS = new Set(["json", "help", "accept-capture-health-unknown"]);

/** Every option the CLI accepts (a workflow that passes another one fails at parse time). */
export const KNOWN_FLAGS = Object.freeze([...FLAGS_WITH_VALUES, ...BOOLEAN_FLAGS]);

/**
 * Parse argv into `{command, positional, flags}`. Unknown flags are usage errors.
 *
 * @param {string[]} argv
 */
export function parseArgv(argv) {
  const [command, ...rest] = argv;
  const flags = { "runner-env": [] };
  const positional = [];
  for (let i = 0; i < rest.length; i += 1) {
    const a = rest[i];
    if (!a.startsWith("--")) {
      positional.push(a);
      continue;
    }
    const eq = a.indexOf("=");
    const name = eq > 0 ? a.slice(2, eq) : a.slice(2);
    if (BOOLEAN_FLAGS.has(name)) {
      flags[name] = true;
      continue;
    }
    if (!FLAGS_WITH_VALUES.has(name)) throw new UsageError(`unknown option --${name}`);
    const value = eq > 0 ? a.slice(eq + 1) : rest[++i];
    if (value === undefined || (eq < 0 && value.startsWith("--"))) throw new UsageError(`--${name} needs a value`);
    if (name === "runner-env") flags["runner-env"].push(value);
    else flags[name] = value;
  }
  return { command, positional, flags };
}

const int = (flags, name, dflt, { min = 0 } = {}) => {
  if (flags[name] === undefined) return dflt;
  const v = Number(flags[name]);
  if (!Number.isInteger(v) || v < min) throw new UsageError(`--${name} must be an integer >= ${min}`);
  return v;
};

/** Resolve a commit-ish to a full 40-hex sha in `repoRoot`. */
export function resolveCommit(ref, { repoRoot = REPO_ROOT, run = spawnSync } = {}) {
  if (!ref) return null;
  if (/^[0-9a-f]{40}$/.test(ref)) return ref;
  const res = run("git", ["-C", repoRoot, "rev-parse", "--verify", `${ref}^{commit}`], { encoding: "utf8", timeout: 20000 });
  const sha = String(res.stdout ?? "").trim();
  if (res.status !== 0 || !/^[0-9a-f]{40}$/.test(sha)) throw new UsageError(`cannot resolve ${ref} to a commit in ${repoRoot}`);
  return sha;
}

/**
 * Normalise `run` options (and the subset `doctor` needs).
 *
 * @param {Record<string, any>} flags
 * @param {{probeSupervisor?: (url: string) => Promise<boolean>, resolve?: typeof resolveCommit, argv?: string[]}} [deps]
 */
export async function normaliseRunOptions(flags, deps = {}) {
  const env = flags.env;
  if (!env) throw new UsageError("--env is required");
  const spec = environmentSpec(env);
  const setups = flags.setups ? LIST(flags.setups) : [...spec.setups];
  for (const s of setups) if (!SETUP_IDS.includes(s)) throw new UsageError(`unknown setup ${s}`);
  const ns = flags.n ? LIST(flags.n).map(Number) : [1, 5, 15];
  if (ns.some((n) => !Number.isInteger(n) || n < 1)) throw new UsageError("--n takes positive integers");
  const arms = flags.arms ? LIST(flags.arms) : [...ARMS];
  for (const a of arms) if (!ARMS.includes(a)) throw new UsageError(`unknown arm ${a}`);
  const resolveFn = deps.resolve ?? resolveCommit;
  const supervisorUrl = flags["supervisor-url"] ?? "http://127.0.0.1:9875";
  let runnerMode = flags["runner-mode"] ?? "auto";
  if (!["auto", "subject", "supervisor"].includes(runnerMode)) throw new UsageError("--runner-mode is auto, subject or supervisor");
  if (setups.includes("runner") && runnerMode === "auto") {
    const answers = await (deps.probeSupervisor ?? supervisorAnswers)(supervisorUrl);
    if (answers === null) {
      throw new UsageError(
        `--runner-mode auto cannot tell whether a supervisor answers at ${supervisorUrl} (no answer in time); ` +
          "pass --runner-mode supervisor or subject — an unknown is never read as absent (D12)",
      );
    }
    runnerMode = answers ? "supervisor" : "subject";
  }
  const variants = flags["runner-variants"] ? LIST(flags["runner-variants"]) : ["default"];
  const baseline = flags["ab-baseline"] ? resolveFn(flags["ab-baseline"]) : null;
  const runnerBuilds = [];
  if (setups.includes("runner")) {
    if (runnerMode === "subject") {
      const exe = flags["runner-exe"] ? resolve(flags["runner-exe"]) : null;
      runnerBuilds.push({ role: baseline ? "head" : "single", commit: flags["head-commit"] ? resolveFn(flags["head-commit"]) : null, exe, source: "artifact" });
      if (baseline) {
        if (!flags["baseline-exe"]) throw new UsageError("--ab-baseline in subject mode needs --baseline-exe (the baseline build's exe)");
        runnerBuilds.push({ role: "baseline", commit: baseline, exe: resolve(flags["baseline-exe"]), source: "artifact" });
      }
    } else {
      const ref = flags["head-ref"] ? resolveFn(flags["head-ref"]) : null;
      const wt = flags["head-worktree"] ? resolve(flags["head-worktree"]) : null;
      if (Boolean(ref) === Boolean(wt)) throw new UsageError("supervisor mode needs exactly one of --head-ref <sha> or --head-worktree <path>");
      const commit = ref ?? resolveFn("HEAD", { repoRoot: wt });
      runnerBuilds.push({ role: baseline ? "head" : "single", commit, gitRef: ref, worktreePath: wt, source: ref ? "git_ref" : "worktree" });
      if (baseline) runnerBuilds.push({ role: "baseline", commit: baseline, gitRef: baseline, worktreePath: null, source: "git_ref" });
    }
  }
  const runnerEnv = {};
  for (const kv of flags["runner-env"] ?? []) {
    const i = kv.indexOf("=");
    if (i <= 0) throw new UsageError(`--runner-env takes KEY=VALUE, got ${kv}`);
    runnerEnv[kv.slice(0, i)] = kv.slice(i + 1);
  }
  return {
    env,
    setups,
    ns,
    arms,
    runs: int(flags, "runs", 1, { min: 1 }),
    profile: flags.profile ?? RUN_DEFAULTS.profile,
    seed: int(flags, "seed", RUN_DEFAULTS.seed),
    warmupMs: int(flags, "warmup-ms", RUN_DEFAULTS.warmupMs),
    soakMs: int(flags, "soak-ms", RUN_DEFAULTS.soakMs, { min: 1 }),
    inputKeys: int(flags, "input-keys", RUN_DEFAULTS.inputKeys),
    inputIntervalMs: int(flags, "input-interval-ms", RUN_DEFAULTS.inputIntervalMs, { min: 1 }),
    switchSamples: int(flags, "switch-samples", RUN_DEFAULTS.switchSamples),
    switchSettleMs: RUN_DEFAULTS.switchSettleMs,
    spawnSamples: int(flags, "spawn-samples", RUN_DEFAULTS.spawnSamples),
    spawnTimeoutMs: RUN_DEFAULTS.spawnTimeoutMs,
    restoreCheckMs: RUN_DEFAULTS.restoreCheckMs,
    out: flags.out ? resolve(flags.out) : undefined,
    redactedOut: flags["redacted-out"] ? resolve(flags["redacted-out"]) : undefined,
    smokeSetups: flags["smoke-setups"] ? LIST(flags["smoke-setups"]) : [],
    probe: flags.probe ? resolve(flags.probe) : undefined,
    workDir: flags["work-dir"] ? resolve(flags["work-dir"]) : undefined,
    runId: flags["run-id"],
    runnerMode,
    runnerBuilds,
    runnerVariants: variants,
    runnerEnv,
    supervisorUrl,
    vscodeAppPath: flags["vscode-app-path"] ? resolve(flags["vscode-app-path"]) : null,
    vscodeCacheDir: flags["vscode-cache-dir"] ? resolve(flags["vscode-cache-dir"]) : null,
    wtCacheDir: flags["wt-cache-dir"] ? resolve(flags["wt-cache-dir"]) : null,
    wslDistro: flags["wsl-distro"] ?? null,
    repoRoot: REPO_ROOT,
    command: ["node", relative(REPO_ROOT, fileURLToPath(import.meta.url)).replace(/\\/g, "/"), ...(deps.argv ?? [])],
  };
}

async function cmdDoctor(flags, io, deps = {}) {
  const env = flags.env;
  if (!env) throw new UsageError("--env is required");
  const spec = environmentSpec(env);
  const o = await normaliseRunOptions({ ...flags, env }, {}).catch((e) => {
    if (e instanceof UsageError && /needs exactly one|needs --baseline-exe/.test(e.message)) return null;
    throw e;
  });
  const setups = o?.setups ?? (flags.setups ? LIST(flags.setups) : [...spec.setups]);
  const adapters = [];
  for (const setup of setups) {
    try {
      const unit = o ? planUnits({ setups: [setup], ns: [o.ns[0]], runs: 1, runnerBuilds: o.runnerBuilds, runnerVariants: o.runnerVariants })[0] : { setup, variant: "default", build: { role: "single", commit: null } };
      adapters.push(
        await createAdapter(
          setup,
          setup === "runner"
            ? { variant: unit.variant, mode: o?.runnerMode === "supervisor" ? "supervisor" : "subject", build: unit.build ?? { role: "single", commit: null }, supervisorUrl: o?.supervisorUrl, repoRoot: REPO_ROOT }
            : setup === "vscode"
              ? {
                  variant: "default",
                  appPath: flags["vscode-app-path"] ? resolve(flags["vscode-app-path"]) : null,
                  cacheDir: flags["vscode-cache-dir"] ? resolve(flags["vscode-cache-dir"]) : null,
                }
              : setup === "windows-terminal" || setup === "tmux-wsl"
                ? { variant: "default", cacheDir: flags["wt-cache-dir"] ? resolve(flags["wt-cache-dir"]) : null, distro: flags["wsl-distro"] ?? null }
                : { variant: "default" },
        ),
      );
    } catch (e) {
      io.err(`doctor: ${setup}: ${e.message}\n`);
    }
  }
  let environment = null;
  let display = null;
  if (spec.start && process.platform === spec.platform) {
    try {
      environment = await (deps.startEnvironment ?? spec.start)({ log: () => {} });
      display = { started: true, detail: environment.display ?? "started" };
    } catch (e) {
      display = { started: false, detail: e.message };
    }
  } else if (spec.platform === "linux") {
    display = { started: false, detail: `the ${env} environment starts only on Linux` };
  }
  try {
    const facts = await (deps.gatherDoctorFacts ?? gatherDoctorFacts)({
      environment: env,
      spec,
      ...(() => {
        const probe = locateProbeBinary({ explicit: flags.probe ? resolve(flags.probe) : null });
        return { probePath: probe.path, probeTried: probe.tried, probeSource: probe.source };
      })(),
      adapters,
      envVars: environment?.env ?? null,
      display,
    });
    const mechanisms = mapDoctor(facts);
    const report = doctorReport(mechanisms, { environment: env, notes: doctorNotes(facts) });
    io.out(flags.json ? `${JSON.stringify(report)}\n` : renderDoctor(report));
    const blocking = blockingMechanisms(mechanisms);
    if (blocking.length > 0) {
      io.err(`termhost-bench: a run cannot start here: ${blocking.map((b) => `${b.id} ${b.state}`).join(", ")}\n`);
      return 3;
    }
    return 0;
  } finally {
    await environment?.stop?.();
  }
}

async function cmdRun(flags, io, argv, deps = {}) {
  const o = await normaliseRunOptions(flags, { argv, ...(deps.probeSupervisor ? { probeSupervisor: deps.probeSupervisor } : {}) });
  try {
    const res = await (deps.runBench ?? runBench)(o, { log: (s) => io.err(`termhost-bench: ${s}\n`) });
    io.out(`${res.outPath}\n`);
    if (res.stopped) {
      io.err(`termhost-bench: the run STOPPED: ${res.stopped.detail}\n`);
      return 1;
    }
    return 0;
  } catch (e) {
    if (e instanceof RunAbort) {
      io.err(`termhost-bench: ${e.message}\n`);
      return 3;
    }
    if (e instanceof RunFailed || e instanceof InvalidResults) {
      io.err(`termhost-bench: ${e.message}\n`);
      return 1;
    }
    throw e;
  }
}

/**
 * `--require-measured-if-operative <mechanism>:<kpi,...>` — KPIs the gate holds
 * to `measured` only where the results file's capabilities report the mechanism
 * OPERATIVE (a hosted Windows VM's capture is the image's to decide, plan Phase 2).
 *
 * @returns {{mechanism: string, kpis: string[]}}
 */
export function parseOperativeRequirement(value) {
  const m = /^([a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+):([a-z0-9_,]+)$/.exec(String(value));
  if (!m) throw new UsageError(`--require-measured-if-operative takes <mechanism>:<kpi,...>, got ${value}`);
  const kpis = LIST(m[2]);
  const unknown = kpis.filter((k) => !KPI_CATALOG.some((c) => c.id === k));
  if (unknown.length > 0) throw new UsageError(`--require-measured-if-operative names KPIs the catalog does not define: ${unknown.join(", ")}`);
  return { mechanism: m[1], kpis };
}

function readDoc(path) {
  try {
    return JSON.parse(readFileSync(resolve(path), "utf8"));
  } catch (e) {
    throw new UsageError(`cannot read ${path}: ${e.message}`);
  }
}

function cmdReport(positional, flags, io) {
  if (positional.length === 0) throw new UsageError("report takes one results file, or several from one environment");
  if (positional.length > 1) return cmdReportMany(positional, flags, io);
  const doc = readDoc(positional[0]);
  const prep = prepareReport(doc);
  if (!prep.valid) {
    // Paths and keywords only: a validator message can quote a value, this output
    // can reach a public CI log, and a third-party reading never leaves the job (D7).
    io.out(`INVALID: ${positional[0]} (${prep.errors.length} errors)\n`);
    for (const e of prep.errors.slice(0, 50)) io.out(`  ${e.path || "/"} [${e.keyword}]\n`);
    return 1;
  }
  if (flags["redact-to"]) {
    if (prep.redacted) {
      writeFileSync(resolve(flags["redact-to"]), `${JSON.stringify(prep.redacted, null, 2)}\n`);
      io.err(`termhost-bench: wrote the runner-rows-only copy ${flags["redact-to"]}\n`);
    } else {
      io.err("termhost-bench: no runner row to publish; nothing redacted\n");
    }
  }
  const failures = checkAssertions(doc, {
    requireMeasured: flags["require-measured"] ? LIST(flags["require-measured"]) : undefined,
    requireMeasuredIfOperative: flags["require-measured-if-operative"] ? [parseOperativeRequirement(flags["require-measured-if-operative"])] : undefined,
    atN: flags["at-n"] ? LIST(flags["at-n"]).map(Number) : undefined,
    requireRowsN: flags["require-rows-n"] ? LIST(flags["require-rows-n"]).map(Number) : undefined,
    setups: flags.setups ? LIST(flags.setups) : undefined,
    arms: flags.arms ? LIST(flags.arms) : undefined,
    roles: flags.roles ? LIST(flags.roles) : undefined,
    abBaseline: Boolean(flags["ab-baseline"]),
    acceptCaptureHealthUnknown: Boolean(flags["accept-capture-health-unknown"]),
  });
  io.out(flags.format === "json" ? `${JSON.stringify({ valid: true, failures }, null, 2)}\n` : renderReport(doc));
  if (failures.length > 0) {
    io.err(`termhost-bench: the gate FAILED (${failures.length}):\n${failures.map((f) => `  - ${f}`).join("\n")}\n`);
    return 1;
  }
  return 0;
}

/**
 * Several results files of ONE environment as one report (the committed report,
 * D7). No gate options: those check one run's file.
 */
function cmdReportMany(positional, flags, io) {
  for (const f of ["redact-to", "require-measured", "require-measured-if-operative", "at-n", "require-rows-n", "ab-baseline"]) {
    if (flags[f] !== undefined) throw new UsageError(`--${f} checks one results file; pass one file`);
  }
  const docs = positional.map(readDoc);
  for (const [i, doc] of docs.entries()) {
    const res = validateResults(doc);
    if (!res.valid) {
      io.out(`INVALID: ${positional[i]} (${res.errors.length} errors)\n`);
      for (const e of res.errors.slice(0, 50)) io.out(`  ${e.path || "/"} [${e.keyword}]\n`);
      return 1;
    }
  }
  let environment;
  try {
    environment = oneEnvironment(docs);
  } catch (e) {
    throw new UsageError(e.message);
  }
  if (flags.format === "json") {
    const st = headlineStatus(docs);
    io.out(
      `${JSON.stringify(
        {
          valid: true,
          environment,
          runs: docs.map((d) => d.run.id),
          headline: { ...st, line: st.established ? HEADLINE_LINE : null },
        },
        null,
        2,
      )}\n`,
    );
  } else {
    io.out(renderReport(docs));
  }
  return 0;
}

function cmdCompare(positional, flags, io) {
  if (positional.length < 1 || positional.length > 2) throw new UsageError("compare takes one or two results files");
  for (const f of ["verdict-json", "propose-baseline"]) {
    if (flags[f] !== undefined && flags.baseline === undefined) throw new UsageError(`--${f} needs --baseline`);
  }
  const head = readDoc(positional[0]);
  const base = positional[1] ? readDoc(positional[1]) : null;
  const opts = { setup: flags.setup ?? "runner", variant: flags.variant ?? "default" };
  if (flags.baseline === undefined) {
    const cmp = compareBuilds(head, base, opts);
    io.out(flags.format === "json" ? `${JSON.stringify(cmp, null, 2)}\n` : renderComparison(cmp));
    return 0;
  }
  // Verdict mode: everything short of a judged regression is reported and exits 0.
  const baselineState = readBaselineFile(resolve(flags.baseline));
  let cmp = null;
  let verdict;
  try {
    cmp = applyVerdict(compareBuilds(head, base, opts), baselineState);
    verdict = cmp.verdict;
  } catch (e) {
    verdict = unknownVerdict("no comparison", e.message);
  }
  if (flags["verdict-json"]) writeFileSync(resolve(flags["verdict-json"]), `${JSON.stringify(verdict, null, 2)}\n`);
  if (flags["propose-baseline"]) {
    try {
      if (!cmp) throw new Error("no comparison to propose from");
      writeFileSync(resolve(flags["propose-baseline"]), `${JSON.stringify(proposeBaseline(cmp, { runId: head.run?.id ?? null }), null, 2)}\n`);
      io.err(`termhost-bench: wrote a proposed baseline ${flags["propose-baseline"]} (review its thresholds before committing it)\n`);
    } catch (e) {
      io.err(`termhost-bench: no baseline proposed: ${e.message}\n`);
    }
  }
  if (flags.format === "json") io.out(`${JSON.stringify(cmp ?? { verdict }, null, 2)}\n`);
  else io.out(cmp ? renderComparison(cmp) : `Verdict: **unknown** — ${verdict.reason}: ${verdict.detail}. An unknown never fails the job.\n`);
  io.err(`termhost-bench: verdict ${verdict.verdict} (${verdict.reason})\n`);
  return verdictFails(verdict) ? 1 : 0;
}

/**
 * @param {string[]} argv
 * @param {{out?: (s: string) => void, err?: (s: string) => void}} [io]
 * @returns {Promise<number>} exit code
 */
export async function main(argv, io = {}, deps = {}) {
  const out = io.out ?? ((s) => process.stdout.write(s));
  const err = io.err ?? ((s) => process.stderr.write(s));
  let parsed;
  try {
    parsed = parseArgv(argv);
  } catch (e) {
    err(`${e.message}\n${HELP}`);
    return 2;
  }
  const { command, positional, flags } = parsed;
  if (!command || flags.help || command === "help" || command === "--help") {
    out(HELP);
    return command ? 0 : 2;
  }
  try {
    switch (command) {
      case "doctor":
        return await cmdDoctor(flags, { out, err }, deps);
      case "run":
        return await cmdRun(flags, { out, err }, argv, deps);
      case "report":
        return cmdReport(positional, flags, { out, err });
      case "compare":
        return cmdCompare(positional, flags, { out, err });
      default:
        throw new UsageError(`unknown command ${command}`);
    }
  } catch (e) {
    if (e instanceof UsageError) {
      err(`termhost-bench: ${e.message}\n`);
      return 2;
    }
    err(`termhost-bench: ${e?.stack ?? e}\n`);
    return 1;
  }
}

/** Whether `argv1` names this module, through any symlink (a symlinked path must not run nothing, silently). */
export function isEntryPoint(argv1, moduleUrl = import.meta.url) {
  if (!argv1) return false;
  const real = (p) => {
    try {
      return realpathSync(p);
    } catch {
      return resolve(p);
    }
  };
  return real(resolve(argv1)) === real(fileURLToPath(moduleUrl));
}

const invokedDirectly = isEntryPoint(process.argv[1]);
if (invokedDirectly) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  });
}
