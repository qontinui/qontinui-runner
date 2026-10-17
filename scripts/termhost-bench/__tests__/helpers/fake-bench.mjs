/**
 * Fakes for driving `run.mjs` end to end without a display, a runner or a probe:
 * a virtual clock (the run loop's injected `sleep` advances it, so a 40 s soak
 * takes no wall time), a fake adapter that writes real slot files and generator
 * logs into the unit's gen dir, and a fake probe driver whose watch segments are
 * built from those logs (`bench-fixtures.mjs`).
 */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { readSlots } from "../../sessions.mjs";
import { MS, genLog, watchSegment } from "./bench-fixtures.mjs";

export class VirtualClock {
  constructor(start = 5_000_000_000_000n) {
    this.ns = start;
  }
  now() {
    return this.ns;
  }
  sleep = async (ms) => {
    this.ns += BigInt(Math.round(ms * 1e6));
  };
}

const RUNNER_BUILD = { commit: "a".repeat(40), source: "artifact", role: "single" };

/**
 * A fake adapter. `opts.refuseAt` makes spawnSessions report a resource-guard
 * refusal at that N; `opts.onArrange(arm, unitIndex)` runs inside arrange (the
 * neutrality test changes the "primary" settings.json there).
 */
export class FakeAdapter {
  constructor({ id, clock, variant = "default", build = null, refuseAt = null, onArrange = null, calls = [] }) {
    this.id = id;
    this.hostIdKey = "TERMHOST_BENCH_TERMINAL_ID";
    this.clock = clock;
    this.variant = variant;
    this.build = build;
    this.refuseAt = refuseAt;
    this.onArrange = onArrange;
    this.calls = calls;
    this.visibleSet = new Set();
    this.genDir = null;
    this.nextHost = 0;
    this.argv = null;
  }
  async check() {
    return { mechanisms: { [`setup.${this.id}`]: { state: "OPERATIVE", reason: "fake" } } };
  }
  async launch(ctx) {
    this.calls.push(["launch", ctx.unit.index]);
    this.genDir = ctx.genDir;
    this.unit = ctx.unit;
    return { ok: true };
  }
  writeSession() {
    const k = readSlots(this.genDir).size;
    const hostId = `h${this.nextHost++}`;
    writeFileSync(
      join(this.genDir, `slot-${k}`),
      `${JSON.stringify({ schema: "termhost-bench/slot/v1", slot: k, run_id: "x", pid: 4_000_000 + k, ppid: 3_000_000 + k, host_terminal_id: { TERMHOST_BENCH_TERMINAL_ID: hostId }, claimed_at_ns: String(this.clock.now()), argv: this.argv })}\n`,
    );
    const recs = genLog({ session: k, t0: this.clock.now() - 3000n * MS, durationMs: 800_000, blockedNsPerWrite: 500_000 });
    writeFileSync(join(this.genDir, `gen-${k}.jsonl`), recs.map((r) => JSON.stringify(r)).join("\n") + "\n");
    return { k, hostId };
  }
  async spawnSessions(n, argv) {
    this.calls.push(["spawnSessions", n]);
    if (!Array.isArray(argv) || !argv.includes("--termhost-bench-generator") || !argv.includes("--echo")) {
      throw new Error(`argv is not the D13 command line: ${JSON.stringify(argv)}`);
    }
    this.argv = argv;
    if (this.refuseAt !== null && n >= this.refuseAt) {
      return { sessions: [], admission: { refused: true, lane: "thread_count", n_reached: n - 1, detail: "fake refusal", threadCeilings: { warn: 256, critical: 400 } } };
    }
    const sessions = [];
    for (let i = 0; i < n; i += 1) {
      const tCmdNs = this.clock.now();
      const { hostId } = this.writeSession();
      sessions.push({ hostId, tCmdNs });
    }
    return { sessions, admission: null };
  }
  async arrange(arm, actx) {
    this.calls.push(["arrange", arm]);
    this.slots = actx.slots;
    await this.onArrange?.(arm, this.unit.index);
    const order = [...actx.slots.keys()].sort((a, b) => a - b);
    this.visibleSet = new Set(arm === "focus" ? [order[0]] : order.slice(0, 9));
    return { ok: true, vTarget: arm === "focus" ? 1 : Math.min(order.length, 9), focusSession: order[0], switchable: order, paneSizes: [{ cols: 120, rows: 40 }] };
  }
  visible() {
    return [...this.visibleSet];
  }
  async show(k) {
    this.visibleSet = new Set([k]);
    return { ok: true, tCmdNs: this.clock.now() };
  }
  async spawnOne() {
    const tCmdNs = this.clock.now();
    const { hostId, k } = this.writeSession();
    this.visibleSet.add(k);
    this.lastSpawned = hostId;
    return { ok: true, hostId, tCmdNs };
  }
  async closeOne(hostId) {
    this.calls.push(["closeOne", hostId]);
    return { ok: true };
  }
  async focusObserved() {
    return { ok: true, session: [...this.visibleSet][0] };
  }
  processRoots() {
    return [process.pid];
  }
  windowTarget() {
    return { pid: process.pid };
  }
  describe() {
    if (this.id === "runner") {
      return {
        id: "runner",
        variant: this.variant,
        app_version: "qontinui-runner fake",
        build: { ...RUNNER_BUILD, ...(this.build ?? {}) },
        settings_digest: { state: "not_applicable", reason: "not_applicable_to_setup" },
        pty_host_impl: "unix_pty",
        render_backend: "xterm",
        pty_holder: false,
        launch_mode: "subject_instance_root",
        session_shell: "bash",
        shell_integration: "injected",
      };
    }
    return {
      id: this.id,
      variant: this.variant,
      app_version: "tmux 3.2a in XTerm(372)",
      build: { state: "not_applicable", reason: "not_applicable_to_setup" },
      settings_digest: `sha256:${"cd".repeat(32)}`,
      pty_host_impl: "unix_pty",
      render_backend: "xterm_xft",
      pty_holder: { state: "not_applicable", reason: "not_applicable_to_setup" },
      launch_mode: "private_server",
      session_shell: "bash -i",
      shell_integration: "not_injected",
    };
  }
  async diagnostics() {
    return this.id === "runner" ? { "health.ui_thread": { comparable: false, source: "fake", state: "measured", data: { wedged: false } } } : {};
  }
  async teardown() {
    this.calls.push(["teardown", this.unit?.index]);
    return { closed: ["fake"], survivors: [] };
  }
}

/**
 * A fake probe driver over the fake adapters' sessions. Like the real one it
 * allows one run per file: a second `watch` while one is open in the same run
 * directory throws, as the real `WatchProcess` would on an unset-aside
 * `probe.jsonl`. Every watch's label is kept in `labels` (by run directory).
 */
export function fakeProbe({ clock, current }) {
  const open = new Set();
  const labels = new Map();
  return {
    path: "fake-probe",
    labels,
    startWatch(o) {
      if (open.has(o.runDir)) throw new Error(`${o.runDir}/probe.jsonl already holds a run's records`);
      if (!o.label) throw new Error("a watch needs a label (its run file is set aside under it)");
      open.add(o.runDir);
      labels.set(o.runDir, [...(labels.get(o.runDir) ?? []), o.label]);
      const t0 = clock.now();
      return {
        ready: async () => true,
        stop: async () => {
          open.delete(o.runDir);
          const t1 = clock.now();
          const adapter = current();
          const slots = readSlots(o.runDir);
          const sessions = adapter
            .visible()
            .filter((k) => slots.has(k))
            .map((k) => ({
              session: k,
              gen: readGen(o.runDir, k),
              latencyMs: 20,
              rect: { x: 10 + 300 * (k % 3), y: 40 + 300 * Math.floor(k / 3), w: 160, h: 16 },
            }));
          return { records: watchSegment({ t0, durationMs: Math.max(1, Number((t1 - t0) / MS)), sessions }), exit: { code: 0 } };
        },
      };
    },
    inject: async () => ({ code: 0, stdout: "", stderr: "" }),
    look: async () => ({ ok: true, sessions: new Map(current().visible().map((k) => [k, { x: 0, y: 0, w: 160, h: 16 }])), frames: 5, detail: "fake look" }),
    activate: async () => ({ ok: true, detail: "fake" }),
    takeInject: () => ({ records: [], path: null }),
  };
}

function readGen(dir, k) {
  return readFileSync(join(dir, `gen-${k}.jsonl`), "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l));
}

/** The facts a doctor would gather on a healthy linux-xvfb box. */
export function healthyFacts({ inputSafe = false } = {}) {
  const verdict = (id, state = "OPERATIVE") => ({ id, state, reason: "fake", checked_at: "2026-10-12T00:00:00Z", depends_on: "" });
  return {
    environment: "linux-xvfb",
    envPlatform: "linux",
    envNotBuilt: null,
    platform: "linux",
    nodeVersion: "v22.20.0",
    xvfbPath: "/usr/bin/Xvfb",
    windowManagerPath: "/usr/bin/matchbox-window-manager",
    display: { started: true, detail: ":99" },
    probePath: "fake-probe",
    probeDoctor: {
      record: {
        type: "doctor",
        mechanisms: [
          verdict("session.interactive"),
          verdict("capture.wgc", "INOPERATIVE-ON-THIS-MACHINE"),
          verdict("capture.dda", "INOPERATIVE-ON-THIS-MACHINE"),
          verdict("capture.x11"),
          verdict("input.safe", inputSafe ? "OPERATIVE" : "INOPERATIVE-ON-THIS-MACHINE"),
          verdict("clock.aligned"),
          verdict("counters.gpu", "INOPERATIVE-ON-THIS-MACHINE"),
        ],
      },
      error: null,
    },
    setupMechanisms: {},
    drain: { state: "not_enrolled", detail: "fake: no machine.json" },
  };
}

/** Base run options for the fakes (short phases; the clock is virtual anyway). */
export function fakeRunOptions(workDir, over = {}) {
  mkdirSync(workDir, { recursive: true });
  return {
    env: "linux-xvfb",
    setups: ["runner", "tmux"],
    ns: [1, 5],
    arms: ["focus", "tiled"],
    runs: 1,
    profile: "claude-tui",
    seed: 1,
    warmupMs: 1000,
    soakMs: 40000,
    inputKeys: 0,
    inputIntervalMs: 100,
    switchSamples: 3,
    switchSettleMs: 500,
    spawnSamples: 2,
    spawnTimeoutMs: 2000,
    smokeSetups: ["tmux"],
    workDir,
    runnerMode: "subject",
    runnerBuilds: [{ role: "single", commit: "a".repeat(40), exe: "fake", source: "artifact" }],
    runnerVariants: ["default"],
    runnerEnv: {},
    repoRoot: process.cwd(),
    command: ["node", "scripts/termhost-bench/cli.mjs", "run"],
    ...over,
  };
}
