/**
 * The `windows-terminal` setup adapter (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 5, D4, D5,
 * D13): Windows Terminal, measured in a PORTABLE copy of the pinned build that
 * this run unpacked — never the operator's own Windows Terminal, which since 1.18
 * runs every window in one process.
 *
 * Install and isolation (D5, `windows-terminal-install.mjs`): the pinned ZIP,
 * SHA-256 checked, cached outside any repository, unpacked into a new directory
 * of the run with a `.portable` file, so its `settings\` folder sits beside the
 * binary. Each unit writes a fresh private `settings.json` there (opacity 100, a
 * fixed font size, `closeOnExit: graceful`; renderer settings left at their
 * defaults and recorded).
 *
 * Launch (D13): every session is `powershell.exe -NoLogo -NoExit -Command <the
 * one generator command line>`, the runner's own session shell, given to
 * `wt.exe` as the pane's command line, each pane with a `--sessionId` the harness
 * chose (the pinned build's parser accepts it — checked from `TerminalApp.dll` —
 * so the session's `WT_SESSION` maps its generator slot exactly):
 *
 *   wt.exe -w termhost-<run> -M new-tab -p termhost-bench --title s0 --sessionId {g0} <cmd>
 *          ; split-pane -V --size 0.667 … ; split-pane -V --size 0.5 …
 *          ; focus-pane -t 0 ; split-pane -H --size 0.5 … ; …
 *
 * builds the tiled 3 x 3 grid (D4: V = min(N, 9)) in tab 0 and gives sessions
 * beyond nine one tab each. The arms then re-arrange the same sessions:
 *
 *   tiled     tab 0's grid, shown with `focus-tab -t 0`
 *   focus     one tab per session (`focus-pane -t <id> ; move-pane -t <new tab>`
 *             takes each pane out of the grid into a tab of its own), the first
 *             session's tab shown (V = 1)
 *   show(k)   `wt -w termhost-<run> focus-tab -t <k's tab>`
 *   spawnOne  `new-tab` (focus) or `split-pane` into tab 0 (tiled)
 *   closeOne  the session's shell ended by PID (proven ours); its profile closes
 *             the pane on exit
 *   roots     the WindowsTerminal.exe the launch proved new; the probe walks its
 *             OpenConsole.exe hosts (session shells and generators are workload)
 *
 * The window is driven only through Windows Terminal's own command line, which
 * hands each later `wt.exe` invocation to the run's instance (the single-instance
 * key of an unpackaged build is a hash of its path). There is no reply, so every
 * arrangement is read back through the probe's `look`, as VS Code's is.
 *
 * Teardown: the stop file (written by the run loop) → the generators exit 0 →
 * `WM_CLOSE` to the window this run opened, owner re-checked → the
 * `WindowsTerminal.exe` PID the launch proved new must exit. A process still
 * holding that PID and start key is ended by PID; nothing is ever selected by
 * image name — the operator's own `WindowsTerminal.exe` shares it.
 */

import { spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join, win32 } from "node:path";

import { WaitAborted } from "../../lib/runner-clients.mjs";
import { carriesFlag, descendantsOf, listProcesses, terminatePid } from "../proc.mjs";
import { ptyHostCensus, ptyHostImpl } from "../ptyhost.mjs";
import { readGeneratorLog, readSlots } from "../sessions.mjs";
import { commandLine } from "../rundir.mjs";
import { incident } from "../results.mjs";
import { AdapterRefusal } from "./index.mjs";
import { ArchiveIntegrityError, ensureArchive, findEnclosingRepo, sha256File } from "./vscode-install.mjs";
import { isDescendantOf, latestPaneSize, shellFacts, windowsPowerShellPath } from "./vscode.mjs";
import {
  BENCH_PROFILE,
  PORTABLE_MARKER,
  PORTABLE_SETTINGS_DIR,
  SPAWNED_PROFILE,
  WT_PIN,
  defaultWtCacheDir,
  ensureWtUnpacked,
  sessionIdSupport,
  wtBuildFor,
  wtRenderBackend,
  wtSettings,
} from "./windows-terminal-install.mjs";

/** Most visible panes in the tiled arm (D4). */
export const TILED_MAX_PANES = 9;

/** Sessions per `wt.exe` invocation beyond the grid (a Windows command line holds 32 767 chars). */
export const TABS_PER_INVOCATION = 8;

/** The top-level window class of a Windows Terminal window. */
export const WT_WINDOW_CLASS = "CASCADIA_HOSTING_WINDOW_CLASS";

/** The flag every generator's command line carries with its run id (rundir.mjs `generatorArgv`). */
export const GENERATOR_FLAG = "--termhost-bench-generator";

/** The variable each session's shell sees, naming its pane (`ConptyConnection.cpp`). */
export const HOST_ID_KEY = "WT_SESSION";

const hrtime = () => process.hrtime.bigint();
const sleepMs = (ms) => new Promise((r) => setTimeout(r, ms));
const absent = (reason, detail) => ({ state: "unknown", reason, detail });

/** `termhost-<run>`: the window this run drives (a window name may not be all digits). */
export function windowName(genRunId) {
  return `termhost-${genRunId}`;
}

/** A fresh session id: `{guid}` for `--sessionId`, the bare lowercase GUID `WT_SESSION` carries. */
export function newSessionId(uuid = randomUUID) {
  const g = String(uuid()).toLowerCase();
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(g)) throw new Error(`not a GUID: ${g}`);
  return { arg: `{${g}}`, env: g };
}

/**
 * One pane's process (D13): Windows PowerShell 5.1, `-NoLogo -NoExit -Command
 * <line>`. Windows Terminal splits its own command line at every `;` (an escaped
 * `\;` keeps its backslash) and re-quotes an argument only by wrapping it in `"`
 * when it holds a space (`AppCommandlineArgs.cpp`), so an argument carrying `;`
 * or `"` would reach the shell changed: refused instead.
 *
 * @returns {string[]}
 */
export function paneCommand(argv, { shellPath }) {
  const out = [shellPath, "-NoLogo", "-NoExit", "-Command", commandLine(argv, "powershell")];
  assertWtSafe(out);
  return out;
}

/** Throw unless every argument survives Windows Terminal's command-line parser unchanged. */
export function assertWtSafe(args) {
  for (const a of args) {
    const s = String(a);
    if (s.includes(";")) throw new AdapterRefusal("harness_error", `a Windows Terminal pane argument carries ";", which wt.exe splits commands at: ${s.slice(0, 120)}`);
    if (s.includes('"')) throw new AdapterRefusal("harness_error", `a Windows Terminal pane argument carries '"', which wt.exe does not escape: ${s.slice(0, 120)}`);
  }
  return args;
}

/**
 * The tiled grid for v panes (v <= 9): columns of near-equal height, at most
 * three columns, the first columns taking the extra pane.
 *
 * @returns {number[]} panes per column
 */
export function gridColumns(v) {
  if (!Number.isInteger(v) || v < 1 || v > TILED_MAX_PANES) throw new RangeError(`a grid holds 1..${TILED_MAX_PANES} panes, got ${v}`);
  const cols = Math.min(3, Math.ceil(Math.sqrt(v)));
  const base = Math.floor(v / cols);
  const extra = v % cols;
  return Array.from({ length: cols }, (_, c) => base + (c < extra ? 1 : 0));
}

const fraction = (x) => String(Math.round(x * 1000) / 1000);

/**
 * The subcommands that build tab 0's grid from `panes` (each `{title, sessionId, cmd}`)
 * and the pane id Windows Terminal assigns each (ids count up per tab in creation
 * order, `Tab.cpp`). Column heads first — each splits the previous head, the new
 * pane taking `(cols - c) / (cols - c + 1)` of it — then each column split
 * downward from its head the same way, so every pane ends at 1/cols x 1/rows.
 *
 * @returns {{commands: string[][], ids: number[]}} ids[i] is panes[i]'s pane id
 */
export function gridCommands(panes, { profile = BENCH_PROFILE } = {}) {
  const cols = gridColumns(panes.length);
  const ids = new Array(panes.length).fill(null);
  const commands = [];
  const term = (p) => ["-p", profile, "--title", p.title, ...(p.sessionId ? ["--sessionId", p.sessionId] : []), ...p.cmd];
  // Pane k of the grid, column-major: column c holds the next cols[c] panes.
  const columnPanes = [];
  let k = 0;
  for (const rows of cols) {
    columnPanes.push(panes.slice(k, k + rows).map((p, r) => ({ p, index: k + r })));
    k += rows;
  }
  let nextId = 0;
  const heads = [];
  columnPanes.forEach((col, c) => {
    const { p, index } = col[0];
    if (c === 0) commands.push(["new-tab", ...term(p)]);
    else commands.push(["split-pane", "-V", "--size", fraction((cols.length - c) / (cols.length - c + 1)), ...term(p)]);
    ids[index] = nextId;
    heads.push(nextId);
    nextId += 1;
  });
  columnPanes.forEach((col, c) => {
    if (col.length < 2) return;
    commands.push(["focus-pane", "-t", String(heads[c])]);
    col.slice(1).forEach(({ p, index }, j) => {
      const left = col.length - 1 - j;
      commands.push(["split-pane", "-H", "--size", fraction(left / (left + 1)), ...term(p)]);
      ids[index] = nextId;
      nextId += 1;
    });
  });
  return { commands, ids };
}

/** One `wt.exe` argv from subcommands: `-w <window> [global...] a ; b ; c`. */
export function wtArgv(window, commands, { global = [] } = {}) {
  const out = ["-w", window, ...global];
  commands.forEach((c, i) => {
    if (i > 0) out.push(";");
    out.push(...c);
  });
  return out;
}

/**
 * The model of the window's tabs and panes the adapter keeps, because Windows
 * Terminal's command line answers nothing: each tab `{panes: [{id, hostId}], nextId}`.
 * It follows the rules of the pinned source — pane ids count up per tab; a pane
 * moved to a tab index past the end becomes a new tab, whose root pane is id 0
 * (`TerminalPage::_MovePane`, `Tab.cpp`); a tab whose last pane leaves closes.
 */
export class WtLayout {
  constructor() {
    this.tabs = [];
  }

  /** The tabs the spawn built: tab 0's grid, then one tab per further session. */
  static afterSpawn(gridHostIds, gridIds, extraHostIds) {
    const l = new WtLayout();
    const panes = gridHostIds.map((hostId, i) => ({ id: gridIds[i], hostId }));
    l.tabs.push({ panes, nextId: panes.length });
    for (const hostId of extraHostIds) l.tabs.push({ panes: [{ id: 0, hostId }], nextId: 1 });
    return l;
  }

  tabOf(hostId) {
    return this.tabs.findIndex((t) => t.panes.some((p) => p.hostId === hostId));
  }

  paneOf(hostId) {
    const t = this.tabOf(hostId);
    return t < 0 ? null : { tab: t, id: this.tabs[t].panes.find((p) => p.hostId === hostId).id };
  }

  /** The layout an arm is in: "tiled" when tab 0 holds several panes, else "focus". */
  get shape() {
    return this.tabs.some((t) => t.panes.length > 1) ? "tiled" : "focus";
  }

  /**
   * Commands that leave every session in a tab of its own, and the model after.
   * `keep` stays where it is; every other pane of a multi-pane tab moves out.
   */
  toFocus() {
    const commands = [];
    for (let t = 0; t < this.tabs.length; t += 1) {
      const tab = this.tabs[t];
      while (tab.panes.length > 1) {
        const p = tab.panes[tab.panes.length - 1];
        commands.push(["focus-tab", "-t", String(t)], ["focus-pane", "-t", String(p.id)], ["move-pane", "-t", String(this.tabs.length)]);
        tab.panes.pop();
        this.tabs.push({ panes: [{ id: 0, hostId: p.hostId }], nextId: 1 });
      }
    }
    return commands;
  }

  /**
   * Commands that bring `hostIds` (at most nine) into one tab — the tab of the
   * first — moving each other one's pane there. The target tab's active pane is
   * split in Windows Terminal's automatic direction, so the sizes are uneven;
   * the caller says so.
   */
  toTiled(hostIds) {
    const commands = [];
    const anchor = hostIds[0];
    for (const hostId of hostIds.slice(1)) {
      const target = this.tabOf(anchor);
      const from = this.tabOf(hostId);
      if (from < 0 || from === target) continue;
      const src = this.tabs[from];
      const moving = src.panes.find((p) => p.hostId === hostId);
      commands.push(["focus-tab", "-t", String(from)], ["focus-pane", "-t", String(moving.id)], ["move-pane", "-t", String(target)]);
      const dst = this.tabs[target];
      dst.panes.push({ id: dst.nextId, hostId });
      dst.nextId += 1;
      src.panes = src.panes.filter((p) => p !== moving);
      if (src.panes.length === 0) this.tabs.splice(from, 1);
    }
    return commands;
  }

  /** A new tab holding `hostId` (spawnOne, focus arm). */
  addTab(hostId) {
    this.tabs.push({ panes: [{ id: 0, hostId }], nextId: 1 });
    return this.tabs.length - 1;
  }

  /** A pane split into tab `t` (spawnOne, tiled arm). */
  addPane(t, hostId) {
    const tab = this.tabs[t];
    tab.panes.push({ id: tab.nextId, hostId });
    tab.nextId += 1;
  }

  /** The pane closed (its process exited): a tab left empty closes. */
  remove(hostId) {
    const t = this.tabOf(hostId);
    if (t < 0) return;
    this.tabs[t].panes = this.tabs[t].panes.filter((p) => p.hostId !== hostId);
    if (this.tabs[t].panes.length === 0) this.tabs.splice(t, 1);
  }
}

// ---------------------------------------------------------------------------
// Read-only process and window queries, and the one window message
// ---------------------------------------------------------------------------

/**
 * Every `WindowsTerminal.exe` with its image path and start time (read-only —
 * selecting rows by image name to READ them is how the doctor tells the run's
 * process from the operator's; nothing is ever signalled by name).
 */
export const WT_PROCESS_QUERY =
  "$ErrorActionPreference='Stop'; " +
  "@(Get-CimInstance -ClassName Win32_Process -Filter \"Name='WindowsTerminal.exe'\" | ForEach-Object { " +
  "[pscustomobject]@{ p=[int]$_.ProcessId; pp=[int]$_.ParentProcessId; x=[string]$_.ExecutablePath; " +
  "t=$(if ($_.CreationDate) { [string]$_.CreationDate.ToFileTimeUtc() } else { '0' }) } }) | ConvertTo-Json -Compress -Depth 2";

/** @returns {{pid: number, ppid: number, exe: string, startKey: string}[]} */
export function parseWtProcesses(text) {
  const t = String(text ?? "").trim();
  if (!t) return [];
  const parsed = JSON.parse(t);
  const rows = Array.isArray(parsed) ? parsed : [parsed];
  return rows.filter((r) => Number.isInteger(r?.p)).map((r) => ({ pid: r.p, ppid: Number.isInteger(r.pp) ? r.pp : 0, exe: String(r.x ?? ""), startKey: String(r.t ?? "0") }));
}

const samePath = (a, b) => win32.normalize(String(a)).toLowerCase() === win32.normalize(String(b)).toLowerCase();

/**
 * D5's proof: the `WindowsTerminal.exe` hosting the run's window is a process
 * that did not exist before the launch and runs from the run's own copy. Several
 * candidates can appear (each later `wt.exe` starts one that hands off and
 * exits); the earliest-started one that is still alive is the instance.
 *
 * @param {{before: {pid: number, startKey: string}[], after: {pid: number, exe: string, startKey: string}[], exePath: string}} o
 * @returns {{ok: true, pid: number, startKey: string} | {ok: false, reason: string, detail: string}}
 */
export function proveNewWtProcess({ before, after, exePath }) {
  const old = new Set(before.map((p) => `${p.pid}:${p.startKey}`));
  const ours = after.filter((p) => samePath(p.exe, exePath));
  const fresh = ours.filter((p) => !old.has(`${p.pid}:${p.startKey}`)).sort((a, b) => (BigInt(a.startKey) < BigInt(b.startKey) ? -1 : 1));
  if (fresh.length > 0) return { ok: true, pid: fresh[0].pid, startKey: fresh[0].startKey };
  if (ours.length > 0) {
    return { ok: false, reason: "shared_process", detail: `the WindowsTerminal.exe running ${exePath} (pid ${ours[0].pid}) existed before this launch, so its process is not this run's alone (D5)` };
  }
  return { ok: false, reason: "harness_error", detail: `no WindowsTerminal.exe from ${exePath} appeared (${after.length} other WindowsTerminal.exe process(es) running)` };
}

/** The C# both window scripts compile (read-only): a PID's top-level windows, a window's owner and class. */
const WINDOW_TYPE =
  "Add-Type -TypeDefinition @'\n" +
  "using System; using System.Text; using System.Collections.Generic; using System.Runtime.InteropServices;\n" +
  "public static class TbWtWin {\n" +
  "  public delegate bool EnumProc(IntPtr h, IntPtr l);\n" +
  '  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);\n' +
  '  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);\n' +
  '  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);\n' +
  '  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);\n' +
  '  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);\n' +
  "  public static string Cls(IntPtr h) { var c = new StringBuilder(256); GetClassName(h, c, 256); return c.ToString(); }\n" +
  "  public static uint Owner(IntPtr h) { uint p; GetWindowThreadProcessId(h, out p); return p; }\n" +
  "  public static List<string> Of(uint want) {\n" +
  "    var o = new List<string>();\n" +
  "    EnumWindows((h, l) => { if (Owner(h) == want) { var t = new StringBuilder(512); GetWindowText(h, t, 512);\n" +
  '      o.Add(h.ToInt64() + "\\t" + (IsWindowVisible(h) ? 1 : 0) + "\\t" + Cls(h) + "\\t" + t.ToString().Replace("\\t", " ")); } return true; }, IntPtr.Zero);\n' +
  "    return o; }\n" +
  "}\n" +
  "'@\n";

/** Lists the top-level windows of `$env:TB_WT_PID` (read-only): `hwnd \t visible \t class \t title`. */
export const WINDOW_QUERY = `$ErrorActionPreference='Stop'\n${WINDOW_TYPE}[TbWtWin]::Of([uint32]$env:TB_WT_PID) | ForEach-Object { $_ }\n`;

/**
 * Posts WM_CLOSE to `$env:TB_WT_HWND` only if that window is still a Windows
 * Terminal window owned by `$env:TB_WT_PID`, and that PID is still the process
 * the launch proved (its creation time, as a FILETIME, equals
 * `$env:TB_WT_START` — the start key `proc.mjs` reads): prints `posted`, or
 * `mismatch`.
 */
export const WINDOW_CLOSE =
  `$ErrorActionPreference='Stop'\n${WINDOW_TYPE}` +
  "Add-Type -TypeDefinition @'\n" +
  "using System; using System.Runtime.InteropServices;\n" +
  'public static class TbWtPost { [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }\n' +
  "'@\n" +
  "$h = [IntPtr][int64]$env:TB_WT_HWND\n" +
  "$p = Get-CimInstance -ClassName Win32_Process -Filter \"ProcessId=$([uint32]$env:TB_WT_PID)\"\n" +
  "$same = $p -and $p.CreationDate -and ([string]$p.CreationDate.ToFileTimeUtc() -eq $env:TB_WT_START)\n" +
  `if ($same -and [TbWtWin]::Owner($h) -eq [uint32]$env:TB_WT_PID -and [TbWtWin]::Cls($h) -eq '${WT_WINDOW_CLASS}') { [void][TbWtPost]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero); 'posted' } else { 'mismatch' }\n`;

/** @returns {{hwnd: number, visible: boolean, cls: string, title: string}[]} */
export function parseWindows(text) {
  return String(text ?? "")
    .split(/\r?\n/)
    .map((l) => l.split("\t"))
    .filter((f) => f.length >= 3 && /^\d+$/.test(f[0]))
    .map(([hwnd, vis, cls, ...title]) => ({ hwnd: Number(hwnd), visible: vis === "1", cls, title: title.join("\t") }));
}

/** The run's window among a PID's windows: visible, of Windows Terminal's class. */
export function pickWindow(windows) {
  return windows.find((w) => w.visible && w.cls === WT_WINDOW_CLASS) ?? null;
}

/**
 * `-EncodedCommand`'s argument: the script as UTF-16LE, base64. Multi-line
 * scripts (the Add-Type here-strings) travel this way, so no command-line quoting
 * layer can change them.
 */
export function encodePowerShell(script) {
  return Buffer.from(String(script), "utf16le").toString("base64");
}

/** Windows PowerShell's argv for `script`, encoded. */
export function powershellArgv(script) {
  return ["-NoProfile", "-NonInteractive", "-EncodedCommand", encodePowerShell(script)];
}

const powershell = (run, script, env) =>
  run("powershell.exe", powershellArgv(script), { encoding: "utf8", windowsHide: true, timeout: 60000, env, maxBuffer: 16 * 1024 * 1024 });

// ---------------------------------------------------------------------------
// The portable instance: install, settings, open, drive, close
// ---------------------------------------------------------------------------

/**
 * One portable Windows Terminal window this run opened. Shared by the
 * `windows-terminal` adapter and the `tmux-wsl` variant, which hosts its tmux
 * client in it.
 */
export class PortableWindowsTerminal {
  /**
   * @param {{log?: (s: string) => void, platform?: string, arch?: string, env?: object,
   *   cacheDir?: string | null, fetch?: typeof fetch, run?: typeof spawnSync,
   *   list?: () => Map<number, object>, kill?: typeof process.kill, exists?: (p: string) => boolean,
   *   sleep?: (ms: number) => Promise<void>, hash?: (p: string) => Promise<string>,
   *   queryWt?: () => object[], queryWindows?: (pid: number) => object[], postClose?: (pid: number, hwnd: number, startKey: string) => string,
   *   launchTimeoutMs?: number, closeTimeoutMs?: number, settleMs?: number, pin?: typeof WT_PIN}} [o]
   */
  constructor(o = {}) {
    this.log = o.log ?? (() => {});
    this.platform = o.platform ?? process.platform;
    this.arch = o.arch ?? process.arch;
    this.baseEnv = o.env ?? process.env;
    this.cacheDir = o.cacheDir ?? defaultWtCacheDir({ platform: this.platform, env: this.baseEnv });
    this.fetch = o.fetch ?? globalThis.fetch;
    this.run = o.run ?? spawnSync;
    this.list = o.list ?? (() => listProcesses({ platform: this.platform }));
    this.kill = o.kill;
    this.exists = o.exists ?? existsSync;
    this.sleep = o.sleep ?? sleepMs;
    this.hashFn = o.hash ?? null;
    this.queryWt = o.queryWt ?? (() => this.defaultQueryWt());
    this.queryWindows = o.queryWindows ?? ((pid) => this.defaultQueryWindows(pid));
    this.postClose = o.postClose ?? ((pid, hwnd, startKey) => this.defaultPostClose(pid, hwnd, startKey));
    this.launchTimeoutMs = o.launchTimeoutMs ?? 60000;
    this.closeTimeoutMs = o.closeTimeoutMs ?? 20000;
    this.settleMs = o.settleMs ?? 400;
    this.pin = o.pin ?? WT_PIN;
    this.cacheVerified = null;
    this.appDir = null;
    this.window = null;
    this.pid = null;
    this.startKey = null;
    this.hwnd = null;
    this.settings = null;
    this.sessionId = { supported: null, detail: "not checked yet" };
    this.notes = [];
    this.abortSignal = null;
  }

  build() {
    return wtBuildFor(this.platform, this.arch, this.pin);
  }

  /** wt.exe and WindowsTerminal.exe of the unpacked copy. */
  get wtExe() {
    return join(this.appDir, "wt.exe");
  }

  get hostExe() {
    return join(this.appDir, "WindowsTerminal.exe");
  }

  get settingsPath() {
    return join(this.appDir, PORTABLE_SETTINGS_DIR, "settings.json");
  }

  /** The doctor's install fact, side-effect free (a cached archive is re-hashed, nothing downloaded). */
  async installFact() {
    if (this.platform !== "win32") return { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `Windows Terminal runs on Windows; this is ${this.platform}` };
    const build = this.build();
    if (!build) return { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `no pinned Windows Terminal build for ${this.platform}-${this.arch}` };
    const repo = findEnclosingRepo(this.cacheDir, { exists: this.exists });
    if (repo) return { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `the archive cache ${this.cacheDir} is inside the git checkout ${repo} (D5)` };
    const cached = join(this.cacheDir, this.pin.version, build.archive);
    if (!this.exists(cached)) {
      return { state: "OPERATIVE", reason: `pinned portable Windows Terminal ${this.pin.version} (sha256 ${build.sha256.slice(0, 16)}…, ${build.url}) not cached; the first launch downloads it into ${this.cacheDir} and verifies it` };
    }
    try {
      const st = statSync(cached);
      const actual = await (this.hashFn ?? sha256File)(cached);
      this.cacheVerified = { path: cached, size: st.size, mtimeMs: st.mtimeMs, sha256: actual };
      return actual === build.sha256
        ? { state: "OPERATIVE", reason: `pinned portable Windows Terminal ${this.pin.version} cached at ${cached}, SHA-256 verified` }
        : { state: "OPERATIVE", reason: `the cached ${cached} fails SHA-256 (${actual.slice(0, 16)}…); the launch discards it and downloads the pinned archive again` };
    } catch (e) {
      return { state: "UNKNOWN", reason: `cannot hash ${cached}: ${e.message}` };
    }
  }

  /**
   * The verified archive, unpacked into this run with its `.portable` marker,
   * and a fresh private `settings\settings.json` for this unit.
   *
   * @param {{runDir: string, shellPath: string}} o
   */
  async prepare({ runDir, shellPath, signal = null }) {
    this.abortSignal = signal;
    const build = this.build();
    if (!build) throw new AdapterRefusal("harness_error", `no pinned Windows Terminal build for ${this.platform}-${this.arch}`);
    let archive;
    try {
      archive = await ensureArchive({
        build,
        cacheDir: this.cacheDir,
        version: this.pin.version,
        fetch: this.fetch,
        log: this.log,
        exists: this.exists,
        verified: this.cacheVerified ?? null,
        signal,
        what: "Windows Terminal",
        ...(this.hashFn ? { hash: this.hashFn } : {}),
      });
    } catch (e) {
      if (e instanceof WaitAborted) throw e;
      throw new AdapterRefusal("harness_error", e instanceof ArchiveIntegrityError ? `refusing the Windows Terminal archive: ${e.message}` : e.message);
    }
    this.archiveSha256 = archive.sha256;
    this.appDir = await ensureWtUnpacked({ archive: archive.path, build, runDir, version: this.pin.version, run: this.run, platform: this.platform, env: this.baseEnv, log: this.log, exists: this.exists });
    if (!this.exists(join(this.appDir, PORTABLE_MARKER))) {
      throw new AdapterRefusal("harness_error", `${this.appDir} has no ${PORTABLE_MARKER}: Windows Terminal would write the operator's %LOCALAPPDATA% settings (D5)`);
    }
    this.sessionId = sessionIdSupport(this.appDir);
    // A fresh settings folder per unit: no state.json or layout carried over.
    const dir = join(this.appDir, PORTABLE_SETTINGS_DIR);
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    this.settings = wtSettings({ shellPath });
    writeFileSync(this.settingsPath, `${JSON.stringify(this.settings, null, 2)}\n`);
    return { appDir: this.appDir, sessionId: this.sessionId };
  }

  defaultQueryWt() {
    const res = powershell(this.run, WT_PROCESS_QUERY, this.baseEnv);
    if (res.error || res.status !== 0) throw new Error(`the WindowsTerminal.exe query failed (${res.error?.message ?? `exit ${res.status}`}): ${String(res.stderr ?? "").slice(0, 300)}`);
    return parseWtProcesses(res.stdout);
  }

  defaultQueryWindows(pid) {
    const res = powershell(this.run, WINDOW_QUERY, { ...this.baseEnv, TB_WT_PID: String(pid) });
    if (res.error || res.status !== 0) throw new Error(`the window query failed (${res.error?.message ?? `exit ${res.status}`}): ${String(res.stderr ?? "").slice(0, 300)}`);
    return parseWindows(res.stdout);
  }

  defaultPostClose(pid, hwnd, startKey) {
    const res = powershell(this.run, WINDOW_CLOSE, { ...this.baseEnv, TB_WT_PID: String(pid), TB_WT_HWND: String(hwnd), TB_WT_START: String(startKey) });
    if (res.error || res.status !== 0) return `error: ${res.error?.message ?? `exit ${res.status}`}: ${String(res.stderr ?? "").slice(0, 200)}`;
    return String(res.stdout ?? "").trim();
  }

  /** Run `wt.exe -w <window> …`; the time the command was issued. */
  invoke(commands, { global = [] } = {}) {
    const argv = wtArgv(this.window, commands, { global });
    const tCmdNs = hrtime();
    const res = this.run(this.wtExe, argv, { encoding: "utf8", windowsHide: false, timeout: 30000, env: this.launchEnv() });
    if (res.error || res.status !== 0) throw new Error(`wt.exe ${commands.map((c) => c[0]).join(" ; ")} failed (${res.error?.message ?? `exit ${res.status}`})`);
    return tCmdNs;
  }

  /** The environment Windows Terminal starts with: no other terminal's session ids. */
  launchEnv() {
    const env = {};
    for (const [k, v] of Object.entries(this.baseEnv)) {
      if (/^(WT_SESSION|WT_PROFILE_ID|TERM_PROGRAM|TERM_PROGRAM_VERSION|TMUX|TMUX_PANE|VSCODE_|CLAUDE_CODE_|CLAUDECODE|QONTINUI_TERMINAL_ID)/.test(k)) continue;
      env[k] = v;
    }
    return env;
  }

  /**
   * Open the run's window with its first subcommands, then prove (D5) that the
   * WindowsTerminal.exe owning it is new and runs from this run's copy.
   *
   * @param {string} window the `-w` name
   * @param {string[][]} commands the first invocation's subcommands
   * @returns {Promise<bigint>} when the command was issued
   */
  async open(window, commands) {
    this.window = window;
    let before;
    try {
      before = this.queryWt();
    } catch (e) {
      throw new AdapterRefusal("shared_process", `the process table could not be read before the launch, so a new Windows Terminal process cannot be proven: ${e.message}`);
    }
    // A WindowsTerminal.exe already running from this copy (an earlier unit's
    // instance that survived its teardown) holds the path-hashed single-instance
    // key: wt.exe would hand it this unit's panes. Refuse before invoking anything.
    const stale = before.filter((p) => samePath(p.exe, this.hostExe));
    if (stale.length > 0) {
      const detail = `WindowsTerminal.exe pid ${stale.map((p) => p.pid).join(", ")} is already running from ${this.hostExe} (an earlier unit's instance survived its teardown); wt.exe would hand it this unit's panes, so nothing was launched (D5)`;
      throw new AdapterRefusal("shared_process", detail, { incident: incident("harness_error", detail, { data: { exe: this.hostExe, survivors: stale.map((p) => p.pid) } }) });
    }
    const tCmdNs = this.invoke(commands, { global: ["-M"] });
    const deadline = Date.now() + this.launchTimeoutMs;
    let proof = null;
    for (;;) {
      if (this.abortSignal?.aborted) throw new WaitAborted(`the Windows Terminal window ${window}`);
      try {
        proof = proveNewWtProcess({ before, after: this.queryWt(), exePath: this.hostExe });
        if (proof.ok) {
          const w = pickWindow(this.queryWindows(proof.pid));
          if (w) {
            this.pid = proof.pid;
            this.startKey = proof.startKey;
            this.hwnd = w.hwnd;
            break;
          }
        } else if (proof.reason === "shared_process") {
          throw new AdapterRefusal("shared_process", proof.detail);
        }
      } catch (e) {
        if (e instanceof AdapterRefusal) throw e;
        this.notes.push(`launch read: ${e.message}`);
      }
      if (Date.now() >= deadline) {
        throw new AdapterRefusal("harness_error", `no window of Windows Terminal's class appeared within ${Math.round(this.launchTimeoutMs / 1000)} s (${proof?.ok ? `pid ${proof.pid} owns none` : proof?.detail ?? "no proof"})`);
      }
      await this.sleep(500);
    }
    this.log(`windows-terminal: pid ${this.pid} (proven new, ${this.hostExe}) window ${this.hwnd}`);
    return tCmdNs;
  }

  /** The process tree this run proved it started (root's start key unchanged), root first. */
  provenTree(who) {
    if (!Number.isInteger(this.pid) || !this.startKey) return { owned: [], table: null };
    let table = null;
    try {
      table = this.list();
      const root = table.get(this.pid);
      if (root && root.startKey === this.startKey) {
        const desc = descendantsOf(table, this.pid).filter((p) => isDescendantOf(table, p.pid, this.pid));
        return { owned: [root, ...desc].map((p) => ({ pid: p.pid, startKey: p.startKey, name: p.name })), table };
      }
      this.notes.push(`${who}: pid ${this.pid} no longer holds the Windows Terminal this run started`);
    } catch (e) {
      this.notes.push(`${who}: process table unreadable: ${e.message}`);
    }
    return { owned: [], table };
  }

  /** Is the proven instance still running (same PID and start key)? */
  alive(table = null) {
    try {
      const t = table ?? this.list();
      return t.get(this.pid)?.startKey === this.startKey;
    } catch {
      return null;
    }
  }

  /**
   * WM_CLOSE to the window this run opened (owner and class re-checked by the
   * script), then the proven WindowsTerminal.exe must exit; whatever of the
   * proven tree still runs is ended by PID + start key, children first.
   *
   * @returns {Promise<{closed: string[], survivors: object[]}>}
   */
  async close() {
    const closed = [];
    const survivors = [];
    if (!Number.isInteger(this.pid)) return { closed, survivors };
    const { owned } = this.provenTree("teardown");
    if (this.alive() && Number.isInteger(this.hwnd)) {
      const r = this.postClose(this.pid, this.hwnd, this.startKey);
      if (r === "posted") closed.push(`WM_CLOSE to window ${this.hwnd}`);
      else this.notes.push(`teardown: WM_CLOSE not posted to window ${this.hwnd} (${r})`);
    }
    const deadline = Date.now() + this.closeTimeoutMs;
    while (this.alive() === true && Date.now() < deadline) await this.sleep(250);
    if (this.alive() === false) closed.push(`WindowsTerminal.exe pid ${this.pid} exited`);
    let alive = [];
    try {
      const now = this.list();
      alive = owned.filter((o) => now.get(o.pid)?.startKey === o.startKey);
      for (const signal of ["SIGTERM", "SIGKILL"]) {
        if (alive.length === 0) break;
        const t = this.list();
        for (const o of [...alive].reverse()) {
          terminatePid(o.pid, { signal, table: t, expect: (p) => p.startKey === o.startKey, ...(this.kill ? { kill: this.kill } : {}) });
        }
        await this.sleep(1500);
        const after = this.list();
        alive = alive.filter((o) => after.get(o.pid)?.startKey === o.startKey);
      }
    } catch (e) {
      this.notes.push(`teardown: read-back failed: ${e.message}`);
    }
    for (const o of alive) survivors.push({ pid: o.pid, what: `${o.name} pid ${o.pid}`, detail: "survived WM_CLOSE, SIGTERM and SIGKILL" });
    return { closed, survivors };
  }

  /** A second signal's last act: SIGKILL the proven tree, children first. */
  emergencyStop() {
    const { owned, table } = this.provenTree("emergency stop");
    if (owned.length === 0) return null;
    let n = 0;
    for (const o of [...owned].reverse()) {
      if (terminatePid(o.pid, { signal: "SIGKILL", table, expect: (p) => p.startKey === o.startKey, ...(this.kill ? { kill: this.kill } : {}) }).outcome === "signalled") n += 1;
    }
    return `signalled Windows Terminal pid ${this.pid} (${n} of ${owned.length} process(es))`;
  }

  settingsDigest() {
    try {
      return `sha256:${createHash("sha256").update(readFileSync(this.settingsPath)).digest("hex")}`;
    } catch {
      return absent("harness_error", "the private settings.json was not written");
    }
  }

  appVersion(extra = "") {
    const sid = this.sessionId.supported === true ? "--sessionId accepted" : this.sessionId.supported === false ? "no --sessionId" : "--sessionId unchecked";
    return `Windows Terminal ${this.pin.version} (portable; ${sid})${extra}`;
  }
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

export class WindowsTerminalAdapter {
  static hostIdKey = HOST_ID_KEY;

  /**
   * @param {object} [o] PortableWindowsTerminal's options, plus `variant`, `uuid`
   *   (session-id source, for tests), `generatorWaitMs`, `paneSettleMs`.
   */
  constructor(o = {}) {
    this.id = "windows-terminal";
    this.hostIdKey = HOST_ID_KEY;
    this.variant = o.variant ?? "default";
    if (this.variant !== "default") throw new RangeError(`unknown windows-terminal variant ${this.variant}`);
    this.log = o.log ?? (() => {});
    this.platform = o.platform ?? process.platform;
    this.baseEnv = o.env ?? process.env;
    this.wt = o.wt ?? new PortableWindowsTerminal(o);
    this.list = o.list ?? (() => listProcesses({ platform: this.platform }));
    this.kill = o.kill;
    this.exists = o.exists ?? existsSync;
    this.sleep = o.sleep ?? sleepMs;
    this.uuid = o.uuid ?? randomUUID;
    this.generatorWaitMs = o.generatorWaitMs ?? 15000;
    this.paneSettleMs = o.paneSettleMs ?? 500;
    this.shellPath = windowsPowerShellPath(this.baseEnv);
    this.layout = null;
    this.slots = new Map();
    this.arm = null;
    this.focusSession = null;
    this.spawned = [];
    this.census = null;
    this.notes = [];
    this.ctx = null;
  }

  async check() {
    if (this.platform !== "win32") {
      return { mechanisms: { "setup.windows_terminal": { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `the windows-terminal setup runs on Windows; this is ${this.platform}` } } };
    }
    return {
      mechanisms: {
        "setup.windows_terminal": await this.wt.installFact(),
        "setup.windows_terminal_shell": this.exists(this.shellPath)
          ? { state: "OPERATIVE", reason: `session shell ${this.shellPath}` }
          : { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `no session shell at ${this.shellPath}` },
      },
    };
  }

  /** @param {{runDir: string, unitDir: string, genDir: string, genRunId: string, signal?: AbortSignal}} ctx */
  async launch(ctx) {
    this.ctx = ctx;
    const facts = (await this.check()).mechanisms;
    for (const id of ["setup.windows_terminal", "setup.windows_terminal_shell"]) {
      if (facts[id]?.state !== "OPERATIVE") throw new AdapterRefusal("harness_error", `${id}: ${facts[id]?.reason ?? "no verdict"}`);
    }
    const r = await this.wt.prepare({ runDir: ctx.runDir, shellPath: this.shellPath, signal: ctx.signal ?? null });
    if (r.sessionId.supported !== true) this.notes.push(r.sessionId.detail);
    this.window = windowName(ctx.genRunId);
    return { ok: true };
  }

  /** A pane spec for one session. */
  paneSpec(title) {
    const sid = this.wt.sessionId.supported === true ? newSessionId(this.uuid) : null;
    return { title, sessionId: sid?.arg ?? null, hostId: sid?.env ?? `${this.window}:${title}`, cmd: paneCommand(this.argv, { shellPath: this.shellPath }) };
  }

  /**
   * Every session started in the tiled layout (the arm measured first): tab 0's
   * grid of up to nine, then one tab per further session, in invocations of at
   * most TABS_PER_INVOCATION tabs so no command line nears Windows' limit.
   */
  async spawnSessions(n, argv) {
    this.argv = argv;
    const specs = Array.from({ length: n }, (_, i) => this.paneSpec(`s${i}`));
    const grid = specs.slice(0, TILED_MAX_PANES);
    const extra = specs.slice(TILED_MAX_PANES);
    const g = gridCommands(grid, { profile: BENCH_PROFILE });
    const sessions = [];
    const t0 = await this.wt.open(this.window, g.commands);
    for (const s of grid) sessions.push({ hostId: s.hostId, tCmdNs: t0 });
    for (let i = 0; i < extra.length; i += TABS_PER_INVOCATION) {
      const batch = extra.slice(i, i + TABS_PER_INVOCATION);
      await this.sleep(this.wt.settleMs);
      const t = this.wt.invoke(batch.map((s) => ["new-tab", "-p", BENCH_PROFILE, "--title", s.title, ...(s.sessionId ? ["--sessionId", s.sessionId] : []), ...s.cmd]));
      for (const s of batch) sessions.push({ hostId: s.hostId, tCmdNs: t });
    }
    this.layout = WtLayout.afterSpawn(grid.map((s) => s.hostId), g.ids, extra.map((s) => s.hostId));
    this.wt.invoke([["focus-tab", "-t", "0"]]);
    return { sessions, admission: null };
  }

  takeCensus() {
    if (this.platform !== "win32" || !Number.isInteger(this.wt.pid)) return null;
    try {
      return ptyHostCensus(this.list(), [this.wt.pid]);
    } catch (e) {
      this.notes.push(`pty-host census failed: ${e.message}`);
      return null;
    }
  }

  /**
   * @param {"focus" | "tiled"} arm
   * @param {{slots: Map<number, string>, n: number, look?: () => Promise<{ok: boolean, sessions: Map<number, object>, detail: string}>}} actx
   */
  async arrange(arm, actx) {
    this.slots = new Map(actx.slots);
    this.arm = arm;
    const order = [...this.slots.keys()].sort((a, b) => a - b);
    const vTarget = arm === "focus" ? 1 : Math.min(Math.max(order.length, 1), TILED_MAX_PANES);
    const switchable = arm === "focus" ? order : [];
    if (order.length === 0) return { ok: false, vTarget, failure: { reason: "harness_error", detail: "no session is mapped to a pane" } };
    if (!this.census) this.census = this.takeCensus();
    this.focusSession = order[0];
    const notes = [];
    try {
      if (arm === "focus") {
        const cmds = this.layout.toFocus();
        if (cmds.length > 0) this.wt.invoke(cmds);
        this.wt.invoke([["focus-tab", "-t", String(this.layout.tabOf(this.slots.get(order[0])))]]);
      } else {
        const shown = order.slice(0, vTarget).map((s) => this.slots.get(s));
        if (this.layout.shape === "focus" && shown.length > 1) {
          this.wt.invoke(this.layout.toTiled(shown));
          notes.push("re-tiled with move-pane: Windows Terminal splits each target pane in its automatic direction, so pane sizes are uneven");
        }
        const p = this.layout.paneOf(this.slots.get(order[0]));
        this.wt.invoke([["focus-tab", "-t", String(p.tab)], ["focus-pane", "-t", String(p.id)]]);
      }
    } catch (e) {
      return { ok: false, vTarget, focusSession: order[0], switchable, notes, failure: { reason: "harness_error", detail: e.message } };
    }
    await this.sleep(this.wt.settleMs);
    if (typeof actx.look === "function") {
      const seen = await actx.look();
      if (!seen.ok) notes.push(`look: ${seen.detail}`);
      else if (arm === "focus" && !(seen.sessions.size === 1 && seen.sessions.has(order[0]))) {
        return { ok: false, vTarget, focusSession: order[0], switchable, notes, failure: { reason: "sessions_not_rendered", detail: `focus on session ${order[0]}, but the probe saw sessions [${[...seen.sessions.keys()].join(",")}]` } };
      } else if (arm === "tiled" && seen.sessions.size < vTarget) notes.push(`the probe saw ${seen.sessions.size} of ${vTarget} panes right after arranging`);
    }
    await this.sleep(this.paneSettleMs);
    const shownSlots = arm === "focus" ? [order[0]] : order.slice(0, vTarget);
    const paneSizes = this.ctx?.genDir ? shownSlots.map((k) => latestPaneSize(readGeneratorLog(this.ctx.genDir, k))).filter(Boolean) : [];
    if (paneSizes.length < shownSlots.length) notes.push(`pane size known for ${paneSizes.length} of ${shownSlots.length} shown session(s): the rest never reported a resize`);
    return { ok: true, vTarget, focusSession: order[0], switchable, paneSizes, notes, layout: { tabs: this.layout.tabs.length } };
  }

  async show(k) {
    const hostId = this.slots.get(k);
    const t = hostId ? this.layout.tabOf(hostId) : -1;
    if (t < 0) return { ok: false, detail: `session ${k} has no tab` };
    try {
      const tCmdNs = this.wt.invoke([["focus-tab", "-t", String(t)]]);
      this.focusSession = k;
      return { ok: true, tCmdNs };
    } catch (e) {
      return { ok: false, detail: e.message };
    }
  }

  async spawnOne() {
    const s = this.paneSpec(`spawn${this.spawned.length}`);
    const term = ["-p", SPAWNED_PROFILE, "--title", s.title, ...(s.sessionId ? ["--sessionId", s.sessionId] : []), ...s.cmd];
    try {
      let tCmdNs;
      if (this.arm === "tiled") {
        tCmdNs = this.wt.invoke([["focus-tab", "-t", "0"], ["split-pane", ...term]]);
        this.layout.addPane(0, s.hostId);
      } else {
        tCmdNs = this.wt.invoke([["new-tab", ...term]]);
        this.layout.addTab(s.hostId);
      }
      this.spawned.push(s.hostId);
      return { ok: true, hostId: s.hostId, tCmdNs };
    } catch (e) {
      return { ok: false, detail: e.message };
    }
  }

  /**
   * End a spawned session's shell (and its generator) by PID: the slot file names
   * both, each is re-checked against the process table (the shell a descendant of
   * the proven Windows Terminal, the generator carrying this run's id), and its
   * profile closes the pane when the shell exits.
   */
  async closeOne(hostId) {
    const id = hostId ?? this.spawned.pop();
    if (!id) return { ok: false, detail: "nothing to close" };
    this.spawned = this.spawned.filter((x) => x !== id);
    const rec = [...readSlots(this.ctx.genDir).values()].find((s) => s.host_terminal_id?.[HOST_ID_KEY] === id);
    if (!rec) return { ok: false, detail: `no generator slot reports ${HOST_ID_KEY}=${id}` };
    let table;
    try {
      table = this.list();
    } catch (e) {
      return { ok: false, detail: `process table unreadable: ${e.message}` };
    }
    const shell = table.get(rec.ppid);
    if (!shell || !isDescendantOf(table, rec.ppid, this.wt.pid) || table.get(this.wt.pid)?.startKey !== this.wt.startKey) {
      return { ok: false, detail: `session shell pid ${rec.ppid} is not a descendant of the proven Windows Terminal pid ${this.wt.pid}` };
    }
    const mine = (p) => carriesFlag(p, GENERATOR_FLAG, this.ctx.genRunId);
    const gen = table.get(rec.pid);
    if (gen && mine(gen)) {
      terminatePid(rec.pid, { table, expect: (p) => p.startKey === gen.startKey && mine(p), ...(this.kill ? { kill: this.kill } : {}) });
    }
    const r = terminatePid(rec.ppid, { table, expect: (p) => p.startKey === shell.startKey, ...(this.kill ? { kill: this.kill } : {}) });
    this.layout.remove(id);
    return r.outcome === "signalled" || r.outcome === "gone" ? { ok: true } : { ok: false, detail: r.detail ?? r.outcome };
  }

  async focusObserved() {
    const session = this.focusSession;
    const p = this.layout?.paneOf(this.slots.get(session));
    if (!p) return { ok: false, failure: { reason: "harness_error", detail: `focused session ${session} has no pane` } };
    try {
      this.wt.invoke([["focus-tab", "-t", String(p.tab)], ["focus-pane", "-t", String(p.id)]]);
      return { ok: true, session };
    } catch (e) {
      return { ok: false, failure: { reason: "harness_error", detail: e.message } };
    }
  }

  processRoots() {
    return Number.isInteger(this.wt.pid) && this.wt.pid > 0 ? [this.wt.pid] : [];
  }

  /** The window this run opened, by handle; never a title (the operator's windows share it). */
  windowTarget() {
    if (Number.isInteger(this.wt.hwnd)) return { hwnd: this.wt.hwnd };
    return Number.isInteger(this.wt.pid) ? { pid: this.wt.pid } : null;
  }

  describe(slots = new Map()) {
    let table = null;
    try {
      table = this.list();
    } catch {
      // recorded below as absences
    }
    const shell = table ? shellFacts(slots, table, this.platform) : null;
    const census = this.census ?? (table && Number.isInteger(this.wt.pid) ? ptyHostCensus(table, [this.wt.pid]) : null);
    return {
      id: "windows-terminal",
      variant: this.variant,
      app_version: this.wt.appVersion(),
      build: { state: "not_applicable", reason: "not_applicable_to_setup" },
      settings_digest: this.wt.settingsDigest(),
      pty_host_impl: ptyHostImpl({ platform: this.platform, census, what: "Windows Terminal" }),
      render_backend: this.wt.settings ? wtRenderBackend(this.wt.settings) : absent("harness_error", "the private settings were not written"),
      pty_holder: { state: "not_applicable", reason: "not_applicable_to_setup" },
      launch_mode: "portable_pinned",
      session_shell: shell?.session_shell ?? "powershell.exe -NoLogo -NoExit",
      shell_integration: shell?.shell_integration ?? absent("harness_error", "no session shell found in the process table"),
    };
  }

  diagnostics() {
    return {};
  }

  /** Wait (bounded) for the generators to leave on the stop file: they must exit 0 before the window closes. */
  async waitGenerators() {
    if (!this.ctx?.genDir) return;
    const slots = [...readSlots(this.ctx.genDir).values()];
    const deadline = Date.now() + this.generatorWaitMs;
    for (;;) {
      let live = [];
      try {
        const t = this.list();
        live = slots.filter((s) => t.has(s.pid) && carriesFlag(t.get(s.pid), GENERATOR_FLAG, this.ctx.genRunId));
      } catch {
        return;
      }
      if (live.length === 0) return;
      if (Date.now() >= deadline) {
        this.notes.push(`teardown: ${live.length} generator(s) still running ${Math.round(this.generatorWaitMs / 1000)} s after the stop file`);
        return;
      }
      await this.sleep(250);
    }
  }

  emergencyStop() {
    return this.wt.emergencyStop();
  }

  async teardown() {
    await this.waitGenerators();
    const { closed, survivors } = await this.wt.close();
    return { closed, survivors, incidents: [], notes: [...this.notes, ...this.wt.notes] };
  }
}
