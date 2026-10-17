/**
 * The `tmux` setup adapter (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * Phase 3): a PRIVATE tmux server (`-L termhost-<run>`) with a PRIVATE config
 * (`-f <unit-dir>/tmux.conf`), hosted in an `xterm` window on Linux (version
 * recorded). It never touches an operator's own tmux server: every command names
 * the private socket, and teardown is `kill-server` on that socket only.
 *
 *   focus     one window per session; the shown window holds one pane (V = 1)
 *   tiled     up to nine panes per window under `select-layout tiled`, the first
 *             window shown (V = min(N, 9)); sessions beyond nine wait in further windows
 *   show(k)   `select-window` on session k's window
 *   spawnOne  `new-window` (focus — it becomes current) or `split-window` +
 *             `select-layout tiled` (tiled): tmux's own way to show a new session
 *   closeOne  `kill-pane`
 *   roots     the server PID (`display-message -p '#{pid}'`) and the xterm PID
 *
 * Each session is the platform's default interactive shell with the generator as
 * its child (D13): tmux runs `<shell> -i -c '<D13 command line>; exec <shell> -i'`
 * as the pane's process, given as an argv after `--`, so the shell forks the
 * generator and stays when it exits, as an operator's shell would.
 */

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";

import { carriesFlag, listProcesses, processAlive, terminatePid } from "../proc.mjs";
import { assertInside, commandLine, posixQuote } from "../rundir.mjs";
import { AdapterRefusal } from "./index.mjs";

/** Most panes the tiled arm puts in one window (D4: V ~ min(N, 9)). */
export const TILED_MAX_PANES = 9;

/**
 * The private config. Generous history, no status line, 256-colour and RGB.
 * `env` becomes the server's global environment (`set-environment -g`), which
 * every pane inherits — how the tmux-wsl variant hands its sessions the clock
 * offset without changing their command line (D13).
 */
export function tmuxConf({ version = "3.2", env = {} } = {}) {
  const [major, minor] = String(version).match(/(\d+)\.(\d+)/)?.slice(1).map(Number) ?? [3, 2];
  const lines = [
    "# termhost-bench private tmux config (written per unit; never the operator's)",
    "set -g status off",
    "set -g history-limit 50000",
    'set -g default-terminal "screen-256color"',
    "set -g escape-time 0",
    "set -g mouse off",
    "set -g focus-events off",
    "set -g bell-action none",
    "set -g visual-bell off",
    "set -g monitor-activity off",
    "set -g visual-activity off",
    "set -g pane-border-status off",
    "set -g remain-on-exit off",
    "set -g automatic-rename off",
    "set -g allow-rename off",
    "set -g aggressive-resize on",
    'set -ga terminal-overrides ",xterm*:Tc"',
  ];
  if (major > 3 || (major === 3 && minor >= 2)) {
    lines.push('set -as terminal-features ",xterm*:RGB:256"');
    lines.push("set -g window-size latest");
  }
  for (const [k, v] of Object.entries(env)) {
    if (!/^[A-Z_][A-Z0-9_]*$/.test(k) || !/^[A-Za-z0-9_.:/-]*$/.test(String(v))) throw new RangeError(`unsafe tmux environment entry ${k}=${v}`);
    lines.push(`set-environment -g ${k} ${v}`);
  }
  return `${lines.join("\n")}\n`;
}

/** xterm's argv: a full-screen window attached to the private server. */
export function xtermArgv({ xtermPath = "xterm", tmuxPath = "tmux", socket, title, cols, rows }) {
  return [
    xtermPath,
    "-T",
    title,
    "-n",
    title,
    "-geometry",
    `${cols}x${rows}+0+0`,
    "-fa",
    "Monospace",
    "-fs",
    "10",
    "+sb",
    "-bg",
    "black",
    "-fg",
    "white",
    "-tn",
    "xterm-256color",
    "-xrm",
    "XTerm*internalBorder: 0",
    "-xrm",
    "XTerm*allowTitleOps: false",
    "-xrm",
    "XTerm*cursorBlink: false",
    "-e",
    tmuxPath,
    "-L",
    socket,
    "attach-session",
    "-t",
    "bench",
  ];
}

/** The pane process: `<shell> -i -c '<command line>; exec <shell> -i'`. */
export function sessionShellArgv(shell, argv) {
  return [shell, "-i", "-c", `${commandLine(argv, "posix")}; exec ${posixQuote(shell)} -i`];
}

/** The environment tmux and xterm run with: no inherited tmux client state. */
export function cleanEnv(env, extra = {}) {
  const out = { ...env, ...extra };
  for (const k of ["TMUX", "TMUX_PANE", "CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION"]) delete out[k];
  return out;
}

const hrtime = () => process.hrtime.bigint();

export class TmuxAdapter {
  static hostIdKey = "TMUX_PANE";

  /**
   * @param {{variant?: string, tmuxPath?: string, xtermPath?: string, shell?: string,
   *   log?: (s: string) => void, run?: typeof spawnSync, spawnFn?: typeof spawn,
   *   platform?: string, env?: object}} [o]
   */
  constructor({
    variant = "default",
    tmuxPath = "tmux",
    xtermPath = "xterm",
    shell = process.env.SHELL || "/bin/bash",
    log = () => {},
    run = spawnSync,
    spawnFn = spawn,
    platform = process.platform,
    env = process.env,
    list = () => listProcesses(),
    alive = processAlive,
    kill,
  } = {}) {
    this.id = "tmux";
    this.list = list;
    this.alive = alive;
    this.kill = kill;
    this.hostIdKey = TmuxAdapter.hostIdKey;
    this.variant = variant;
    this.tmuxPath = tmuxPath;
    this.xtermPath = xtermPath;
    this.shell = shell;
    this.log = log;
    this.run = run;
    this.spawnFn = spawnFn;
    this.platform = platform;
    this.baseEnv = env;
    this.socket = null;
    this.confPath = null;
    this.env = null;
    this.argv = null;
    this.xterm = null;
    this.xtermExit = null;
    this.serverPid = null;
    this.windowId = null;
    this.arm = null;
    this.slots = new Map();
    this.spawned = [];
    this.versions = { tmux: null, xterm: null };
  }

  /**
   * Run tmux with `args` as given (the `-L` socket included). The tmux-wsl
   * variant runs it inside its WSL distro instead.
   */
  tmuxRaw(args) {
    return this.run(this.tmuxPath, args, { encoding: "utf8", env: this.env ?? cleanEnv(this.baseEnv), timeout: 20000 });
  }

  /** The private config's path as tmux sees it. */
  confArg() {
    return this.confPath;
  }

  /** A tmux command against the private server. */
  tmux(args, { allowFail = false } = {}) {
    const res = this.tmuxRaw(["-L", this.socket, ...args]);
    if (!allowFail && (res.error || res.status !== 0)) {
      throw new Error(
        `tmux ${args.join(" ")} failed (${res.error?.message ?? `exit ${res.status}`}): ${String(res.stderr ?? "").trim().slice(0, 300)}`,
      );
    }
    return res;
  }

  versionOf(path, args) {
    const res = this.run(path, args, { encoding: "utf8", timeout: 10000 });
    if (res.error || res.status !== 0) return null;
    return `${res.stdout ?? ""}${res.stderr ?? ""}`.trim().split("\n")[0] || null;
  }

  check() {
    const mechanisms = {};
    if (this.platform !== "linux") {
      const reason = `the tmux setup runs on Linux (on Windows it is the tmux-wsl variant, Phase 5); this is ${this.platform}`;
      mechanisms["setup.tmux"] = { state: "INOPERATIVE-ON-THIS-MACHINE", reason };
      mechanisms["setup.xterm"] = { state: "INOPERATIVE-ON-THIS-MACHINE", reason };
      return { mechanisms };
    }
    this.versions.tmux = this.versionOf(this.tmuxPath, ["-V"]);
    this.versions.xterm = this.versionOf(this.xtermPath, ["-version"]);
    mechanisms["setup.tmux"] = this.versions.tmux
      ? { state: "OPERATIVE", reason: this.versions.tmux }
      : { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `${this.tmuxPath} -V did not answer (apt-get install tmux)` };
    mechanisms["setup.xterm"] = this.versions.xterm
      ? { state: "OPERATIVE", reason: this.versions.xterm }
      : { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `${this.xtermPath} -version did not answer (apt-get install xterm)` };
    return { mechanisms };
  }

  /**
   * @param {{unitDir: string, runDir: string, genRunId: string, env: {env: object, screen?: object}}} ctx
   */
  async launch(ctx) {
    const facts = this.check().mechanisms;
    for (const [id, m] of Object.entries(facts)) {
      if (m.state !== "OPERATIVE") throw new AdapterRefusal("harness_error", `${id}: ${m.reason}`);
    }
    this.socket = `termhost-${ctx.genRunId}`;
    this.title = `termhost-${ctx.genRunId}`;
    this.unitDir = ctx.unitDir;
    this.confPath = assertInside(ctx.runDir, join(ctx.unitDir, "tmux.conf"));
    writeFileSync(this.confPath, tmuxConf({ version: this.versions.tmux?.replace(/^tmux\s*/, "") }));
    this.env = cleanEnv(this.baseEnv, ctx.env?.env ?? {});
    const screen = ctx.env?.screen ?? { width: 1920, height: 1080 };
    // Monospace 10 pt is about 8 x 17 px; a window manager maximises it regardless.
    this.cols = Math.max(80, Math.floor(screen.width / 8) - 4);
    this.rows = Math.max(24, Math.floor(screen.height / 17) - 1);
    return { ok: true };
  }

  async spawnSessions(n, argv) {
    this.argv = argv;
    const sessions = [];
    for (let i = 0; i < n; i += 1) {
      const shellArgv = sessionShellArgv(this.shell, argv);
      const tCmdNs = hrtime();
      const res =
        i === 0
          ? this.tmuxRaw([
              "-L",
              this.socket,
              "-f",
              this.confArg(),
              "new-session",
              "-d",
              "-s",
              "bench",
              "-x",
              String(this.cols),
              "-y",
              String(this.rows),
              "-n",
              "s0",
              "-P",
              "-F",
              "#{pane_id}",
              "--",
              ...shellArgv,
            ])
          : this.tmux(["new-window", "-d", "-t", "bench:", "-n", `s${i}`, "-P", "-F", "#{pane_id}", "--", ...shellArgv]);
      if (res.error || res.status !== 0) {
        throw new Error(`tmux could not start session ${i}: ${res.error?.message ?? String(res.stderr).trim()}`);
      }
      sessions.push({ hostId: String(res.stdout).trim(), tCmdNs });
    }
    this.serverPid = Number.parseInt(String(this.tmux(["display-message", "-p", "#{pid}"]).stdout).trim(), 10);
    await this.startHost();
    return { sessions, admission: null };
  }

  /** The terminal emulator the private server is shown in: xterm on Linux. */
  async startHost() {
    await this.startXterm();
  }

  /** Wait for a client to attach to the private server; its PID, or null. */
  async waitForClient(timeoutMs, exited = () => false) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline && !exited()) {
      const out = String(this.tmux(["list-clients", "-F", "#{client_pid}"], { allowFail: true }).stdout ?? "").trim();
      if (out) return Number.parseInt(out.split("\n")[0], 10);
      await new Promise((r) => setTimeout(r, 200));
    }
    return null;
  }

  async startXterm() {
    const argv = xtermArgv({
      xtermPath: this.xtermPath,
      tmuxPath: this.tmuxPath,
      socket: this.socket,
      title: this.title,
      cols: this.cols,
      rows: this.rows,
    });
    this.xterm = this.spawnFn(argv[0], argv.slice(1), { env: this.env, stdio: "ignore" });
    this.xterm.on?.("exit", (code, signal) => {
      this.xtermExit = { code, signal };
    });
    const clientPid = await this.waitForClient(15000, () => Boolean(this.xtermExit));
    if (!clientPid) {
      throw new Error(`xterm never attached to the private tmux server (${JSON.stringify(this.xtermExit ?? "no exit")})`);
    }
    this.windowId = readWindowId(clientPid);
  }

  /** {pane, window, index, cols, rows, active} for every pane of the session. */
  panes() {
    const out = String(
      this.tmux([
        "list-panes",
        "-s",
        "-t",
        "bench",
        "-F",
        "#{pane_id}\t#{window_id}\t#{window_index}\t#{pane_width}\t#{pane_height}\t#{window_active}",
      ]).stdout,
    ).trim();
    if (!out) return [];
    return out.split("\n").map((line) => {
      const [pane, window, index, cols, rows, active] = line.split("\t");
      return { pane, window, index: Number(index), cols: Number(cols), rows: Number(rows), active: active === "1" };
    });
  }

  /** Every pane in its own window (the focus layout, and the base the tiled one is built from). */
  separateAll() {
    const byWindow = new Map();
    for (const p of this.panes()) {
      if (!byWindow.has(p.window)) byWindow.set(p.window, []);
      byWindow.get(p.window).push(p);
    }
    for (const list of byWindow.values()) {
      for (const p of list.slice(1)) this.tmux(["break-pane", "-d", "-s", p.pane]);
    }
  }

  windowOf(pane) {
    return String(this.tmux(["display-message", "-p", "-t", pane, "#{window_id}"]).stdout).trim();
  }

  /**
   * @param {"focus" | "tiled"} arm
   * @param {{slots: Map<number, string>}} actx slot -> pane id
   */
  async arrange(arm, actx) {
    this.slots = new Map(actx.slots);
    this.arm = arm;
    const order = [...this.slots.keys()].sort((a, b) => a - b);
    if (order.length === 0) return { ok: false, vTarget: 1, failure: { reason: "harness_error", detail: "no session is mapped to a pane" } };
    this.separateAll();
    if (arm === "focus") {
      const focus = order[0];
      this.tmux(["select-window", "-t", this.windowOf(this.slots.get(focus))]);
      return { ok: true, vTarget: 1, focusSession: focus, switchable: order, paneSizes: this.visiblePaneSizes() };
    }
    for (let g = 0; g < order.length; g += TILED_MAX_PANES) {
      const group = order.slice(g, g + TILED_MAX_PANES).map((s) => this.slots.get(s));
      const anchor = group[0];
      for (const pane of group.slice(1)) {
        this.tmux(["join-pane", "-d", "-s", pane, "-t", anchor]);
        this.tmux(["select-layout", "-t", this.windowOf(anchor), "tiled"]);
      }
      this.tmux(["select-layout", "-t", this.windowOf(anchor), "tiled"]);
    }
    this.tmux(["select-window", "-t", this.windowOf(this.slots.get(order[0]))]);
    return {
      ok: true,
      vTarget: Math.min(order.length, TILED_MAX_PANES),
      focusSession: order[0],
      switchable: [],
      paneSizes: this.visiblePaneSizes(),
    };
  }

  visiblePaneSizes() {
    return this.panes()
      .filter((p) => p.active)
      .map((p) => ({ cols: p.cols, rows: p.rows }));
  }

  async show(k) {
    const pane = this.slots.get(k);
    if (!pane) return { ok: false, detail: `session ${k} has no pane` };
    const window = this.windowOf(pane);
    const tCmdNs = hrtime();
    this.tmux(["select-window", "-t", window]);
    return { ok: true, tCmdNs };
  }

  async spawnOne() {
    const shellArgv = sessionShellArgv(this.shell, this.argv);
    let res;
    const tCmdNs = hrtime();
    if (this.arm === "tiled") {
      const active = this.panes().find((p) => p.active);
      res = this.tmux(["split-window", "-t", active?.pane ?? "bench:", "-P", "-F", "#{pane_id}", "--", ...shellArgv]);
      this.tmux(["select-layout", "-t", active?.window ?? "bench:", "tiled"]);
    } else {
      res = this.tmux(["new-window", "-t", "bench:", "-P", "-F", "#{pane_id}", "--", ...shellArgv]);
    }
    const hostId = String(res.stdout).trim();
    this.spawned.push(hostId);
    return { ok: true, hostId, tCmdNs };
  }

  async closeOne(hostId) {
    const pane = hostId ?? this.spawned.pop();
    if (!pane) return { ok: false, detail: "nothing to close" };
    this.spawned = this.spawned.filter((p) => p !== pane);
    const window = this.arm === "tiled" ? this.windowOf(pane) : null;
    this.tmux(["kill-pane", "-t", pane], { allowFail: true });
    if (window) this.tmux(["select-layout", "-t", window, "tiled"], { allowFail: true });
    return { ok: true };
  }

  async focusObserved() {
    const active = this.panes().find((p) => p.active);
    if (!active) return { ok: false, failure: { reason: "harness_error", detail: "no active tmux pane" } };
    let session = null;
    for (const [slot, pane] of this.slots) if (pane === active.pane) session = slot;
    this.tmux(["select-pane", "-t", active.pane]);
    return session === null
      ? { ok: false, failure: { reason: "harness_error", detail: `the active pane ${active.pane} maps to no session` } }
      : { ok: true, session };
  }

  processRoots() {
    return [this.serverPid, this.xterm?.pid].filter((p) => Number.isInteger(p) && p > 0);
  }

  windowTarget() {
    if (Number.isInteger(this.windowId)) return { hwnd: this.windowId };
    if (Number.isInteger(this.xterm?.pid)) return { pid: this.xterm.pid };
    return { title: this.title };
  }

  describe() {
    let digest = { state: "unknown", reason: "harness_error", detail: "tmux.conf not written" };
    try {
      digest = `sha256:${createHash("sha256").update(readFileSync(this.confPath)).digest("hex")}`;
    } catch {
      // keep the absence
    }
    return {
      id: "tmux",
      variant: this.variant,
      app_version: `${this.versions.tmux ?? "tmux (version unknown)"} in ${this.versions.xterm ?? "xterm (version unknown)"}`,
      build: { state: "not_applicable", reason: "not_applicable_to_setup" },
      settings_digest: digest,
      pty_host_impl: "unix_pty",
      render_backend: "xterm_xft",
      pty_holder: { state: "not_applicable", reason: "not_applicable_to_setup" },
      launch_mode: "private_server",
      session_shell: `${basename(this.shell)} -i`,
      shell_integration: "not_injected",
    };
  }

  diagnostics() {
    return {};
  }

  async teardown() {
    const closed = [];
    const survivors = [];
    if (this.socket) {
      const res = this.tmux(["kill-server"], { allowFail: true });
      closed.push(`tmux server -L ${this.socket} (kill-server exit ${res.status})`);
    }
    if (this.xterm && !this.xtermExit) {
      await new Promise((r) => setTimeout(r, 1500));
      if (!this.xtermExit) {
        this.xterm.kill("SIGTERM");
        await new Promise((r) => setTimeout(r, 1500));
      }
      closed.push(`xterm pid ${this.xterm.pid}`);
    }
    if (Number.isInteger(this.serverPid) && this.alive(this.serverPid)) {
      // kill-server should have ended it; end it by PID only if it is still OUR server.
      const table = this.list();
      const r = terminatePid(this.serverPid, {
        signal: "SIGTERM",
        table,
        expect: (p) => carriesFlag(p, "-L", this.socket),
        ...(this.kill ? { kill: this.kill } : {}),
      });
      if (r.outcome === "signalled") await new Promise((res) => setTimeout(res, 1000));
      if (this.alive(this.serverPid) && carriesFlag(this.list().get(this.serverPid) ?? {}, "-L", this.socket)) {
        survivors.push({ pid: this.serverPid, what: `tmux server -L ${this.socket}` });
      }
    }
    return { closed, survivors, incidents: [] };
  }
}

/** The X window id xterm exports to its child as WINDOWID (Linux /proc), or null. */
export function readWindowId(childPid, { read = readFileSync } = {}) {
  try {
    const env = read(`/proc/${childPid}/environ`).toString("utf8").split("\0");
    const kv = env.find((e) => e.startsWith("WINDOWID="));
    const v = kv ? Number.parseInt(kv.slice("WINDOWID=".length), 10) : NaN;
    return Number.isInteger(v) && v > 0 ? v : null;
  } catch {
    return null;
  }
}
