/**
 * The `tmux-wsl` setup — the WSL variant of `adapters/tmux.mjs` (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 5): a private
 * tmux server (`-L termhost-<run>`, private config) inside a WSL 2 distro, shown
 * in the run's PORTABLE Windows Terminal (`windows-terminal.mjs`) as one tab
 * running `wsl.exe -d <distro> --exec tmux -L termhost-<run> attach-session -t bench`.
 *
 * Everything tmux does is the Linux adapter's — the same arrangements, the same
 * `select-window` switch, `new-window` / `split-window` spawns and `kill-pane`
 * close — with each tmux command run through `wsl.exe -d <distro> --exec`, so
 * no shell inside the distro reinterprets an argument. What differs:
 *
 *   sessions  the distro's `bash -i -c '<D13 line>; exec bash -i'`; the D13 argv's
 *             node is the distro's and its Windows paths are translated with
 *             `wslpath -a` — the flags and their order are unchanged;
 *   clock     the generators run on the Linux VM's CLOCK_MONOTONIC, which is not
 *             the QPC clock the probe reads. The launch measures the offset with
 *             round trips through the distro and hands it to every pane in the
 *             private server's environment (TERMHOST_BENCH_CLOCK_OFFSET_NS); a
 *             round trip too slow to bound it refuses the unit as clock_unaligned;
 *   roots     the portable Windows Terminal (the probe samples it on Windows);
 *             the WSL-side tmux server and clients are sampled through /proc
 *             inside the distro (`wsl-sampler.mjs`) and joined into the host's
 *             cpu_pct and memory_mb; the shared WSL VM process is sampled from
 *             Windows and reported as wsl_vm_* (attributed to no setup);
 *   doctor    the distro (WSL 2), tmux and node inside it, plus the portable
 *             Windows Terminal's install facts.
 *
 * Teardown: the stop file → the generators exit (read back inside the distro) →
 * `kill-server` on the private socket (the attach client exits, so the tab
 * closes) → the samplers stop → `WM_CLOSE` to the window if it is still open and
 * the proven WindowsTerminal.exe must exit. Anything this run started inside the
 * distro that is still running is reported, never signalled by name.
 */

import { createHash } from "node:crypto";
import { closeSync, mkdirSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { listProcesses, terminatePid } from "../proc.mjs";
import { ptyHostCensus, ptyHostImpl } from "../ptyhost.mjs";
import { assertInside } from "../rundir.mjs";
import {
  VM_PROCESS_QUERY,
  VM_SAMPLER_SCRIPT,
  WSL_SAMPLER_SCRIPT,
  chooseDistro,
  gatherWslFacts,
  mapWslFacts,
  measureClockOffset,
  pickVmProcess,
  readSamplerFile,
  toWslPath,
  wslExePath,
  wslGeneratorArgv,
  wslInvocation,
} from "../wsl.mjs";
import { AdapterRefusal } from "./index.mjs";
import { TmuxAdapter, cleanEnv, tmuxConf } from "./tmux.mjs";
import { windowsPowerShellPath } from "./vscode.mjs";
import { PortableWindowsTerminal, assertWtSafe, newSessionId, windowName } from "./windows-terminal.mjs";
import { BENCH_PROFILE, wtRenderBackend } from "./windows-terminal-install.mjs";

/** The largest clock round trip that still bounds the offset usefully (error <= half of it). */
export const MAX_CLOCK_RTT_NS = 20_000_000n;

/** The variable the generators read the offset from (`generator/ai-session.mjs` CLOCK_OFFSET_ENV). */
export const CLOCK_OFFSET_ENV = "TERMHOST_BENCH_CLOCK_OFFSET_NS";

/** The flag every generator carries with its run id. */
const GENERATOR_FLAG = "--termhost-bench-generator";

/**
 * A read-only scan run by the distro's node: the PIDs whose argv carries
 * `argv[1] argv[2]` as adjacent arguments.
 */
export const WSL_FLAG_SCAN =
  "const [f,v]=process.argv.slice(1);const fs=require('fs');for(const d of fs.readdirSync('/proc')){if(!/^\\d+$/.test(d))continue;let a;try{a=fs.readFileSync('/proc/'+d+'/cmdline','utf8').split('\\0')}catch{continue}const i=a.indexOf(f);if(i>=0&&a[i+1]===v)console.log(d)}";

/** The pane command Windows Terminal runs: the tmux client, attached to the private server. */
export function attachCommand({ wslExe, distro, tmuxPath = "tmux", socket }) {
  return assertWtSafe([wslExe, "-d", distro, "--exec", tmuxPath, "-L", socket, "attach-session", "-t", "bench"]);
}

/** The WSL sampler's argv inside the distro. */
export function samplerArgv({ node = "node", script, serverPid, socket, runId, out, stopFile, clkTck, clockOffsetNs, intervalMs = 500 }) {
  return [node, script, "--server-pid", String(serverPid), "--socket", socket, "--run-id", runId, "--out", out, "--stop-file", stopFile, "--clk-tck", String(clkTck), "--clock-offset-ns", String(clockOffsetNs), "--interval-ms", String(intervalMs)];
}

export class TmuxWslAdapter extends TmuxAdapter {
  /**
   * @param {object} [o] TmuxAdapter's options, plus `distro` (default: WSL's
   *   default distro), `wslExe`, `wt` / PortableWindowsTerminal options, and
   *   test seams: `wslFacts`, `clockOffset`, `spawnFn`, `queryVm`, `translate`.
   */
  constructor(o = {}) {
    super({ ...o, shell: o.shell ?? "/bin/bash", platform: o.platform ?? process.platform, list: o.list ?? (() => listProcesses({ platform: o.platform ?? process.platform })) });
    this.id = "tmux-wsl";
    this.distroWanted = o.distro ?? null;
    this.distro = null;
    this.wslExe = o.wslExe ?? wslExePath(o.env ?? process.env);
    this.wt = o.wt ?? new PortableWindowsTerminal(o);
    this.wslFactsFn = o.wslFacts ?? (() => gatherWslFacts({ run: this.run, wslExe: this.wslExe, wanted: this.distroWanted }));
    this.clockOffsetFn = o.clockOffset ?? ((distro) => measureClockOffset({ distro, wslExe: this.wslExe }));
    this.queryVm = o.queryVm ?? (() => this.defaultQueryVm());
    this.translateFn = o.translate ?? null;
    this.sleep = o.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
    this.generatorWaitMs = o.generatorWaitMs ?? 15000;
    this.facts = null;
    this.translated = new Map();
    this.clock = null;
    this.samplers = [];
    this.notes = [];
  }

  /** A tmux command inside the distro, never through a shell there. */
  tmuxRaw(args) {
    const [cmd, argv] = wslInvocation(this.distro, [this.tmuxPath, ...args], { wslExe: this.wslExe });
    return this.run(cmd, argv, { encoding: "utf8", env: this.env ?? cleanEnv(this.baseEnv), timeout: 30000, windowsHide: true });
  }

  /** A Windows path as the distro sees it: `wslpath -a`, else the default automount. */
  translate(winPath) {
    if (this.translated.has(winPath)) return this.translated.get(winPath);
    let out = null;
    if (this.translateFn) out = this.translateFn(winPath);
    else {
      const [cmd, argv] = wslInvocation(this.distro, ["wslpath", "-a", winPath], { wslExe: this.wslExe });
      const r = this.run(cmd, argv, { encoding: "utf8", timeout: 30000, windowsHide: true });
      out = r.error || r.status !== 0 ? null : String(r.stdout ?? "").trim() || null;
      if (!out) this.notes.push(`wslpath -a failed for ${winPath}; using the default automount`);
    }
    const p = out ?? toWslPath(winPath);
    this.translated.set(winPath, p);
    return p;
  }

  confArg() {
    return this.translate(this.confPath);
  }

  async check() {
    if (this.platform !== "win32") {
      const reason = `the tmux-wsl setup runs on Windows (on Linux it is the tmux setup); this is ${this.platform}`;
      return { mechanisms: { "setup.wsl": { state: "INOPERATIVE-ON-THIS-MACHINE", reason } } };
    }
    let facts;
    try {
      facts = this.wslFactsFn();
    } catch (e) {
      facts = { list: null, listError: e.message };
    }
    this.facts = facts;
    const d = facts.list ? chooseDistro(facts.list, this.distroWanted) : null;
    this.distro = d?.name ?? null;
    return { mechanisms: { "setup.windows_terminal": await this.wt.installFact(), ...mapWslFacts({ ...facts, wanted: this.distroWanted }) } };
  }

  defaultQueryVm() {
    const r = this.run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", VM_PROCESS_QUERY], { encoding: "utf8", timeout: 60000, windowsHide: true });
    if (r.error || r.status !== 0) throw new Error(`the WSL VM process query failed (${r.error?.message ?? `exit ${r.status}`})`);
    return pickVmProcess(r.stdout);
  }

  /** @param {{runDir: string, unitDir: string, genDir: string, genRunId: string, env?: object, signal?: AbortSignal}} ctx */
  async launch(ctx) {
    this.ctx = ctx;
    const facts = (await this.check()).mechanisms;
    for (const id of ["setup.windows_terminal", "setup.wsl", "setup.wsl_tmux", "setup.wsl_node"]) {
      if (facts[id]?.state !== "OPERATIVE") throw new AdapterRefusal("harness_error", `${id}: ${facts[id]?.reason ?? "no verdict"}`);
    }
    this.versions.tmux = this.facts.tmux;
    let clock;
    try {
      clock = await this.clockOffsetFn(this.distro);
    } catch (e) {
      throw new AdapterRefusal("clock_unaligned", `the WSL clock's offset from the probe's could not be measured: ${e.message}`);
    }
    if (clock.rttNs > MAX_CLOCK_RTT_NS) {
      throw new AdapterRefusal("clock_unaligned", `the fastest clock round trip through ${this.distro} took ${Number(clock.rttNs) / 1e6} ms, more than ${Number(MAX_CLOCK_RTT_NS) / 1e6} ms: the offset is not bounded well enough to time markers`);
    }
    // A WSL reading plus this is the host's time.
    this.clock = { addNs: -clock.offsetNs, rttNs: clock.rttNs, n: clock.n };
    await this.wt.prepare({ runDir: ctx.runDir, shellPath: windowsPowerShellPath(this.baseEnv), signal: ctx.signal ?? null });
    this.socket = `termhost-${ctx.genRunId}`;
    this.title = windowName(ctx.genRunId);
    this.unitDir = ctx.unitDir;
    this.confPath = assertInside(ctx.runDir, join(ctx.unitDir, "tmux.conf"));
    writeFileSync(this.confPath, tmuxConf({ version: String(this.facts.tmux ?? "").replace(/^tmux\s*/, ""), env: { [CLOCK_OFFSET_ENV]: String(this.clock.addNs) } }));
    this.env = cleanEnv(this.baseEnv);
    // The grid of panes fills the maximised window; tmux follows the client's size.
    this.cols = 200;
    this.rows = 50;
    return { ok: true };
  }

  async spawnSessions(n, argv) {
    const wslArgv = wslGeneratorArgv(argv, { node: "node", translate: (p) => this.translate(p) });
    return super.spawnSessions(n, wslArgv);
  }

  /** The portable Windows Terminal, one tab, attached to the private server; then the samplers. */
  async startHost() {
    const sid = this.wt.sessionId.supported === true ? newSessionId() : null;
    const cmd = attachCommand({ wslExe: this.wslExe, distro: this.distro, tmuxPath: this.tmuxPath, socket: this.socket });
    await this.wt.open(this.title, [["new-tab", "-p", BENCH_PROFILE, "--title", "tmux", ...(sid ? ["--sessionId", sid.arg] : []), ...cmd]]);
    const client = await this.waitForClient(30000);
    if (!client) throw new Error(`the tmux client in Windows Terminal never attached to the private server -L ${this.socket}`);
    this.startSamplers();
  }

  /** Start the WSL-side /proc sampler and the Windows-side VM sampler (each to a file in the unit). */
  startSamplers() {
    const dir = join(this.unitDir, "wsl");
    mkdirSync(dir, { recursive: true });
    this.wslOut = join(dir, "wsl-samples.jsonl");
    this.vmOut = join(dir, "vm-samples.jsonl");
    this.samplerStop = join(dir, "stop");
    const clkTck = Number(this.facts.clkTck) > 0 ? Number(this.facts.clkTck) : 100;
    const argv = samplerArgv({
      script: this.translate(WSL_SAMPLER_SCRIPT),
      serverPid: this.serverPid,
      socket: this.socket,
      runId: this.ctx.genRunId,
      out: this.translate(this.wslOut),
      stopFile: this.translate(this.samplerStop),
      clkTck,
      clockOffsetNs: this.clock.addNs,
    });
    const [cmd, args] = wslInvocation(this.distro, argv, { wslExe: this.wslExe });
    this.spawnSampler("wsl", cmd, args, {});
    let vm = null;
    try {
      vm = this.queryVm();
    } catch (e) {
      this.notes.push(`the WSL VM process could not be found: ${e.message}`);
    }
    if (vm) {
      this.vm = vm;
      this.spawnSampler("vm", "powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", VM_SAMPLER_SCRIPT], { TB_OUT: this.vmOut, TB_STOP: this.samplerStop, TB_VM_PID: String(vm.pid), TB_INTERVAL_MS: "500" });
    } else {
      this.notes.push("no vmmemWSL / vmmem process: the WSL VM is unsampled");
    }
  }

  spawnSampler(name, cmd, args, extraEnv) {
    const err = openSync(join(this.unitDir, "wsl", `${name}-sampler-stderr.log`), "a");
    let child;
    try {
      child = this.spawnFn(cmd, args, { stdio: ["pipe", "ignore", err], windowsHide: true, env: { ...this.env, ...extraEnv } });
    } finally {
      closeSync(err);
    }
    const rec = { name, child, pid: child.pid, startKey: null, exited: false };
    child.on?.("exit", () => {
      rec.exited = true;
    });
    try {
      rec.startKey = this.list().get(child.pid)?.startKey ?? null;
    } catch {
      // stays unprovable: teardown then only waits for it
    }
    this.samplers.push(rec);
  }

  processRoots() {
    return Number.isInteger(this.wt.pid) && this.wt.pid > 0 ? [this.wt.pid] : [];
  }

  windowTarget() {
    if (Number.isInteger(this.wt.hwnd)) return { hwnd: this.wt.hwnd };
    return Number.isInteger(this.wt.pid) ? { pid: this.wt.pid } : null;
  }

  /** The WSL-side and VM samples, for the run loop's KPI computation (wsl.mjs). */
  wslSamples() {
    return {
      wslRecords: this.wslOut ? readSamplerFile(this.wslOut) : [],
      vmRecords: this.vmOut ? readSamplerFile(this.vmOut) : [],
      clockAddNs: this.clock ? String(this.clock.addNs) : null,
      clockRttNs: this.clock ? String(this.clock.rttNs) : null,
    };
  }

  describe() {
    let digest = { state: "unknown", reason: "harness_error", detail: "tmux.conf or the Windows Terminal settings were not written" };
    try {
      const h = createHash("sha256").update(readFileSync(this.confPath)).update(readFileSync(this.wt.settingsPath));
      digest = `sha256:${h.digest("hex")}`;
    } catch {
      // keep the absence
    }
    let census = null;
    try {
      census = Number.isInteger(this.wt.pid) ? ptyHostCensus(this.list(), [this.wt.pid]) : null;
    } catch {
      // recorded as an absence by ptyHostImpl
    }
    const rtt = this.clock ? `; clock offset ${this.clock.addNs} ns, round trip ${Number(this.clock.rttNs) / 1e6} ms` : "";
    return {
      id: "tmux-wsl",
      variant: this.variant,
      app_version: `${this.facts?.tmux ?? "tmux (version unknown)"} in WSL ${this.distro ?? "?"} (node ${this.facts?.node ?? "?"}) in ${this.wt.appVersion(rtt)}`,
      build: { state: "not_applicable", reason: "not_applicable_to_setup" },
      settings_digest: digest,
      // The sessions' own ptys are Linux ptys inside tmux; what carries tmux's
      // output to the window is the portable Windows Terminal's ConPTY host.
      pty_host_impl: ptyHostImpl({ platform: this.platform, census, what: "the portable Windows Terminal hosting tmux" }),
      render_backend: this.wt.settings ? wtRenderBackend(this.wt.settings) : { state: "unknown", reason: "harness_error", detail: "the Windows Terminal settings were not written" },
      pty_holder: { state: "not_applicable", reason: "not_applicable_to_setup" },
      launch_mode: "private_server",
      session_shell: "bash -i",
      shell_integration: "not_injected",
    };
  }

  /** PIDs inside the distro whose argv carries `flag value` (read-only), or null when unreadable. */
  wslPidsCarrying(flag, value) {
    if (!this.distro) return null;
    // `--` ends node's own options, so a flag like `-L` reaches the script.
    const [cmd, argv] = wslInvocation(this.distro, ["node", "-e", WSL_FLAG_SCAN, "--", flag, value], { wslExe: this.wslExe });
    const r = this.run(cmd, argv, { encoding: "utf8", timeout: 30000, windowsHide: true });
    if (r.error || r.status !== 0) return null;
    return String(r.stdout ?? "")
      .split(/\r?\n/)
      .filter((l) => /^\d+$/.test(l.trim()))
      .map(Number);
  }

  emergencyStop() {
    if (this.socket && this.distro) this.tmux(["kill-server"], { allowFail: true });
    return this.wt.emergencyStop();
  }

  async teardown() {
    const closed = [];
    const survivors = [];
    // 1. The generators leave on the stop file (read back inside the distro).
    if (this.ctx?.genRunId && this.distro) {
      const deadline = Date.now() + this.generatorWaitMs;
      for (;;) {
        const live = this.wslPidsCarrying(GENERATOR_FLAG, this.ctx.genRunId);
        if (live === null || live.length === 0 || Date.now() >= deadline) break;
        await this.sleep(1000);
      }
    }
    // 2. The private server, by its socket only.
    if (this.socket && this.distro) {
      const res = this.tmux(["kill-server"], { allowFail: true });
      closed.push(`tmux server -L ${this.socket} in ${this.distro} (kill-server exit ${res.status})`);
    }
    // 3. The samplers.
    if (this.samplerStop) {
      try {
        writeFileSync(this.samplerStop, "");
      } catch {
        // the unit dir exists by construction
      }
    }
    for (const s of this.samplers) {
      try {
        s.child.stdin?.end();
      } catch {
        // already closed
      }
    }
    const deadline = Date.now() + 10000;
    while (this.samplers.some((s) => !s.exited) && Date.now() < deadline) await this.sleep(250);
    for (const s of this.samplers.filter((x) => !x.exited)) {
      if (!s.startKey) {
        survivors.push({ pid: s.pid, what: `the ${s.name} sampler (pid ${s.pid})`, detail: "did not stop, and its start key was never read, so it is not signalled" });
        continue;
      }
      try {
        const table = this.list();
        terminatePid(s.pid, { table, expect: (p) => p.startKey === s.startKey, ...(this.kill ? { kill: this.kill } : {}) });
      } catch (e) {
        this.notes.push(`teardown: the ${s.name} sampler: ${e.message}`);
      }
    }
    // 4. The window: WM_CLOSE if it is still open, and the proven WindowsTerminal.exe must exit.
    const w = await this.wt.close();
    closed.push(...w.closed);
    survivors.push(...w.survivors);
    // 5. Read back inside the distro: nothing this run started may still run there.
    if (this.distro) {
      const gens = this.ctx?.genRunId ? this.wslPidsCarrying(GENERATOR_FLAG, this.ctx.genRunId) : [];
      const servers = this.socket ? this.wslPidsCarrying("-L", this.socket) : [];
      if (gens === null || servers === null) this.notes.push(`teardown: the read-back inside ${this.distro} failed`);
      for (const pid of gens ?? []) survivors.push({ pid, what: `generator pid ${pid} in ${this.distro}`, detail: "still running after kill-server" });
      for (const pid of servers ?? []) survivors.push({ pid, what: `tmux -L ${this.socket} pid ${pid} in ${this.distro}`, detail: "still running after kill-server" });
    }
    return { closed, survivors, incidents: [], notes: [...this.notes, ...this.wt.notes] };
  }
}
