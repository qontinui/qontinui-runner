/**
 * D12 primary-state neutrality (`neutrality.mjs`): the primary runner's
 * settings.json, paired_user.json and auth_tokens.enc are digested, and a change
 * mid-run is an incident that STOPS the run — never smoothed over, never attributed
 * to the harness by assumption.
 *
 *   node --test scripts/termhost-bench/__tests__/neutrality.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { machineBlock } from "../machine.mjs";
import { NeutralityGuard, diffSnapshots, digestFile, neutralityVerdict, platformDirs, primaryStatePaths, snapshot } from "../neutrality.mjs";
import { runBench } from "../run.mjs";
import { validateResults } from "../validate.mjs";
import { FakeAdapter, fakeProbe, fakeProbeBinary, fakeRunOptions, healthyFacts, VirtualClock } from "./helpers/fake-bench.mjs";

test("the primary's files live where the runner's own resolution puts them", () => {
  const win = primaryStatePaths({ platform: "win32", env: { APPDATA: "C:\\U\\AppData\\Roaming", LOCALAPPDATA: "C:\\U\\AppData\\Local" }, home: "C:\\U" });
  assert.match(win[0].path, /Roaming[\\/]com\.qontinui\.runner[\\/]settings\.json$/);
  assert.match(win[1].path, /Local[\\/]com\.qontinui\.runner[\\/]paired_user\.json$/);
  assert.match(win[2].path, /Local[\\/]com\.qontinui\.runner[\\/]auth_tokens\.enc$/);
  const linux = primaryStatePaths({ platform: "linux", env: {}, home: "/home/u" });
  assert.match(linux[0].path.replace(/\\/g, "/"), /\/home\/u\/\.config\/com\.qontinui\.runner\/settings\.json$/);
  assert.match(linux[1].path.replace(/\\/g, "/"), /\/home\/u\/\.local\/share\/com\.qontinui\.runner\/paired_user\.json$/);
  // XDG overrides are honoured only when absolute, as the dirs crate does.
  assert.equal(platformDirs({ platform: "linux", env: { XDG_CONFIG_HOME: "relative" }, home: "/h" }).configDir.replace(/\\/g, "/"), "/h/.config");
});

test("digests distinguish present, absent and unreadable files", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-neu-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const f = join(dir, "settings.json");
  writeFileSync(f, "{}");
  assert.equal(digestFile(f).state, "present");
  assert.match(digestFile(f).sha256, /^sha256:[0-9a-f]{64}$/);
  assert.deepEqual(digestFile(join(dir, "missing.json")), { state: "absent" });
  const unreadable = digestFile(f, { read: () => { const e = new Error("denied"); e.code = "EACCES"; throw e; } });
  assert.equal(unreadable.state, "unreadable");
});

test("a changed, created or deleted file is a change; an untouched one is not", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-neu-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const paths = [
    { label: "settings.json", path: join(dir, "settings.json") },
    { label: "paired_user.json", path: join(dir, "paired_user.json") },
    { label: "auth_tokens.enc", path: join(dir, "auth_tokens.enc") },
  ];
  writeFileSync(paths[0].path, "a");
  writeFileSync(paths[1].path, "b");
  const before = snapshot(paths);
  assert.deepEqual(diffSnapshots(before, snapshot(paths)), []);
  writeFileSync(paths[0].path, "a2");
  unlinkSync(paths[1].path);
  writeFileSync(paths[2].path, "c");
  const changes = diffSnapshots(before, snapshot(paths));
  assert.deepEqual(changes.map((c) => c.label), ["settings.json", "paired_user.json", "auth_tokens.enc"]);
  assert.equal(changes[1].after, "absent");
  assert.equal(changes[2].before, "absent");
});

test("the guard's incident stops the run and does not blame the harness", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-neu-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const settings = join(dir, "settings.json");
  writeFileSync(settings, "x");
  const guard = new NeutralityGuard({ paths: [{ label: "settings.json", path: settings }] });
  guard.baseline();
  assert.equal(guard.check("first").changed, false);
  writeFileSync(settings, "y");
  const res = guard.check("before unit 3 arm focus");
  assert.equal(res.changed, true);
  assert.equal(res.incident.kind, "primary_state_changed");
  assert.equal(res.incident.stops_run, true);
  assert.match(res.incident.detail, /writer unattributed/i);
  assert.match(res.incident.detail, /before unit 3 arm focus/);
});

test("D12 fixture: the primary's settings.json changing mid-run produces an incident and stops the run", async (t) => {
  const tmp = mkdtempSync(join(tmpdir(), "tb-d12-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  mkdirSync(join(tmp, "primary"), { recursive: true });
  const settings = join(tmp, "primary", "settings.json");
  writeFileSync(settings, '{"terminal": {}}');
  const clock = new VirtualClock();
  const calls = [];
  let current = null;
  const res = await runBench(
    fakeRunOptions(join(tmp, "work"), { out: join(tmp, "results.json"), ns: [1, 5], setups: ["runner", "tmux"] }),
    {
      log: () => {},
      sleep: clock.sleep,
      startEnvironment: async () => ({ env: { DISPLAY: ":99" }, screen: { width: 1920, height: 1080 }, display: ":99", stop: async () => {} }),
      probePath: fakeProbeBinary(tmp),
      createAdapter: async (setup, o) => {
        current = new FakeAdapter({
          id: setup,
          clock,
          variant: o.variant,
          calls,
          // A concurrent writer rewrites the primary's settings during the first runner
          // arm measured (tiled: it measures before focus).
          onArrange: (arm, unit) => {
            if (setup === "runner" && unit === 0 && arm === "tiled") writeFileSync(settings, '{"terminal": {"pty_holder": true}}');
          },
        });
        return current;
      },
      probe: fakeProbe({ clock, current: () => current }),
      gatherDoctorFacts: async () => healthyFacts(),
      sampleBackgroundLoad: async () => ({ cpu_pct: 1, memory_used_mb: 100, sampled_ms: 1 }),
      machineBlock: (o) => machineBlock({ ...o, gpu: { state: "unknown", reason: "not_instrumented", detail: "test" } }),
      neutrality: new NeutralityGuard({ paths: [{ label: "settings.json", path: settings }] }),
      reapGenerators: async () => ({ ended: [], survivors: [] }),
      primaryPresent: async () => false,
    },
  );
  assert.ok(res.stopped, "the run stopped");
  assert.equal(res.stopped.kind, "primary_state_changed");
  const inc = res.doc.incidents.filter((i) => i.kind === "primary_state_changed");
  assert.equal(inc.length, 1);
  assert.equal(inc[0].stops_run, true);
  assert.match(inc[0].detail, /after u000-runner/);
  // Nothing after the incident launched: only unit 0 ran, and it was torn down.
  assert.deepEqual(calls.filter((c) => c[0] === "launch").map((c) => c[1]), [0]);
  assert.deepEqual(calls.filter((c) => c[0] === "teardown").map((c) => c[1]), [0]);
  // The file is still written, valid, with the stop recorded beside the rows.
  assert.equal(validateResults(res.doc).valid, true);
  // The arm measured after the change (focus: tiled measures first) never ran.
  const focus = res.doc.rows.find((r) => r.scenario.arm === "focus");
  assert.equal(focus.kpis.output_latency_ms.state, "unknown");
  assert.match(focus.kpis.output_latency_ms.detail, /the run stopped/);
});

test("the guard's verdict: watching is OPERATIVE, nothing-to-protect says so, and watching nothing beside a primary is UNKNOWN (review S7)", () => {
  const present = { label: "settings.json", state: "present", sha256: `sha256:${"ab".repeat(32)}` };
  const absent = (label) => ({ label, state: "absent" });
  const all = [absent("settings.json"), absent("paired_user.json"), absent("auth_tokens.enc")];
  assert.equal(neutralityVerdict([present, absent("paired_user.json"), absent("auth_tokens.enc")], true).state, "OPERATIVE");
  const none = neutralityVerdict(all, false);
  assert.equal(none.state, "OPERATIVE");
  assert.match(none.reason, /nothing to protect/);
  assert.equal(neutralityVerdict(all, true).state, "UNKNOWN", "a primary is present, and the guard would watch nothing");
  assert.equal(neutralityVerdict(all, null).state, "UNKNOWN", "an unanswered :9876 probe may be a primary");
  const unreadable = neutralityVerdict([{ label: "settings.json", state: "unreadable", error: "EACCES" }, absent("paired_user.json"), absent("auth_tokens.enc")], false);
  assert.equal(unreadable.state, "UNKNOWN", "a file that exists but cannot be read is not absent");
});
