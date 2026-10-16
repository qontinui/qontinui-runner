/**
 * Phase 4's integration surface: the `windows-hosted` environment and its doctor
 * fact, D6's `dom_renderer_fallback` label on VS Code rows of a linux-xvfb run
 * (and their exclusion from every comparison), the capability-conditional gate
 * (`--require-measured-if-operative`), the VS Code CLI options, and the contract
 * of the `run-windows-hosted` job in `.github/workflows/termhost-bench.yml` — the
 * options it passes exist, the gate is the plan's, and nothing but the runner's
 * own rows leaves the VM (D7).
 *
 *   node --test scripts/termhost-bench/__tests__/windows-hosted.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { KNOWN_FLAGS, UsageError, normaliseRunOptions, parseOperativeRequirement } from "../cli.mjs";
import { VERDICT_EXCLUDED_LABELS, compareBuilds } from "../compare.mjs";
import { PROBE_MECHANISMS, blockingMechanisms, gatherDoctorFacts, mapDoctor } from "../doctor.mjs";
import { ENVIRONMENT_SPECS, environmentSpec } from "../envs/index.mjs";
import { SCREEN_QUERY, hostedRunnerVerdict, parseScreen, startWindowsHosted } from "../envs/windows-hosted.mjs";
import { CROSSCHECK_PROFILES, KPI_CATALOG, ROW_LABELS } from "../kpi-catalog.mjs";
import { machineBlock } from "../machine.mjs";
import { NeutralityGuard } from "../neutrality.mjs";
import { watchArgs } from "../probe.mjs";
import { checkAssertions, renderReport } from "../report.mjs";
import { adapterOptionsFor, fallbackSetup, noteLogLines, rowLabels, runBench } from "../run.mjs";
import { validateResults } from "../validate.mjs";
import { FakeAdapter, VirtualClock, fakeProbe, fakeRunOptions, healthyFacts } from "./helpers/fake-bench.mjs";
import { windowsDesktopResults } from "./helpers/results.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKFLOW = join(HERE, "..", "..", "..", ".github", "workflows", "termhost-bench.yml");
const HOSTED = { GITHUB_ACTIONS: "true", RUNNER_ENVIRONMENT: "github-hosted", RUNNER_OS: "Windows", ImageOS: "win25" };

// ---------------------------------------------------------------------------
// The environment
// ---------------------------------------------------------------------------

test("windows-hosted starts only on a GitHub-hosted Windows runner: a workstation is windows-desktop", () => {
  assert.equal(hostedRunnerVerdict(HOSTED).state, "OPERATIVE");
  assert.match(hostedRunnerVerdict(HOSTED).reason, /ImageOS=win25/);
  for (const env of [{}, { ...HOSTED, RUNNER_ENVIRONMENT: "self-hosted" }, { ...HOSTED, GITHUB_ACTIONS: undefined }, { ...HOSTED, RUNNER_OS: "Linux" }]) {
    const v = hostedRunnerVerdict(env);
    assert.equal(v.state, "INOPERATIVE-ON-THIS-MACHINE", JSON.stringify(env));
    assert.match(v.reason, /windows-desktop/);
  }
});

test("the screen is measured read-only: the adapter's current mode and the applied DPI", async () => {
  assert.deepEqual(parseScreen('{"w":1920,"h":1080,"dpi":96}'), { width: 1920, height: 1080, scalePct: 100 });
  assert.deepEqual(parseScreen('{"w":2560,"h":1440,"dpi":144}'), { width: 2560, height: 1440, scalePct: 150 });
  assert.deepEqual(parseScreen('{"w":1024,"h":768,"dpi":0}'), { width: 1024, height: 768, scalePct: 100 });
  assert.equal(parseScreen('{"w":0,"h":768}'), null);
  assert.equal(parseScreen("garbage"), null);
  assert.match(SCREEN_QUERY, /Win32_VideoController/);
  assert.match(SCREEN_QUERY, /AppliedDPI/);
  assert.doesNotMatch(SCREEN_QUERY, /\bSet-|Remove-|New-Item/, "the query changes nothing");
  await assert.rejects(startWindowsHosted({ platform: "linux" }), /Windows only/);
  const calls = [];
  const env = await startWindowsHosted({ platform: "win32", run: (cmd, args) => (calls.push([cmd, ...args]), { status: 0, stdout: '{"w":1920,"h":1080,"dpi":96}' }) });
  assert.deepEqual(env.screen, { width: 1920, height: 1080, scalePct: 100 });
  assert.deepEqual(env.env, {}, "no display server to point anything at");
  assert.equal(calls[0][0], "powershell.exe");
  await env.stop();
  const blind = await startWindowsHosted({ platform: "win32", run: () => ({ status: 1, stdout: "" }) });
  assert.equal(blind.screen, null, "an unreadable screen is absent, not guessed");
});

test("the environment registry: windows-hosted and windows-desktop built, linux-xvfb labels VS Code rows", () => {
  const hosted = environmentSpec("windows-hosted");
  assert.equal(hosted.notBuilt, null);
  assert.equal(hosted.start, startWindowsHosted);
  assert.ok(hosted.setups.includes("vscode") && hosted.setups.includes("runner"));
  assert.deepEqual(hosted.doctorFacts(HOSTED), { "env.hosted_runner": hostedRunnerVerdict(HOSTED) });
  assert.equal(environmentSpec("windows-desktop").notBuilt, null, "Phase 5 builds windows-desktop");
  assert.ok(environmentSpec("windows-hosted").setups.includes("windows-terminal"), "the hosted job smokes Windows Terminal");
  assert.deepEqual(environmentSpec("linux-xvfb").setupLabels.vscode, ["dom_renderer_fallback"]);
  for (const spec of Object.values(ENVIRONMENT_SPECS)) {
    for (const labels of Object.values(spec.setupLabels ?? {})) for (const l of labels) assert.ok(Object.hasOwn(ROW_LABELS, l), `${l} is a catalog row label`);
  }
});

test("the doctor merges the environment's own facts, and an off-runner windows-hosted run cannot start", async () => {
  const spec = { ...environmentSpec("windows-hosted"), doctorFacts: () => ({ "env.hosted_runner": hostedRunnerVerdict({}) }) };
  const facts = await gatherDoctorFacts({ environment: "windows-hosted", spec, probePath: null, adapters: [] });
  assert.equal(facts.setupMechanisms["env.hosted_runner"].state, "INOPERATIVE-ON-THIS-MACHINE");
  const m = mapDoctor({ ...facts, platform: "win32" });
  assert.equal(m["env.hosted_runner"].state, "INOPERATIVE-ON-THIS-MACHINE");
  assert.ok(blockingMechanisms(m).some((b) => b.id === "env.hosted_runner"), "it blocks the run (exit 3)");
  const ok = mapDoctor({ ...facts, platform: "win32", setupMechanisms: { "env.hosted_runner": hostedRunnerVerdict(HOSTED) } });
  assert.ok(!blockingMechanisms(ok).some((b) => b.id === "env.hosted_runner"));
});

// ---------------------------------------------------------------------------
// D6: VS Code rows on linux-xvfb
// ---------------------------------------------------------------------------

test("row labels add the environment's labels for the setup, once", () => {
  const setup = { id: "vscode" };
  const geometry = { cols: 120, rows: 40 };
  assert.deepEqual(rowLabels({ softwareRendered: true, setup, smoke: false, paneSizes: [], geometry, envLabels: ["dom_renderer_fallback"] }), ["software_rendered", "dom_renderer_fallback"]);
  assert.deepEqual(rowLabels({ softwareRendered: false, setup, smoke: true, paneSizes: [], geometry }), ["assert_only_smoke"]);
  assert.deepEqual(rowLabels({ softwareRendered: false, setup, smoke: false, paneSizes: [], geometry, envLabels: ["assert_only_smoke"] }).length, 1);
});

test("a linux-xvfb run with VS Code labels every VS Code row dom_renderer_fallback, measured or not, and the file stays valid", async (t) => {
  const tmp = mkdtempSync(join(tmpdir(), "tb-vsc-run-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  mkdirSync(join(tmp, "primary"), { recursive: true });
  const settings = join(tmp, "primary", "settings.json");
  writeFileSync(settings, "{}");
  const clock = new VirtualClock();
  let current = null;
  const res = await runBench(fakeRunOptions(join(tmp, "work"), { setups: ["vscode", "tmux"], out: join(tmp, "results.json") }), {
    log: () => {},
    sleep: clock.sleep,
    startEnvironment: async () => ({ env: { DISPLAY: ":99" }, screen: { width: 1920, height: 1080, scalePct: 100 }, display: ":99", stop: async () => {} }),
    probePath: "fake-probe",
    createAdapter: async (setup, o) => (current = new FakeAdapter({ id: setup, clock, variant: o.variant })),
    probe: fakeProbe({ clock, current: () => current }),
    gatherDoctorFacts: async () => healthyFacts(),
    sampleBackgroundLoad: async () => ({ cpu_pct: 1, memory_used_mb: 100, sampled_ms: 1 }),
    machineBlock: (o) => machineBlock({ ...o, gpu: { state: "unknown", reason: "not_instrumented", detail: "test" } }),
    neutrality: new NeutralityGuard({ paths: [{ label: "settings.json", path: settings }] }),
    reapGenerators: async () => ({ ended: [], survivors: [] }),
  });
  const v = validateResults(res.doc);
  assert.equal(v.valid, true, JSON.stringify(v.errors.slice(0, 3)));
  const vsc = res.doc.rows.filter((r) => r.setup.id === "vscode");
  const tmux = res.doc.rows.filter((r) => r.setup.id === "tmux");
  assert.ok(vsc.length > 0 && tmux.length > 0);
  assert.ok(vsc.every((r) => r.labels.includes("dom_renderer_fallback")), "every VS Code row");
  assert.ok(tmux.every((r) => !r.labels.includes("dom_renderer_fallback")), "and no other setup's");
});

/** A fake run in `env` that records every watch's options; returns the doc and those options. */
async function profiledRun(t, env, setups, { stale = null } = {}) {
  const tmp = mkdtempSync(join(tmpdir(), "tb-profile-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  mkdirSync(join(tmp, "primary"), { recursive: true });
  const settings = join(tmp, "primary", "settings.json");
  writeFileSync(settings, "{}");
  const clock = new VirtualClock();
  let current = null;
  const watches = [];
  const probe = fakeProbe({ clock, current: () => current });
  const startWatch = probe.startWatch.bind(probe);
  probe.startWatch = (o) => {
    watches.push(o);
    // `stale`: a probe that judges under another profile than its argv says.
    return startWatch(stale ? { ...o, crosscheckProfile: stale } : o);
  };
  const res = await runBench(fakeRunOptions(join(tmp, "work"), { env, setups, ns: [1], smokeSetups: [], out: join(tmp, "results.json") }), {
    log: () => {},
    sleep: clock.sleep,
    startEnvironment: async () => ({ env: {}, screen: { width: 1920, height: 1080, scalePct: 100 }, display: null, stop: async () => {} }),
    probePath: "fake-probe",
    createAdapter: async (setup, o) => (current = new FakeAdapter({ id: setup, clock, variant: o.variant })),
    probe,
    gatherDoctorFacts: async () => ({ ...healthyFacts(), "env.hosted_runner": { state: "OPERATIVE", reason: "fake" } }),
    sampleBackgroundLoad: async () => ({ cpu_pct: 1, memory_used_mb: 100, sampled_ms: 1 }),
    machineBlock: (o) => machineBlock({ ...o, gpu: { state: "unknown", reason: "not_instrumented", detail: "test" } }),
    neutrality: new NeutralityGuard({ paths: [{ label: "settings.json", path: settings }] }),
    reapGenerators: async () => ({ ended: [], survivors: [] }),
    primaryPresent: async () => false,
  });
  return { doc: res.doc, watches };
}

test("the cross-check profile is the environment's: software-compositor on windows-hosted, strict elsewhere, and every spec declares one", () => {
  assert.equal(environmentSpec("windows-hosted").crosscheckProfile, "software-compositor");
  assert.equal(environmentSpec("windows-desktop").crosscheckProfile, "strict");
  assert.equal(environmentSpec("linux-xvfb").crosscheckProfile, "strict");
  for (const spec of Object.values(ENVIRONMENT_SPECS)) assert.ok(CROSSCHECK_PROFILES.includes(spec.crosscheckProfile), spec.id);
});

test("the profile flows from the environment spec to every watch's probe argv, into every row, and into the report", async (t) => {
  for (const [env, setups, want] of [
    ["windows-hosted", ["runner"], "software-compositor"],
    ["linux-xvfb", ["runner"], "strict"],
  ]) {
    const { doc, watches } = await profiledRun(t, env, setups);
    assert.ok(watches.length > 0, env);
    for (const w of watches) {
      const argv = watchArgs(w);
      const i = argv.indexOf("--crosscheck-profile");
      assert.ok(i > 0, `${env} ${w.label}: no --crosscheck-profile in ${argv.join(" ")}`);
      assert.equal(argv[i + 1], want, `${env} ${w.label}`);
    }
    assert.ok(doc.rows.length > 0);
    for (const r of doc.rows) assert.equal(r.scenario.crosscheck_profile, want, env);
    // End to end through the gate: the fake probe echoes the profile its argv
    // carried, so a watch told another profile than the row's would be voided.
    for (const r of doc.rows) {
      assert.doesNotMatch(r.kpis.output_latency_ms.detail ?? "", /judged the capture cross-check/, `${env}: ${JSON.stringify(r.kpis.output_latency_ms)}`);
    }
    assert.ok(doc.rows.some((r) => r.kpis.output_latency_ms.state === "measured"), `${env}: some row's output latency is measured`);
    assert.equal(validateResults(doc).valid, true, JSON.stringify(validateResults(doc).errors.slice(0, 3)));
    assert.match(renderReport(doc), new RegExp(`\\| ${want} \\|`), "the report shows it beside the row's latency");
  }
});

test("a probe that judged under another profile than the environment declared voids the rows' latency, end to end (round 4 S1)", async (t) => {
  const { doc } = await profiledRun(t, "windows-hosted", ["runner"], { stale: "strict" });
  assert.ok(doc.rows.length > 0);
  for (const r of doc.rows) {
    assert.equal(r.kpis.output_latency_ms.state, "unknown", JSON.stringify(r.kpis.output_latency_ms));
    assert.equal(r.kpis.output_latency_ms.reason, "harness_error");
    assert.match(r.kpis.output_latency_ms.detail, /the soak watch judged the capture cross-check under the strict profile; the windows-hosted environment declares software-compositor/);
  }
});

test("a VS Code unit that never described itself still records the launch mode it was planned with", () => {
  const unit = { setup: "vscode", variant: "default", build: null };
  assert.equal(fallbackSetup(unit, "x", {}).launch_mode, "portable_pinned");
  assert.equal(fallbackSetup(unit, "x", { vscodeAppPath: "C:\\vsc" }).launch_mode, "installed_app_path");
  assert.equal(fallbackSetup({ setup: "tmux", variant: "default" }, "x", {}).launch_mode, "private_server");
});

test("KPI notes reach the log for the runner only; a third party's stay in its (never uploaded) results file (D7)", () => {
  assert.deepEqual(noteLogLines("runner", "unit u0 arm focus", ["a reading"]), ["unit u0 arm focus: a reading"]);
  for (const setup of ["vscode", "tmux", "windows-terminal", "tmux-wsl"]) assert.deepEqual(noteLogLines(setup, "unit u1 arm tiled", ["a reading"]), [], setup);
});

test("rows labelled dom_renderer_fallback stay out of every comparison (and so of Phase 6's verdict)", () => {
  assert.deepEqual(VERDICT_EXCLUDED_LABELS, ["dom_renderer_fallback"]);
  const doc = (labels) => ({
    machine: { environment: "linux-xvfb" },
    rows: [1, 5].flatMap((n) => [
      { setup: { id: "vscode", variant: "default", build: { state: "not_applicable" } }, scenario: { n, arm: "focus" }, kpis: {}, labels },
    ]),
  });
  assert.throws(() => compareBuilds(doc(["software_rendered", "dom_renderer_fallback"]), doc(["software_rendered", "dom_renderer_fallback"]), { setup: "vscode" }), /no vscode\/default head rows/);
  const cmp = compareBuilds(doc(["software_rendered"]), doc(["software_rendered"]), { setup: "vscode" });
  assert.ok(cmp.entries.length > 0, "the same rows without the label do compare");
});

// ---------------------------------------------------------------------------
// The capability-conditional gate and the VS Code CLI options
// ---------------------------------------------------------------------------

test("--require-measured-if-operative holds a KPI to measured only where the file reports the mechanism OPERATIVE", () => {
  const doc = windowsDesktopResults();
  const target = doc.rows.find((r) => r.setup.id === "vscode" && r.scenario.n === 15);
  target.kpis.output_latency_ms = { state: "unknown", unit: "ms", n: 0, reason: "capture_unavailable", detail: "hosted VM" };
  const gate = { atN: [15], setups: ["vscode"], arms: ["tiled"], requireMeasured: ["cpu_pct"], requireMeasuredIfOperative: [{ mechanism: "capture.wgc", kpis: ["output_latency_ms"] }] };
  doc.capabilities["capture.wgc"] = { state: "OPERATIVE", reason: "fake" };
  const held = checkAssertions(doc, gate);
  assert.equal(held.length, 1);
  assert.match(held[0], /vscode N = 15 tiled run 0: output_latency_ms is unknown\(capture_unavailable\) — hosted VM$/, "a third party's detail reaches the log only reading-free (D7)");
  doc.capabilities["capture.wgc"] = { state: "INOPERATIVE-ON-THIS-MACHINE", reason: "no capture on the image" };
  assert.deepEqual(checkAssertions(doc, gate), [], "capture inoperative: unknown with its reason is the honest reading");
  target.kpis.cpu_pct = { state: "unknown", unit: "% of one logical core", n: 0, reason: "harness_error", detail: "x" };
  assert.equal(checkAssertions(doc, gate).length, 1, "an unconditional requirement still holds");
});

test("the CLI: the VS Code options and the conditional gate parse, and a malformed one is a usage error", async () => {
  for (const f of ["vscode-app-path", "vscode-cache-dir", "require-measured-if-operative"]) assert.ok(KNOWN_FLAGS.includes(f), f);
  assert.deepEqual(parseOperativeRequirement("capture.wgc:output_latency_ms,frame_interval_ms"), { mechanism: "capture.wgc", kpis: ["output_latency_ms", "frame_interval_ms"] });
  assert.throws(() => parseOperativeRequirement("capture:output_latency_ms"), UsageError);
  assert.throws(() => parseOperativeRequirement("capture.wgc:latency"), /does not define: latency/);
  const o = await normaliseRunOptions({ env: "windows-hosted", setups: "vscode", "vscode-cache-dir": "c-dir", "vscode-app-path": "app" }, {});
  assert.match(o.vscodeCacheDir.replace(/\\/g, "/"), /\/c-dir$/);
  assert.match(o.vscodeAppPath.replace(/\\/g, "/"), /\/app$/);
  assert.deepEqual(o.runnerBuilds, [], "no runner, no build");
  const opts = adapterOptionsFor({ setup: "vscode", variant: "default" }, o, () => {});
  assert.equal(opts.cacheDir, o.vscodeCacheDir);
  assert.equal(opts.appPath, o.vscodeAppPath);
});

// ---------------------------------------------------------------------------
// The run-windows-hosted job
// ---------------------------------------------------------------------------

/** The lines of one job, from its key to the next job key. */
function jobLines(yml, key) {
  const lines = yml.split(/\r?\n/);
  const start = lines.findIndex((l) => l === `  ${key}:`);
  assert.ok(start > 0, `the workflow has no job ${key}`);
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i += 1) {
    if (/^ {2}[A-Za-z0-9_-]+:\s*$/.test(lines[i])) {
      end = i;
      break;
    }
  }
  return lines.slice(start, end);
}

/** The job's steps, each as its lines. */
function steps(job) {
  const out = [];
  for (const l of job) {
    if (/^ {6}- /.test(l)) out.push([l]);
    else if (out.length > 0) out.at(-1).push(l);
  }
  return out.map((s) => ({ name: (/name: (.*)$/.exec(s.find((l) => /^ {6}- name: | {8}name: /.test(l)) ?? "") ?? [])[1] ?? "", text: s.join("\n"), lines: s }));
}

/** Every cli.mjs invocation (with `\` continuations) and every `args=( … )` array. */
function invocations(lines) {
  const out = [];
  for (let i = 0; i < lines.length; i += 1) {
    if (/node scripts\/termhost-bench\/cli\.mjs (doctor|run|report|compare)\b/.test(lines[i])) {
      let text = lines[i];
      while (/\\\s*$/.test(lines[i]) && i + 1 < lines.length) text += `\n${lines[++i]}`;
      out.push(text);
    } else if (/\bargs\+?=\(/.test(lines[i])) {
      let text = lines[i];
      while (!/\)\s*$/.test(lines[i]) && i + 1 < lines.length) text += `\n${lines[++i]}`;
      out.push(text);
    }
  }
  return out;
}

const flagsOf = (text) => [...text.matchAll(/(?:^|[\s(])--([a-z][a-z0-9-]*)/g)].map((m) => m[1]);
const yml = () => readFileSync(WORKFLOW, "utf8");

test("the job runs on bench-scope's triggers, on windows-latest, inside GitHub's 360-minute ceiling", () => {
  const job = jobLines(yml(), "run-windows-hosted");
  const text = job.join("\n");
  assert.match(text, /^ {4}needs: bench-scope$/m);
  assert.match(text, /^ {4}if: needs\.bench-scope\.outputs\.run == 'true'$/m);
  assert.match(text, /^ {4}runs-on: windows-latest$/m);
  const timeout = Number(/^ {4}timeout-minutes: (\d+)$/m.exec(text)[1]);
  assert.ok(timeout <= 360, "GitHub-hosted jobs stop at 360 minutes");
  const bounded = [...text.matchAll(/^ {8}timeout-minutes: (\d+)$/gm)].reduce((a, m) => a + Number(m[1]), 0);
  assert.ok(bounded < timeout, `the step bounds (${bounded}) fit inside the job's (${timeout}), so an expiry names its step`);
  assert.match(text, /if \[ "\$EVENT" = "pull_request" \]; then\n\s+n=1; arms=focus,tiled; runs=1; baseline=none/, "the PR smoke: N = 1, one run, head only");
  for (const s of steps(job).filter((x) => /baseline/i.test(x.name) && !/Resolve/.test(x.name))) {
    assert.match(s.text, /if: steps\.params\.outputs\.baseline != 'none'/, s.name);
  }
});

test("every option the job passes to cli.mjs is one the CLI accepts", () => {
  const job = jobLines(yml(), "run-windows-hosted");
  const all = invocations(job);
  assert.ok(all.length >= 7, `found ${all.length} invocations: the extraction broke`);
  const used = new Set(all.flatMap(flagsOf));
  for (const f of ["env", "setups", "runner-mode", "runner-exe", "head-commit", "ab-baseline", "baseline-exe", "probe", "out", "smoke-setups", "vscode-cache-dir", "require-measured", "require-measured-if-operative", "at-n", "require-rows-n", "redact-to", "format"]) {
    assert.ok(used.has(f), `the job passes --${f}`);
  }
  assert.deepEqual([...used].filter((f) => !KNOWN_FLAGS.includes(f)), [], "options the CLI would refuse");
});

test("the runner A/B and the VS Code smoke are separate runs: their own setups, runs, work dirs and results files", () => {
  const job = jobLines(yml(), "run-windows-hosted");
  const all = invocations(job);
  const runnerRun = all.find((x) => /args=\(run --env windows-hosted --setups runner\b/.test(x));
  assert.ok(runnerRun, "the A/B: windows-hosted, the runner alone, subject mode");
  assert.match(runnerRun, /--runner-mode subject/);
  assert.match(runnerRun, /--runs "\$RUNS"/);
  assert.match(runnerRun, /--work-dir "\$RUNNER_TEMP\/tb" /);
  assert.ok(all.some((x) => /args\+=\(--ab-baseline "\$BASELINE"[\s\S]*--baseline-exe/.test(x)), "the baseline build joins the same run (D11)");
  const smoke = all.find((x) => /cli\.mjs run --env windows-hosted --setups vscode\b/.test(x));
  assert.ok(smoke, "the smoke: windows-hosted, VS Code alone");
  assert.match(smoke, /--runs 1 --smoke-setups vscode/);
  assert.match(smoke, /--vscode-cache-dir "\$RUNNER_TEMP\/termhost-cache"/, "the archive cache sits outside every checkout");
  assert.match(smoke, /--work-dir "\$RUNNER_TEMP\/tb-vscode"/);
  assert.match(smoke, /--out "\$RUNNER_TEMP\/tb-vscode-out\/results\.json"/);
  const vscodeStep = steps(job).find((s) => s.name === "Run the VS Code smoke");
  assert.match(vscodeStep.text, /if: \$\{\{ !cancelled\(\) && steps\.probe\.outcome == 'success' \}\}/, "it runs even when a runner step failed");
});

test("the gate: the process and producer KPIs always, output latency where capture.wgc is OPERATIVE — for the runner and for VS Code", () => {
  const all = invocations(jobLines(yml(), "run-windows-hosted"));
  const gates = all.filter((x) => /--require-measured /.test(x));
  assert.equal(gates.length, 2, "one gate per run");
  for (const [setup, g] of [["runner", gates.find((x) => /--setups runner/.test(x))], ["vscode", gates.find((x) => /--setups vscode/.test(x))]]) {
    assert.ok(g, `${setup} has a gate`);
    assert.match(g, new RegExp(`--setups ${setup} --arms "\\$ARMS" --require-rows-n "\\$N"`), "rows for every N and arm, never missing");
    const always = /--require-measured ([a-z_,]+)/.exec(g)[1].split(",");
    assert.deepEqual(always, ["cpu_pct", "memory_mb", "producer_stall_pct"]);
    const [mechanism, kpis] = /--require-measured-if-operative ([^\s]+)/.exec(g)[1].split(":");
    assert.equal(mechanism, "capture.wgc");
    assert.ok(PROBE_MECHANISMS.includes(mechanism), "a mechanism the probe's doctor reports");
    assert.deepEqual(kpis.split(","), ["output_latency_ms"]);
    for (const id of [...always, ...kpis.split(",")]) assert.ok(KPI_CATALOG.some((k) => k.id === id), id);
    assert.match(g, /--at-n "\$GATE_N"/);
    assert.match(g, /--format json/, "the full file is reported as failures, never rendered");
  }
});

test("only the runner's rows leave the VM: uploads and the summary come from the redacted copy and runner unit dirs (D7)", () => {
  const job = jobLines(yml(), "run-windows-hosted");
  const uploads = steps(job).filter((s) => /uses: actions\/upload-artifact@/.test(s.text));
  assert.ok(uploads.length >= 2);
  const paths = [];
  for (const s of uploads) {
    const at = s.lines.findIndex((l) => /^\s+path:/.test(l));
    const single = /^\s+path: (?!\|)(\S.*)$/.exec(s.lines[at]);
    if (single) paths.push(single[1]);
    else for (let i = at + 1; i < s.lines.length && /^ {12}\S/.test(s.lines[i]); i += 1) paths.push(s.lines[i].trim());
  }
  for (const p of paths) {
    assert.ok(/termhost-bench-windows-hosted-runner\.json$/.test(p) || /\/tb\/termhost-bench\/\*\/units\/\*-runner-\*\//.test(p), `upload path ${p} could carry a third-party row`);
    assert.ok(!/tb-vscode|termhost-cache|results\.json$/.test(p), `upload path ${p} reaches the VS Code smoke`);
  }
  const summary = job.filter((l) => /cli\.mjs (report|compare)/.test(l) && /GITHUB_STEP_SUMMARY/.test(l));
  assert.ok(summary.length >= 2);
  for (const l of summary) assert.match(l, /"\$redacted"/, l.trim());
  for (const inv of invocations(job).filter((x) => /cli\.mjs report "\$results"/.test(x))) assert.match(inv, /--format json/);
  const smokeAssert = steps(job).find((s) => /Assert the VS Code smoke/.test(s.name));
  assert.doesNotMatch(smokeAssert.text, /GITHUB_STEP_SUMMARY|upload-artifact/);
});

test("the display step is best effort, and the doctor reports the vscode facts with the same cache", () => {
  const job = jobLines(yml(), "run-windows-hosted");
  const display = steps(job).find((s) => s.name === "Set the display resolution");
  assert.match(display.text, /continue-on-error: true/);
  assert.match(display.text, /shell: powershell/);
  const doctor = invocations(job).find((x) => /cli\.mjs doctor --env windows-hosted --setups runner,vscode/.test(x));
  assert.ok(doctor);
  assert.match(doctor, /--vscode-cache-dir "\$RUNNER_TEMP\/termhost-cache"/);
});
