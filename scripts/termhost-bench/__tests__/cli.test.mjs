/**
 * The CLI (`cli.mjs`) and the CI job that drives it (`.github/workflows/termhost-bench.yml`
 * `run-linux-xvfb`): argv parsing, run-option normalisation (D11 build roles, D12
 * launch modes), and the workflow's contract — every option it passes exists, the
 * gate's KPIs are the plan's, and nothing but the runner's rows is uploaded or
 * summarised (D7). The weekly job is otherwise first exercised on a hosted VM, so a
 * typo here would surface a week late; this catches it at PR time.
 *
 *   node --test scripts/termhost-bench/__tests__/cli.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { KNOWN_FLAGS, UsageError, main, normaliseRunOptions, parseArgv } from "../cli.mjs";
import { KPI_CATALOG } from "../kpi-catalog.mjs";
import { InvalidResults } from "../results.mjs";
import { RunAbort, RunFailed } from "../run.mjs";
import { linuxXvfbResults } from "./helpers/results.mjs";
import { healthyFacts } from "./helpers/fake-bench.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKFLOW = join(HERE, "..", "..", "..", ".github", "workflows", "termhost-bench.yml");
const SHA_H = "1".repeat(40);
const SHA_B = "2".repeat(40);
const resolve = (ref) => (ref === "HEAD" ? SHA_H : ref === "base" ? SHA_B : ref);

// ---------------------------------------------------------------------------
// argv
// ---------------------------------------------------------------------------

test("argv: values, --k=v, booleans, repeatable --runner-env, positionals", () => {
  const p = parseArgv(["report", "r.json", "--format=json", "--at-n", "15", "--json", "--runner-env", "A=1", "--runner-env", "B=2"]);
  assert.equal(p.command, "report");
  assert.deepEqual(p.positional, ["r.json"]);
  assert.deepEqual(p.flags, { "runner-env": ["A=1", "B=2"], format: "json", "at-n": "15", json: true });
});

test("argv: an unknown option, and an option missing its value, are usage errors", () => {
  assert.throws(() => parseArgv(["run", "--soak", "5"]), (e) => e instanceof UsageError && /unknown option --soak/.test(e.message));
  assert.throws(() => parseArgv(["run", "--n"]), /--n needs a value/);
  assert.throws(() => parseArgv(["run", "--n", "--arms", "focus"]), /--n needs a value/);
});

test("main: usage errors exit 2 with the help text; help exits 0", async () => {
  const err = [];
  assert.equal(await main(["run", "--bogus"], { out: () => {}, err: (s) => err.push(s) }), 2);
  assert.match(err.join(""), /unknown option --bogus[\s\S]*doctor/);
  const out = [];
  assert.equal(await main(["--help"], { out: (s) => out.push(s), err: () => {} }), 0);
  assert.match(out.join(""), /termhost-bench/);
  assert.equal(await main(["frobnicate"], { out: () => {}, err: () => {} }), 2);
});

// ---------------------------------------------------------------------------
// run options
// ---------------------------------------------------------------------------

test("subject mode with --ab-baseline: head and baseline builds, each its own exe (D11, D12)", async () => {
  const o = await normaliseRunOptions(
    { env: "linux-xvfb", setups: "runner,tmux", n: "1,5,15", arms: "focus,tiled", runs: "2", "runner-mode": "subject", "runner-exe": "/b/head/runner", "head-commit": "HEAD", "ab-baseline": "base", "baseline-exe": "/b/base/runner", "runner-env": ["WEBKIT_DISABLE_DMABUF_RENDERER=1"] },
    { resolve },
  );
  assert.deepEqual(o.ns, [1, 5, 15]);
  assert.equal(o.runs, 2);
  assert.deepEqual(o.runnerBuilds.map((b) => [b.role, b.commit]), [["head", SHA_H], ["baseline", SHA_B]]);
  assert.match(o.runnerBuilds[1].exe.replace(/\\/g, "/"), /\/b\/base\/runner$/);
  assert.deepEqual(o.runnerEnv, { WEBKIT_DISABLE_DMABUF_RENDERER: "1" });
  await assert.rejects(
    normaliseRunOptions({ env: "linux-xvfb", setups: "runner", "runner-mode": "subject", "runner-exe": "/x", "ab-baseline": "base" }, { resolve }),
    /needs --baseline-exe/,
  );
});

test("without a baseline the one build is `single`; auto mode asks whether a supervisor answers", async () => {
  const sub = await normaliseRunOptions({ env: "linux-xvfb", setups: "runner", "runner-exe": "/x" }, { resolve, probeSupervisor: async () => false });
  assert.equal(sub.runnerMode, "subject");
  assert.deepEqual(sub.runnerBuilds.map((b) => b.role), ["single"]);
  const sup = await normaliseRunOptions({ env: "linux-xvfb", setups: "runner", "head-ref": "HEAD" }, { resolve, probeSupervisor: async () => true });
  assert.equal(sup.runnerMode, "supervisor");
  assert.deepEqual(sup.runnerBuilds[0], { role: "single", commit: SHA_H, gitRef: SHA_H, worktreePath: null, source: "git_ref" });
  await assert.rejects(normaliseRunOptions({ env: "linux-xvfb", setups: "runner", "runner-mode": "supervisor" }, { resolve }), /exactly one of --head-ref/);
  const tmuxOnly = await normaliseRunOptions({ env: "linux-xvfb", setups: "tmux" }, { resolve, probeSupervisor: async () => assert.fail("no runner, no probe") });
  assert.deepEqual(tmuxOnly.runnerBuilds, []);
});

test("run options refuse unknown setups, arms and malformed numbers", async () => {
  await assert.rejects(normaliseRunOptions({ env: "linux-xvfb", setups: "kitty" }, { resolve }), /unknown setup kitty/);
  await assert.rejects(normaliseRunOptions({ env: "linux-xvfb", setups: "tmux", arms: "grid" }, { resolve }), /unknown arm grid/);
  await assert.rejects(normaliseRunOptions({ env: "linux-xvfb", setups: "tmux", n: "1,0" }, { resolve }), /positive integers/);
  await assert.rejects(normaliseRunOptions({ env: "linux-xvfb", setups: "tmux", runs: "0" }, { resolve }), /--runs must be an integer >= 1/);
  await assert.rejects(normaliseRunOptions({ env: "linux-xvfb", setups: "tmux", "runner-env": ["NOEQUALS"] }, { resolve }), /KEY=VALUE/);
  await assert.rejects(normaliseRunOptions({ setups: "tmux" }, { resolve }), /--env is required/);
});

// ---------------------------------------------------------------------------
// the workflow's contract
// ---------------------------------------------------------------------------

/** The text of one job of the workflow (from its key line to the next job key). */
function jobText(yml, key) {
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

/**
 * Every cli.mjs invocation of the job: a `node scripts/termhost-bench/cli.mjs <cmd>`
 * line with its `\`-continued lines, and the run step's `args=( … )` /
 * `args+=( … )` arrays.
 */
function cliInvocations(lines) {
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

test("every option the run job passes to cli.mjs is one the CLI accepts", () => {
  const job = jobText(readFileSync(WORKFLOW, "utf8"), "run-linux-xvfb");
  const invocations = cliInvocations(job);
  assert.ok(invocations.length >= 6, `found ${invocations.length} invocations: the extraction itself broke`);
  const used = new Set(invocations.flatMap(flagsOf));
  for (const f of ["env", "setups", "runner-mode", "runner-exe", "head-commit", "ab-baseline", "baseline-exe", "smoke-setups", "probe", "out", "require-measured", "at-n", "require-rows-n", "redact-to", "format"]) {
    assert.ok(used.has(f), `the job passes --${f}`);
  }
  const unknown = [...used].filter((f) => !KNOWN_FLAGS.includes(f));
  assert.deepEqual(unknown, [], "options the CLI would refuse");
  // The extraction can fail: a planted typo is caught.
  assert.deepEqual(flagsOf("node scripts/termhost-bench/cli.mjs run --soak 5").filter((f) => !KNOWN_FLAGS.includes(f)), ["soak"]);
});

test("the gate asserts the plan's four KPIs, all of them in the catalog", () => {
  const job = jobText(readFileSync(WORKFLOW, "utf8"), "run-linux-xvfb").join("\n");
  const m = /--require-measured ([a-z_,]+)/.exec(job);
  assert.ok(m, "the job asserts measured KPIs");
  assert.deepEqual(m[1].split(","), ["output_latency_ms", "cpu_pct", "memory_mb", "producer_stall_pct"]);
  for (const id of m[1].split(",")) assert.ok(KPI_CATALOG.some((k) => k.id === id), id);
  assert.match(job, /--require-rows-n "\$N"/, "rows are required for the whole ladder");
  assert.match(job, /--setups runner,tmux --arms "\$ARMS"/);
});

test("only the runner's rows leave the job: the artifact and the summary come from the redacted copy (D7)", () => {
  const job = jobText(readFileSync(WORKFLOW, "utf8"), "run-linux-xvfb");
  // The job's steps, split at step indentation; the upload steps' `path:` values.
  const steps = [];
  for (const l of job) {
    if (/^ {6}- /.test(l)) steps.push([l]);
    else if (steps.length > 0) steps.at(-1).push(l);
  }
  const uploads = steps.filter((s) => s.some((l) => /uses: actions\/upload-artifact@/.test(l)));
  assert.ok(uploads.length >= 2, `upload steps found: ${uploads.length}`);
  const paths = [];
  for (const s of uploads) {
    const at = s.findIndex((l) => /^\s+path:/.test(l));
    assert.ok(at > 0, `an upload step without a path: ${s[0].trim()}`);
    const single = /^\s+path: (?!\|)(\S.*)$/.exec(s[at]);
    if (single) paths.push(single[1]);
    else for (let i = at + 1; i < s.length && /^ {12}\S/.test(s[i]); i += 1) paths.push(s[i].trim());
  }
  assert.ok(paths.length >= uploads.length, `upload paths found: ${paths.length}`);
  // Every upload path names the redacted file or a runner unit directory.
  for (const p of paths) {
    assert.ok(/termhost-bench-linux-xvfb-runner\.json$/.test(p) || /\/units\/\*-runner-\*\//.test(p), `upload path ${p} could carry a third-party row`);
  }
  assert.ok(!paths.some((p) => /results\.json$/.test(p)), "the full results file is never uploaded");
  // The summary renders the redacted copy only.
  const summary = job.filter((l) => /cli\.mjs (report|compare)/.test(l) && /GITHUB_STEP_SUMMARY/.test(l));
  assert.ok(summary.length >= 2);
  for (const l of summary) assert.match(l, /"\$redacted"/, l.trim());
  // The full file is only ever reported with --format json (failures, not values).
  for (const inv of cliInvocations(job).filter((t) => /cli\.mjs report "\$results"/.test(t))) assert.match(inv, /--format json/);
});

test("the benchmark runs on its own scope: a PR touching the bench, schedule and dispatch — never push", () => {
  const yml = readFileSync(WORKFLOW, "utf8");
  const run = jobText(yml, "run-linux-xvfb").join("\n");
  assert.match(run, /^ {4}needs: bench-scope$/m);
  assert.match(run, /^ {4}if: needs\.bench-scope\.outputs\.run == 'true'$/m);
  const scope = jobText(yml, "bench-scope").join("\n");
  assert.match(scope, /schedule\|workflow_dispatch\) run=true ;;/);
  assert.match(scope, /\*\) run=false ;;/, "push (and anything else) does not run it");
  const pattern = /grep -qE '([^']+)'/.exec(scope);
  assert.ok(pattern, "the PR scope is a path pattern");
  const re = new RegExp(pattern[1]);
  for (const p of ["scripts/termhost-bench/run.mjs", "crates/termhost-probe/src/watch.rs", "scripts/lib/runner-clients.mjs", ".github/workflows/termhost-bench.yml"]) {
    assert.ok(re.test(p), `${p} is in the plan's smoke scope`);
  }
  for (const p of ["Cargo.lock", "Cargo.toml", "rust-toolchain.toml", "src-tauri/src/main.rs", ".github/actions/checkout-sibling/action.yml"]) {
    assert.ok(!re.test(p), `${p} rebuilds the probe but does not run the benchmark`);
  }
});

test("the job builds the baseline only when there is one, and the smoke is N = 1, one run, head only", () => {
  const yml = readFileSync(WORKFLOW, "utf8");
  const job = jobText(yml, "run-linux-xvfb").join("\n");
  assert.match(job, /if \[ "\$EVENT" = "pull_request" \]; then\n\s+n=1; arms=focus,tiled; runs=1; baseline=none/);
  for (const step of ["Check out the baseline", "Check out sibling qontinui-schemas (baseline)", "Build the baseline runner"]) {
    const at = job.indexOf(`- name: ${step}`);
    assert.ok(at >= 0, step);
    assert.match(job.slice(at, at + 200), /if: steps\.params\.outputs\.baseline != 'none'/, step);
  }
  assert.match(yml, /^ {2}schedule:\n {4}- cron: "[^"]+"$/m, "a weekly schedule");
  assert.match(yml, /^ {2}workflow_dispatch:\n[\s\S]*?baseline_sha:/m, "dispatch takes a baseline");
});

// ---------------------------------------------------------------------------
// Review round 1
// ---------------------------------------------------------------------------

test("auto mode refuses to guess when the supervisor probe is unanswered (D12)", async () => {
  await assert.rejects(
    normaliseRunOptions({ env: "linux-xvfb", setups: "runner", "runner-exe": "/x" }, { resolve, probeSupervisor: async () => null }),
    (e) => e instanceof UsageError && /cannot tell whether a supervisor answers/.test(e.message),
  );
});

test("exit codes follow the header: 3 when the doctor blocks, 1 when a run fails or stops, 0 otherwise", async () => {
  const quiet = { out: () => {}, err: () => {} };
  const runWith = (impl) => main(["run", "--env", "linux-xvfb", "--setups", "tmux"], quiet, { runBench: impl });
  assert.equal(await runWith(async () => ({ outPath: "/x", stopped: null })), 0);
  assert.equal(await runWith(async () => ({ outPath: "/x", stopped: { detail: "interrupted" } })), 1);
  assert.equal(await runWith(async () => { throw new RunAbort("the doctor reports a run cannot start here"); }), 3);
  assert.equal(await runWith(async () => { throw new RunFailed("no row was produced"); }), 1, "no row is a failure, not a doctor refusal");
  assert.equal(await runWith(async () => { throw new InvalidResults("invalid", "/r/results.invalid.json"); }), 1);
  const doctor = (facts) => main(["doctor", "--env", "linux-xvfb", "--setups", "tmux"], quiet, { gatherDoctorFacts: async () => facts, startEnvironment: async () => ({ env: {}, display: ":99", stop: async () => {} }) });
  assert.equal(await doctor(healthyFacts()), 0);
  assert.equal(await doctor({ ...healthyFacts(), drain: { state: "drained", detail: "coord reports this device drained" } }), 3, "a drained device blocks the run");
  assert.equal(await doctor({ ...healthyFacts(), xvfbPath: null }), 3);
});

test("an invalid results file is reported by path and keyword only, never with the value", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-cli-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const doc = linuxXvfbResults();
  doc.rows[1].kpis.jank_share = { state: "measured", unit: "%", n: 500, value: 143.75 };
  const file = join(dir, "results.json");
  writeFileSync(file, JSON.stringify(doc));
  const out = [];
  assert.equal(await main(["report", file], { out: (x) => out.push(x), err: () => {} }), 1);
  const text = out.join("");
  assert.match(text, /^INVALID: /m);
  assert.match(text, /\/rows\/1\/kpis\/jank_share\/value \[maximum\]/);
  assert.doesNotMatch(text, /143/, "a third-party reading never reaches the log (D7)");
});

test("the bench job's Doctor step writes its summary when the doctor blocks, then fails with its exit code (review round 2 nit 2)", (t) => {
  const yml = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", ".github", "workflows", "termhost-bench.yml"), "utf8");
  const at = yml.indexOf("node scripts/termhost-bench/cli.mjs doctor");
  assert.ok(at > 0, "the step runs the bench doctor");
  const runAt = yml.lastIndexOf("run: |\n", at);
  const lines = yml.slice(runAt + "run: |\n".length).split("\n");
  const indent = lines[0].match(/^ */)[0].length;
  const body = [];
  for (const l of lines) {
    if (l.trim() !== "" && l.match(/^ */)[0].length < indent) break;
    body.push(l.slice(indent));
  }
  const tmp = mkdtempSync(join(tmpdir(), "tb-step-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  const fakeNode = join(tmp, "fake-node.sh").replace(/\\/g, "/");
  writeFileSync(fakeNode, 'echo "BLOCKED device.drain UNKNOWN"\nexit 3\n');
  // The step as written, with the doctor replaced by one that blocks (exit 3).
  const script = body.join("\n").replace("node scripts/termhost-bench/cli.mjs doctor", `sh "${fakeNode}" doctor`);
  writeFileSync(join(tmp, "step.sh"), script);
  const summary = join(tmp, "summary.md").replace(/\\/g, "/");
  const res = spawnSync("bash", ["--noprofile", "--norc", "-eo", "pipefail", join(tmp, "step.sh").replace(/\\/g, "/")], {
    cwd: tmp,
    env: { ...process.env, GITHUB_STEP_SUMMARY: summary, RUNNER_TEMP: tmp.replace(/\\/g, "/"), GITHUB_WORKSPACE: tmp.replace(/\\/g, "/") },
    encoding: "utf8",
  });
  if (res.error) return t.skip(`no bash here: ${res.error.message}`);
  assert.equal(res.status, 3, res.stderr);
  const md = readFileSync(summary, "utf8");
  assert.match(md, /### termhost-bench doctor \(linux-xvfb\)/);
  assert.match(md, /BLOCKED device\.drain UNKNOWN/);
});

test("report --ab-baseline fails the gate on an unproven runner build at the gate N (review round 3)", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-cli-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const doc = linuxXvfbResults();
  const r = doc.rows.find((x) => x.setup.id === "runner" && x.scenario.n === 5 && x.scenario.arm === "focus");
  r.setup.build = { state: "unknown", reason: "harness_error", detail: "never proven", role: r.setup.build.role };
  r.labels = [...(r.labels ?? []), "unknown_build"];
  const file = join(dir, "results.json");
  writeFileSync(file, JSON.stringify(doc));
  const gate = ["report", file, "--format", "json", "--setups", "runner", "--arms", "focus", "--require-measured", "cpu_pct", "--at-n", "5"];
  const err = [];
  const io = { out: () => {}, err: (x) => err.push(x) };
  assert.equal(await main(gate, io), 0, "without --ab-baseline the gate is unchanged");
  assert.equal(await main([...gate, "--ab-baseline", "b".repeat(40)], io), 1);
  assert.match(err.join(""), /the build is unproven \(unknown_build\)/);
});
