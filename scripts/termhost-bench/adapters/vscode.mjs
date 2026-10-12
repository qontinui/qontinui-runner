/**
 * The `vscode` setup adapter (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * Phase 4, D4, D5, D13): VS Code's integrated terminal, measured in an ISOLATED
 * instance — never the operator's own editor.
 *
 * Install (D5): the pinned portable build (`vscode-install.mjs`: Windows ZIP,
 * Linux tar.gz, SHA-256 checked against Microsoft's published hash, cached
 * outside any repository, unpacked into a new directory of the run), or an
 * installed app named by `--vscode-app-path`, measured "as configured" — and
 * then only when the launch proves the process tree is this run's own.
 *
 * Isolation (D5): every byte VS Code writes lands in the unit directory.
 *   - Portable mode: `VSCODE_PORTABLE=<unit>/vscode/portable`. The app honours it
 *     only when the directory exists and its product.json has no install
 *     `target`; the pinned archives have none. Without it VS Code reads, and
 *     creates when absent, `~/.vscode/argv.json` in the user's home. An
 *     installed build (`--vscode-app-path`) normally HAS a target, so portable
 *     mode is off there: the adapter records it (doctor reason, `app_version`),
 *     and the only file outside the unit is that argv.json.
 *   - `--user-data-dir`, `--extensions-dir` and `--shared-data-dir` inside that directory,
 *     `--disable-workspace-trust`, in-memory secret storage (no keychain write),
 *     no update or experiment fetches, no crash reporter.
 *   - Settings written into the private profile (`vscodeSettings`): telemetry
 *     off, scrollback, and only what keeps the measured panes uncovered and
 *     teardown free of dialogs. `terminal.integrated.gpuAcceleration` and
 *     `terminal.integrated.windowsUseConptyDll` are left at their defaults and
 *     RECORDED (as `render_backend` and, beside the measured census,
 *     `pty_host_impl`).
 *
 * Driving it: `vscode-harness-extension/`, loaded with `--extensionDevelopmentPath`,
 * creates and arranges the terminals through VS Code's stable API and serves a
 * 127.0.0.1 control endpoint (port written into the unit directory, a per-launch
 * nonce required on every call). `protocol.js` is shared with this client.
 *
 *   focus     panel tabs, the panel maximized, one shown (V = 1)
 *   tiled     a grid of up to 3 x 3 editor groups, one terminal editor each
 *             (V = min(N, 9)); sessions beyond nine stay as hidden panel tabs
 *   show(k)   the setup's own way: `Terminal.show()`
 *   spawnOne  a new terminal tab — in the panel (focus) or the active editor
 *             group (tiled) — the same command line (D13)
 *   closeOne  `Terminal.dispose()`
 *   roots     the instance's main process; the probe walks its descendants: the
 *             renderer, GPU, shared, extension-host and pty-host utility
 *             processes and the ConPTY hosts (generators and their shells are
 *             workload and excluded by the probe)
 *
 * Teardown acts only on PIDs this run proved it started: the main process the
 * adapter spawned (by handle, then by PID + start key) and the descendants it
 * recorded with their start keys. `workbench.action.quit` first; never a kill by
 * image name — the operator's own `Code.exe` shares the name.
 */

import { spawn, spawnSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { basename, dirname, join, win32 } from "node:path";
import { fileURLToPath } from "node:url";

import { WaitAborted } from "../../lib/runner-clients.mjs";
import { descendantsOf, listProcesses, terminatePid } from "../proc.mjs";
import { ptyHostCensus, ptyHostImpl } from "../ptyhost.mjs";
import { readGeneratorLog } from "../sessions.mjs";
import { assertInside, commandLine } from "../rundir.mjs";
import { AdapterRefusal } from "./index.mjs";
import { sessionShellArgv } from "./tmux.mjs";
import {
  ArchiveIntegrityError,
  VSCODE_PIN,
  defaultCacheDir,
  ensureArchive,
  ensureUnpacked,
  findEnclosingRepo,
  linuxSandboxVerdict,
  locateApp,
  metadataUrl,
  readProduct,
  resolveAppPath,
  sha256File,
} from "./vscode-install.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

/** The harness extension, loaded with `--extensionDevelopmentPath`. */
export const HARNESS_EXTENSION_DIR = join(HERE, "..", "vscode-harness-extension");

/** The control protocol, shared with the extension (one definition, both ends). */
export const protocol = require("../vscode-harness-extension/protocol.js");

/** The variable each session exports, naming its terminal (D13 slot mapping). */
export const HOST_ID_KEY = protocol.TERMINAL_ID_ENV;

/**
 * Scrollback lines per terminal: the runner's own xterm scrollback
 * (`src/components/terminal/TerminalInstance.tsx` `scrollback: 10000`), so each
 * host keeps the same history per pane and memory compares like for like.
 */
export const SCROLLBACK_LINES = 10000;

/** Most visible panes in the tiled arm (D4). */
export const TILED_MAX_PANES = protocol.MAX_TILED;

const hrtime = () => process.hrtime.bigint();
const sleepMs = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * The private profile's `settings.json`. Deliberately ABSENT:
 * `terminal.integrated.gpuAcceleration` and `terminal.integrated.windowsUseConptyDll`
 * (their defaults are what an operator gets; both are read back and recorded).
 */
export function vscodeSettings({ scrollback = SCROLLBACK_LINES } = {}) {
  return {
    // Telemetry and anything that fetches behaviour from the network mid-run.
    "telemetry.telemetryLevel": "off",
    "workbench.enableExperiments": false,
    "update.mode": "none",
    "update.showReleaseNotes": false,
    "extensions.autoUpdate": false,
    "extensions.autoCheckUpdates": false,
    "extensions.ignoreRecommendations": true,
    // Startup UI that would take the area the panes need: the welcome editor,
    // the secondary side bar, built-in AI chat views and sign-in prompts, tips.
    "workbench.startupEditor": "none",
    "workbench.secondarySideBar.defaultVisibility": "hidden",
    "chat.disableAIFeatures": true,
    "workbench.tips.enabled": false,
    "workbench.welcomePage.walkthroughs.openOnInstall": false,
    "security.workspace.trust.enabled": false,
    "window.newWindowDimensions": "maximized",
    // The focus arm maximizes the panel with a toggle. Under the default
    // "preserve", a panel hidden while maximized reopens maximized and the toggle
    // would UN-maximize it; "never" makes every open start un-maximized, so one
    // toggle always lands maximized.
    "workbench.panel.opensMaximized": "never",
    "window.restoreWindows": "none",
    // Teardown without a modal dialog (killing an editor terminal that has
    // children asks by default).
    "window.confirmBeforeClose": "never",
    "terminal.integrated.confirmOnKill": "never",
    "terminal.integrated.confirmOnExit": "never",
    "terminal.integrated.scrollback": scrollback,
  };
}

/** `argv.json` in the portable directory: crash reporting off, as telemetry is. */
export const ARGV_JSON = Object.freeze({ "enable-crash-reporter": false });

/** Where one unit's private profile lives. */
export function profileLayout(unitDir) {
  const base = join(unitDir, "vscode");
  const portable = join(base, "portable");
  return {
    base,
    portable,
    userData: join(portable, "user-data"),
    extensions: join(portable, "extensions"),
    sharedData: join(portable, "shared-data"),
    settings: join(portable, "user-data", "User", "settings.json"),
    argvJson: join(portable, "argv.json"),
    config: join(base, "harness-config.json"),
    control: join(base, "control.json"),
    harnessLog: join(base, "harness.log"),
    stdout: join(base, "code-stdout.log"),
    stderr: join(base, "code-stderr.log"),
  };
}

/**
 * The isolated instance's argv. No folder is opened (an empty window); GPU
 * acceleration is never forced either way.
 *
 * @param {{exe: string, userDataDir: string, extensionsDir: string, sharedDataDir?: string | null,
 *   extensionDir: string, noSandbox?: boolean}} o
 */
export function vscodeArgv({ exe, userDataDir, extensionsDir, sharedDataDir = null, extensionDir, noSandbox = false }) {
  return [
    exe,
    "--user-data-dir",
    userDataDir,
    "--extensions-dir",
    extensionsDir,
    // Explicit, so shared data stays private even where portable mode is off.
    // An undocumented option (no --help line) in the pinned 1.139.1: its
    // out/main.js parses "shared-data-dir" as a string argument, and
    // `appSharedDataHome` returns it ahead of VSCODE_PORTABLE and the home folder
    // (observed 2026-10-12 in the unpacked archive). A build without it warns
    // about an unknown option and falls back to those.
    ...(sharedDataDir ? ["--shared-data-dir", sharedDataDir] : []),
    "--extensionDevelopmentPath",
    extensionDir,
    "--disable-workspace-trust",
    "--new-window",
    "--skip-welcome",
    "--skip-release-notes",
    "--skip-add-to-recently-opened",
    "--disable-telemetry",
    "--disable-crash-reporter",
    "--disable-updates",
    "--disable-experiments",
    "--use-inmemory-secretstorage",
    ...(noSandbox ? ["--no-sandbox"] : []),
  ];
}

/** Variables that tie a process to another editor, terminal or session. */
const FOREIGN_ENV = /^(VSCODE_|ELECTRON_|CLAUDE_CODE_|TMUX)/;
const FOREIGN_KEYS = new Set([
  "CLAUDECODE",
  "QONTINUI_TERMINAL_ID",
  "QONTINUI_RUNNER_CONTEXT",
  "QONTINUI_PINNED_SESSION_ID",
  "TERM_PROGRAM",
  "TERM_PROGRAM_VERSION",
  "WT_SESSION",
  "WT_PROFILE_ID",
  protocol.TERMINAL_ID_ENV,
  protocol.CONFIG_ENV,
]);

/**
 * The instance's environment: the harness's own, minus anything that would tie
 * it to another editor or session (`ELECTRON_RUN_AS_NODE` would turn Code.exe
 * into plain node; `VSCODE_*` names the parent editor's IPC), plus portable mode,
 * the harness config, and the environment's display on Linux.
 */
export function vscodeEnv(base, { portableDir, configPath, display = null, platform = process.platform }) {
  const env = {};
  for (const [k, v] of Object.entries(base)) {
    if (FOREIGN_ENV.test(k) || FOREIGN_KEYS.has(k)) continue;
    env[k] = v;
  }
  if (portableDir) env.VSCODE_PORTABLE = portableDir;
  env[protocol.CONFIG_ENV] = configPath;
  if (platform === "linux" && display) env.DISPLAY = display;
  return env;
}

/** Windows PowerShell 5.1 by absolute path: the runner's session shell (D13). */
export function windowsPowerShellPath(env = process.env) {
  return win32.join(env.SystemRoot || env.windir || "C:\\Windows", "System32", "WindowsPowerShell", "v1.0", "powershell.exe");
}

/**
 * D13: the platform's default interactive shell with the generator as its child,
 * on the one command line every setup gives it — on Windows
 * `powershell.exe -NoLogo -NoExit -Command <line>` (the runner's shell, which
 * Windows Terminal is given the same way), elsewhere tmux's form
 * `<shell> -i -c '<line>; exec <shell> -i'`. The shell outlives the generator,
 * as an operator's would.
 *
 * @param {{platform?: string, argv: string[], shell?: string | null, env?: object}} o
 * @returns {{shellPath: string, shellArgs: string[], sessionShell: string}}
 */
export function d13Shell({ platform = process.platform, argv, shell = null, env = process.env }) {
  if (platform === "win32") {
    return {
      shellPath: windowsPowerShellPath(env),
      shellArgs: ["-NoLogo", "-NoExit", "-Command", commandLine(argv, "powershell")],
      sessionShell: "powershell.exe -NoLogo -NoExit",
    };
  }
  const sh = shell || env.SHELL || "/bin/bash";
  const full = sessionShellArgv(sh, argv);
  return { shellPath: full[0], shellArgs: full.slice(1), sessionShell: `${basename(sh)} -i` };
}

/** The session shell's path alone (fixed in the harness config at launch). */
export function sessionShellPath({ platform = process.platform, shell = null, env = process.env }) {
  return platform === "win32" ? windowsPowerShellPath(env) : shell || env.SHELL || "/bin/bash";
}

/**
 * `render_backend`: the resolved `terminal.integrated.gpuAcceleration` value
 * (default `auto` = WebGL with a DOM fallback VS Code takes itself; no stable API
 * reports which one a pane ended up on, so the setting is what is recorded).
 */
export function renderBackend(readback) {
  const v = readback?.value;
  if (typeof v === "string" && /^[a-z]+$/.test(v)) return `gpu_acceleration_${v}`;
  return { state: "unknown", reason: "harness_error", detail: "terminal.integrated.gpuAcceleration was not read back" };
}

/** The pty host `terminal.integrated.windowsUseConptyDll` selects, or null. */
export function settingPtyHostImpl(readback) {
  if (readback?.value === true) return "bundled_openconsole";
  if (readback?.value === false) return "system_conhost";
  return null;
}

/**
 * Is `pid` a descendant of `ancestor` in `table`? Walks parent links upward, each
 * parent started no later than its child (a later start is a recycled PID, and
 * the chain is broken there).
 */
export function isDescendantOf(table, pid, ancestor, { maxDepth = 64 } = {}) {
  let cur = table.get(pid);
  for (let i = 0; cur && i < maxDepth; i += 1) {
    const parent = table.get(cur.ppid);
    if (!parent || cur.ppid === cur.pid) return false;
    if (BigInt(parent.startKey || "0") > BigInt(cur.startKey || "0")) return false;
    if (parent.pid === ancestor) return true;
    cur = parent;
  }
  return false;
}

/**
 * The session shell and whether VS Code injected its shell integration, measured
 * from the shell's command line (VS Code injects by rewriting the shell's
 * arguments to source `shellIntegration*`; it does not for a `-Command` / `-c`
 * session, which is what D13 gives it).
 */
export function shellFacts(slots, table, platform = process.platform) {
  for (const rec of slots.values()) {
    const shell = table.get(rec.ppid);
    if (!shell) continue;
    const name = basename(String(Array.isArray(shell.argv) ? (shell.argv[0] ?? shell.name) : shell.name));
    const cmd = shell.cmdline ?? "";
    const session =
      platform === "win32"
        ? /powershell/i.test(name) && /-NoLogo/i.test(cmd) && /-NoExit/i.test(cmd)
          ? "powershell.exe -NoLogo -NoExit"
          : `${name} (unexpected arguments)`
        : `${name} -i`;
    return { session_shell: session, shell_integration: /shellIntegration/i.test(cmd) ? "injected" : "not_injected" };
  }
  return null;
}

/**
 * The pane size a session last reported through its generator's `resize`
 * records, or null. VS Code's stable API exposes no terminal dimensions
 * (`Terminal.dimensions` is proposed), so the session itself is asked. The start
 * record's `terminal_size` is NOT used: it is the size the session was created
 * at, before any arrangement, and taking it would let `pane_smaller_than_profile`
 * be decided by a pane that no longer exists. A session that never resized is an
 * unknown size, and the arrangement's notes say how many were.
 */
export function latestPaneSize(records) {
  let size = null;
  for (const r of records) {
    if (r?.type === "resize" && Number.isInteger(r.cols) && Number.isInteger(r.rows)) size = { cols: r.cols, rows: r.rows };
  }
  return size;
}

/** A failed call to the harness endpoint. */
export class HarnessCallError extends Error {
  constructor(message, { status = null, code = null } = {}) {
    super(message);
    this.name = "HarnessCallError";
    this.status = status;
    this.code = code;
  }
}

/** The client of the extension's control endpoint. */
export class HarnessClient {
  /** @param {{port: number, nonce: string, fetch?: typeof fetch}} o */
  constructor({ port, nonce, fetch = globalThis.fetch }) {
    this.port = port;
    this.nonce = nonce;
    this.fetch = fetch;
  }

  /** `signal`: the run's abort, for launch-time calls only (teardown's calls must still run after it). */
  async call(cmd, args = {}, { timeoutMs = 30000, signal = null } = {}) {
    const req = protocol.buildRpc(cmd, args, this.nonce);
    let res;
    try {
      res = await this.fetch(`http://127.0.0.1:${this.port}${req.path}`, {
        method: "POST",
        headers: req.headers,
        body: req.body,
        signal: signal ? AbortSignal.any([AbortSignal.timeout(timeoutMs), signal]) : AbortSignal.timeout(timeoutMs),
      });
    } catch (e) {
      if (signal?.aborted) throw new WaitAborted(`the harness extension's ${cmd}`);
      throw new HarnessCallError(`${cmd}: the harness endpoint did not answer (${e?.message ?? e})`);
    }
    const text = await res.text();
    let body;
    try {
      body = JSON.parse(text);
    } catch {
      throw new HarnessCallError(`${cmd}: HTTP ${res.status} with a non-JSON body`, { status: res.status });
    }
    if (!res.ok || body?.ok !== true) {
      throw new HarnessCallError(`${cmd}: ${body?.error?.code ?? `HTTP ${res.status}`}: ${body?.error?.message ?? ""}`.trim(), {
        status: res.status,
        code: body?.error?.code ?? null,
      });
    }
    return body.result;
  }
}

const absent = (reason, detail) => ({ state: "unknown", reason, detail });

export class VscodeAdapter {
  static hostIdKey = HOST_ID_KEY;

  /**
   * @param {{variant?: string, log?: (s: string) => void, appPath?: string | null, cacheDir?: string | null,
   *   platform?: string, arch?: string, env?: object, shell?: string | null, fetch?: typeof fetch, controlFetch?: typeof fetch,
   *   spawnFn?: typeof spawn, run?: typeof spawnSync, list?: () => Map<number, object>, kill?: typeof process.kill,
   *   exists?: (p: string) => boolean, sleep?: (ms: number) => Promise<void>, launchTimeoutMs?: number,
   *   quitTimeoutMs?: number, sandbox?: (chromeSandbox: string | null) => {state: string, reason: string, noSandbox: boolean},
   *   pin?: typeof VSCODE_PIN}} [o]
   */
  constructor(o = {}) {
    this.id = "vscode";
    this.hostIdKey = HOST_ID_KEY;
    this.variant = o.variant ?? "default";
    if (this.variant !== "default") throw new RangeError(`unknown vscode variant ${this.variant}`);
    this.log = o.log ?? (() => {});
    this.appPath = o.appPath ?? null;
    this.platform = o.platform ?? process.platform;
    this.arch = o.arch ?? process.arch;
    this.baseEnv = o.env ?? process.env;
    this.cacheDir = o.cacheDir ?? defaultCacheDir({ platform: this.platform, env: this.baseEnv });
    this.shell = o.shell ?? null;
    // Archive downloads and the localhost control endpoint are separate concerns.
    this.fetch = o.fetch ?? globalThis.fetch;
    this.controlFetch = o.controlFetch ?? globalThis.fetch;
    this.spawnFn = o.spawnFn ?? spawn;
    this.run = o.run ?? spawnSync;
    this.list = o.list ?? (() => listProcesses({ platform: this.platform }));
    this.kill = o.kill;
    this.exists = o.exists ?? existsSync;
    this.sleep = o.sleep ?? sleepMs;
    this.launchTimeoutMs = o.launchTimeoutMs ?? 180000;
    this.quitTimeoutMs = o.quitTimeoutMs ?? 20000;
    this.sandboxFn = o.sandbox ?? null;
    this.hashFn = o.hash ?? null;
    this.paneSettleMs = o.paneSettleMs ?? 500;
    this.cacheVerified = null;
    this.pin = o.pin ?? VSCODE_PIN;
    this.app = null;
    this.paths = null;
    this.child = null;
    this.childExit = null;
    this.pid = null;
    this.startKey = null;
    this.abortSignal = null;
    this.client = null;
    this.hello = null;
    this.shellLaunch = null;
    this.slots = new Map();
    this.arm = null;
    this.focusSession = null;
    this.spawned = [];
    this.census = null;
    this.notes = [];
  }

  /** Doctor facts, side-effect free (a cached archive is re-hashed, nothing downloaded). */
  async check() {
    const m = {};
    if (!["win32", "linux"].includes(this.platform)) {
      const reason = `the vscode setup runs on Windows and Linux; this is ${this.platform}`;
      return { mechanisms: { "setup.vscode": { state: "INOPERATIVE-ON-THIS-MACHINE", reason } } };
    }
    m["setup.vscode"] = await this.installFact();
    m["setup.vscode_extension"] = this.extensionFact();
    const shellPath = sessionShellPath({ platform: this.platform, shell: this.shell, env: this.baseEnv });
    m["setup.vscode_shell"] = this.exists(shellPath)
      ? { state: "OPERATIVE", reason: `session shell ${shellPath}` }
      : { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `no session shell at ${shellPath}` };
    if (this.platform === "linux") {
      const s = this.sandboxVerdict(null);
      m["setup.vscode_sandbox"] = { state: s.state, reason: s.reason };
    }
    return { mechanisms: m };
  }

  /** This platform's build in the pin, or null. */
  build() {
    return this.pin.builds[`${this.platform}-${this.arch}`] ?? null;
  }

  sandboxVerdict(chromeSandbox) {
    return this.sandboxFn ? this.sandboxFn(chromeSandbox) : linuxSandboxVerdict({ chromeSandbox });
  }

  async installFact() {
    if (this.appPath) {
      try {
        const app = resolveAppPath(this.appPath, { platform: this.platform, exists: this.exists });
        const product = app.productJson ? readProduct(app.productJson) : null;
        const off = product && Object.hasOwn(product, "target") ? `; portable mode OFF (product target "${product.target}"): VS Code reads, and creates when absent, ~/.vscode/argv.json` : "";
        return { state: "OPERATIVE", reason: `installed VS Code ${product?.version ?? "version unknown"} at ${app.exe} ("as configured"; private profile; exclusivity proven at launch${off})` };
      } catch (e) {
        return { state: "INOPERATIVE-ON-THIS-MACHINE", reason: e.message };
      }
    }
    const build = this.build();
    if (!build) return { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `no pinned VS Code build for ${this.platform}-${this.arch}` };
    const repo = findEnclosingRepo(this.cacheDir, { exists: this.exists });
    if (repo) return { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `the archive cache ${this.cacheDir} is inside the git checkout ${repo} (D5)` };
    const cached = join(this.cacheDir, this.pin.version, build.archive);
    if (!this.exists(cached)) {
      return {
        state: "OPERATIVE",
        reason: `pinned VS Code ${this.pin.version} (sha256 ${build.sha256.slice(0, 16)}…, ${metadataUrl(build, this.pin.version)}) not cached; the first launch downloads it into ${this.cacheDir} and verifies it`,
      };
    }
    try {
      const st = statSync(cached);
      const actual = await (this.hashFn ?? sha256File)(cached);
      // Handed to the launch's ensureArchive, so a 336 MB archive is hashed once.
      this.cacheVerified = { path: cached, size: st.size, mtimeMs: st.mtimeMs, sha256: actual };
      return actual === build.sha256
        ? { state: "OPERATIVE", reason: `pinned VS Code ${this.pin.version} cached at ${cached}, SHA-256 verified` }
        : { state: "OPERATIVE", reason: `the cached ${cached} fails SHA-256 (${actual.slice(0, 16)}…); the launch discards it and downloads the pinned archive again` };
    } catch (e) {
      return { state: "UNKNOWN", reason: `cannot hash ${cached}: ${e.message}` };
    }
  }

  extensionFact() {
    try {
      const pkg = JSON.parse(readFileSync(join(HARNESS_EXTENSION_DIR, "package.json"), "utf8"));
      const main = join(HARNESS_EXTENSION_DIR, pkg.main ?? "");
      if (!pkg.engines?.vscode || !this.exists(main)) throw new Error("the manifest names no engine or its main is missing");
      return { state: "OPERATIVE", reason: `harness extension ${pkg.name}@${pkg.version} (engines.vscode ${pkg.engines.vscode})` };
    } catch (e) {
      return { state: "INOPERATIVE-ON-THIS-MACHINE", reason: `the harness extension at ${HARNESS_EXTENSION_DIR} is unusable: ${e.message}` };
    }
  }

  /** The pinned build: cached (verified) or downloaded (verified), unpacked into this run. */
  async installPinned(ctx) {
    const build = this.build();
    if (!build) throw new AdapterRefusal("harness_error", `no pinned VS Code build for ${this.platform}-${this.arch}`);
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
        signal: this.abortSignal,
        ...(this.hashFn ? { hash: this.hashFn } : {}),
      });
    } catch (e) {
      if (e instanceof WaitAborted) throw e;
      throw new AdapterRefusal("harness_error", e instanceof ArchiveIntegrityError ? `refusing the VS Code archive: ${e.message}` : e.message);
    }
    const appDir = await ensureUnpacked({
      archive: archive.path,
      build,
      runDir: ctx.runDir,
      version: this.pin.version,
      commit: this.pin.commit,
      run: this.run,
      platform: this.platform,
      env: this.baseEnv,
      log: this.log,
      exists: this.exists,
    });
    const located = locateApp(appDir, build, { exists: this.exists });
    return { ...located, appDir, product: readProduct(located.productJson), launchMode: "portable_pinned", archiveSha256: archive.sha256 };
  }

  /**
   * `ctx.signal` is the run's abort: every launch-time wait (the archive download,
   * the control file, the hello and prepare calls) ends at it with `WaitAborted`,
   * and the run then tears down as usual.
   *
   * @param {{runDir: string, unitDir: string, genDir: string, genRunId: string, env?: {env?: object}, signal?: AbortSignal}} ctx
   */
  async launch(ctx) {
    this.ctx = ctx;
    this.abortSignal = ctx.signal ?? null;
    const facts = (await this.check()).mechanisms;
    for (const id of ["setup.vscode", "setup.vscode_extension", "setup.vscode_shell"]) {
      if (facts[id]?.state !== "OPERATIVE") throw new AdapterRefusal("harness_error", `${id}: ${facts[id]?.reason ?? "no verdict"}`);
    }
    if (this.appPath) {
      const app = resolveAppPath(this.appPath, { platform: this.platform, exists: this.exists });
      const product = app.productJson ? readProduct(app.productJson) : null;
      // An installed build's product.json names its install `target` (user /
      // system), and VS Code then ignores VSCODE_PORTABLE: argv.json is read (and
      // created when absent) in ~/.vscode. Recorded, never hidden.
      const target = product && Object.hasOwn(product, "target") ? String(product.target) : null;
      this.app = { ...app, chromeSandbox: null, product, launchMode: "installed_app_path", portable: target === null, target };
      if (target !== null) this.notes.push(`portable mode is off: the installed build's product target is "${target}", so VS Code reads ~/.vscode/argv.json`);
    } else {
      this.app = await this.installPinned(ctx);
      this.app.target = this.app.product && Object.hasOwn(this.app.product, "target") ? String(this.app.product.target) : null;
      this.app.portable = this.app.target === null;
    }
    const p = profileLayout(ctx.unitDir);
    for (const k of Object.keys(p)) assertInside(ctx.runDir, p[k]);
    this.paths = p;
    for (const d of [p.base, p.portable, p.userData, p.extensions, dirname(p.settings)]) mkdirSync(d, { recursive: true });
    writeFileSync(p.settings, `${JSON.stringify(vscodeSettings(), null, 2)}\n`);
    if (this.app.portable) writeFileSync(p.argvJson, `${JSON.stringify(ARGV_JSON, null, 2)}\n`);
    const nonce = randomBytes(32).toString("hex");
    const shellPath = sessionShellPath({ platform: this.platform, shell: this.shell, env: this.baseEnv });
    writeFileSync(
      p.config,
      `${JSON.stringify({ schema: protocol.CONFIG_SCHEMA, nonce, controlFile: p.control, shellPath, platform: this.platform, logFile: p.harnessLog, terminalPrefix: "vsc" }, null, 2)}\n`,
      { mode: 0o600 },
    );
    const sandbox = this.platform === "linux" ? this.sandboxVerdict(this.app.chromeSandbox) : null;
    if (sandbox?.noSandbox) this.notes.push(sandbox.reason);
    const argv = vscodeArgv({ exe: this.app.exe, userDataDir: p.userData, extensionsDir: p.extensions, sharedDataDir: p.sharedData, extensionDir: HARNESS_EXTENSION_DIR, noSandbox: sandbox?.noSandbox === true });
    const env = vscodeEnv(this.baseEnv, { portableDir: this.app.portable ? p.portable : null, configPath: p.config, display: ctx.env?.env?.DISPLAY ?? null, platform: this.platform });
    const out = openSync(p.stdout, "a");
    const err = openSync(p.stderr, "a");
    const t0 = Date.now();
    try {
      this.child = this.spawnFn(argv[0], argv.slice(1), { cwd: p.base, env, stdio: ["ignore", out, err], windowsHide: false });
    } finally {
      closeSync(out);
      closeSync(err);
    }
    this.pid = this.child.pid;
    this.child.on?.("exit", (code, signal) => {
      this.childExit = { code, signal, afterMs: Date.now() - t0 };
    });
    try {
      // Read while we hold the child's handle: Windows does not reuse a PID whose
      // process object is still open, so this key is our process's. (The read is
      // synchronous, so no exit event can land during it.) What happens to the PID
      // AFTER our child exits is teardown's guard: it claims nothing once
      // `childExit` is set.
      this.startKey = this.list().get(this.pid)?.startKey ?? null;
    } catch (e) {
      this.notes.push(`process table unreadable after launch: ${e.message}`);
    }
    this.log(`vscode: pid ${this.pid} (${this.app.launchMode}) from ${this.app.exe}`);
    const control = await this.waitForControl(p.control);
    this.client = new HarnessClient({ port: control.port, nonce, fetch: this.controlFetch });
    this.hello = await this.client.call("hello", {}, { timeoutMs: 30000, signal: this.abortSignal });
    if (this.hello?.protocol !== protocol.PROTOCOL) throw new AdapterRefusal("harness_error", `the extension speaks ${this.hello?.protocol}, not ${protocol.PROTOCOL}`);
    this.proveExclusive();
    if (this.app.launchMode === "portable_pinned" && this.hello.vscode?.version !== this.pin.version) {
      throw new AdapterRefusal("harness_error", `the instance reports VS Code ${this.hello.vscode?.version}, not the pinned ${this.pin.version}`);
    }
    const prep = await this.client.call("prepare", {}, { timeoutMs: 60000, signal: this.abortSignal });
    for (const f of prep.failed ?? []) this.notes.push(`prepare: ${f.id} failed: ${f.error}`);
    return { ok: true };
  }

  /** The control file, once the extension listens; a launch that exits or stalls is refused with its reason. */
  async waitForControl(file) {
    const deadline = Date.now() + this.launchTimeoutMs;
    let last = null;
    for (;;) {
      if (this.abortSignal?.aborted) throw new WaitAborted(`the VS Code harness extension's control file (pid ${this.pid})`);
      if (this.childExit) {
        const e = this.childExit;
        if (e.code === 0 && e.afterMs < 15000) {
          throw new AdapterRefusal("shared_process", `VS Code pid ${this.pid} exited 0 after ${e.afterMs} ms: it handed off to another instance, so the process tree is not this run's (D5)`);
        }
        throw new AdapterRefusal("harness_error", `VS Code exited (${JSON.stringify(e)}) before its harness extension listened${this.stderrTail()}`);
      }
      try {
        return protocol.parseControl(readFileSync(file, "utf8"));
      } catch (e) {
        if (e?.code !== "ENOENT") last = e;
      }
      if (Date.now() >= deadline) {
        throw new AdapterRefusal("harness_error", `the harness extension wrote no control file within ${Math.round(this.launchTimeoutMs / 1000)} s${last ? ` (${last.message})` : ""}${this.stderrTail()}`);
      }
      await this.sleep(250);
    }
  }

  stderrTail() {
    try {
      const t = readFileSync(this.paths.stderr, "utf8").trim();
      return t ? ` — stderr: ${t.slice(-400)}` : "";
    } catch {
      return "";
    }
  }

  /**
   * D5: the extension host that answered must be a descendant of the main
   * process this adapter spawned — a launch that attached to some other VS Code
   * would measure the operator's processes.
   */
  proveExclusive() {
    let table;
    try {
      table = this.list();
    } catch (e) {
      throw new AdapterRefusal("shared_process", `the process tree could not be read to prove it is this run's own: ${e.message}`);
    }
    const host = this.hello?.process?.pid;
    if (!Number.isInteger(host) || !isDescendantOf(table, host, this.pid)) {
      throw new AdapterRefusal("shared_process", `the extension host pid ${host} is not a descendant of the launched VS Code pid ${this.pid} (D5)`);
    }
  }

  async spawnSessions(n, argv) {
    this.argv = argv;
    const sh = d13Shell({ platform: this.platform, argv, shell: this.shell, env: this.baseEnv });
    this.shellLaunch = { shellPath: sh.shellPath, shellArgs: sh.shellArgs };
    const sessions = [];
    for (let done = 0; done < n; ) {
      const count = Math.min(protocol.MAX_SPAWN, n - done);
      const tCmdNs = hrtime();
      const r = await this.client.call("spawn", { count, location: "panel", ...this.shellLaunch, show: false }, { timeoutMs: 30000 + 3000 * count });
      for (const t of r.terminals) sessions.push({ hostId: t.id, tCmdNs });
      done += count;
    }
    return { sessions, admission: null };
  }

  /** The ConPTY hosts under this instance, while its sessions are live (Windows). */
  takeCensus() {
    if (this.platform !== "win32" || !Number.isInteger(this.pid)) return null;
    try {
      return ptyHostCensus(this.list(), [this.pid]);
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
    if (order.length === 0) return { ok: false, vTarget, failure: { reason: "harness_error", detail: "no session is mapped to a terminal" } };
    if (!this.census) this.census = this.takeCensus();
    this.focusSession = order[0];
    const switchable = arm === "focus" ? order : [];
    let r;
    try {
      r = await this.client.call("arrange", { arm, order: order.map((s) => this.slots.get(s)) }, { timeoutMs: 120000 });
    } catch (e) {
      return { ok: false, vTarget, focusSession: order[0], switchable, failure: { reason: "harness_error", detail: e.message } };
    }
    const notes = [...(r.notes ?? [])];
    if (!r.ok) {
      return { ok: false, vTarget, focusSession: order[0], switchable, notes, failure: { reason: "arm_not_realisable", detail: (r.failures ?? []).join("; ") || "the extension reported the arm unrealised" } };
    }
    if (typeof actx.look === "function") {
      const seen = await actx.look();
      if (!seen.ok) {
        notes.push(`look: ${seen.detail}`);
      } else if (arm === "focus" && !(seen.sessions.size === 1 && seen.sessions.has(order[0]))) {
        return {
          ok: false,
          vTarget,
          focusSession: order[0],
          switchable,
          notes,
          failure: { reason: "sessions_not_rendered", detail: `focus on session ${order[0]}, but the probe saw sessions [${[...seen.sessions.keys()].join(",")}]` },
        };
      } else if (arm === "tiled" && seen.sessions.size < vTarget) {
        notes.push(`the probe saw ${seen.sessions.size} of ${vTarget} panes right after arranging`);
      }
    }
    // Let the host's resize reach the shown sessions, then read the size each reported.
    await this.sleep(this.paneSettleMs);
    const shown = arm === "focus" ? [order[0]] : order.slice(0, vTarget);
    const paneSizes = this.ctx?.genDir ? shown.map((k) => latestPaneSize(readGeneratorLog(this.ctx.genDir, k))).filter(Boolean) : [];
    if (paneSizes.length < shown.length) notes.push(`pane size known for ${paneSizes.length} of ${shown.length} shown session(s): the rest never reported a resize`);
    return { ok: true, vTarget, focusSession: order[0], switchable, paneSizes, notes, layout: { rows: r.rows ?? [1], groups: r.groups ?? [] } };
  }

  async show(k) {
    const id = this.slots.get(k);
    if (!id) return { ok: false, detail: `session ${k} has no terminal` };
    const tCmdNs = hrtime();
    try {
      await this.client.call("show", { id, preserveFocus: false });
      this.focusSession = k;
      return { ok: true, tCmdNs };
    } catch (e) {
      return { ok: false, detail: e.message };
    }
  }

  async spawnOne() {
    const tCmdNs = hrtime();
    try {
      const r = await this.client.call("spawn", { count: 1, location: this.arm === "tiled" ? "editor-active" : "panel", ...this.shellLaunch, show: true });
      const hostId = r.terminals[0].id;
      this.spawned.push(hostId);
      return { ok: true, hostId, tCmdNs };
    } catch (e) {
      return { ok: false, tCmdNs, detail: e.message };
    }
  }

  async closeOne(hostId) {
    const id = hostId ?? this.spawned.pop();
    if (!id) return { ok: false, detail: "nothing to close" };
    this.spawned = this.spawned.filter((x) => x !== id);
    try {
      const r = await this.client.call("close", { id });
      return r.closed.includes(id) ? { ok: true } : { ok: false, detail: `terminal ${id} did not close within its bound` };
    } catch (e) {
      return { ok: false, detail: e.message };
    }
  }

  async focusObserved() {
    const session = this.focusSession;
    const id = this.slots.get(session);
    if (!id) return { ok: false, failure: { reason: "harness_error", detail: `focused session ${session} has no terminal` } };
    try {
      const r = await this.client.call("focus", { id });
      return r.active ? { ok: true, session } : { ok: false, failure: { reason: "harness_error", detail: `terminal ${id} did not become the active terminal` } };
    } catch (e) {
      return { ok: false, failure: { reason: "harness_error", detail: e.message } };
    }
  }

  processRoots() {
    return Number.isInteger(this.pid) && this.pid > 0 ? [this.pid] : [];
  }

  /** The workbench window: the main process owns it. Never a title — the operator's editors share it. */
  windowTarget() {
    return Number.isInteger(this.pid) ? { pid: this.pid } : null;
  }

  settingsDigest() {
    try {
      return `sha256:${createHash("sha256").update(readFileSync(this.paths.settings)).digest("hex")}`;
    } catch {
      return absent("harness_error", "the private settings.json was not written");
    }
  }

  appVersion() {
    const version = this.hello?.vscode?.version ?? this.app?.product?.version ?? null;
    if (!version) return absent("harness_error", "neither the instance nor its product.json named a version");
    const commit = this.app?.product?.commit ? `${this.app.product.commit.slice(0, 12)}, ` : "";
    const electron = this.hello?.process?.electron ? `Electron ${this.hello.process.electron}` : "Electron ?";
    const portable = this.app && this.app.portable === false ? `; portable mode off (product target "${this.app.target}")` : "";
    return `VS Code ${version} (${commit}${electron})${portable}`;
  }

  describe(slots = new Map()) {
    let table = null;
    try {
      table = this.list();
    } catch {
      // recorded below as absences
    }
    const shell = table ? shellFacts(slots, table, this.platform) : null;
    const census = this.census ?? (table && this.platform === "win32" && Number.isInteger(this.pid) ? ptyHostCensus(table, [this.pid]) : null);
    return {
      id: "vscode",
      variant: this.variant,
      app_version: this.appVersion(),
      build: { state: "not_applicable", reason: "not_applicable_to_setup" },
      settings_digest: this.settingsDigest(),
      pty_host_impl: ptyHostImpl({ platform: this.platform, census, settingImpl: settingPtyHostImpl(this.hello?.settings?.windowsUseConptyDll), what: "VS Code" }),
      render_backend: renderBackend(this.hello?.settings?.gpuAcceleration),
      pty_holder: { state: "not_applicable", reason: "not_applicable_to_setup" },
      launch_mode: this.app?.launchMode ?? (this.appPath ? "installed_app_path" : "portable_pinned"),
      session_shell: shell?.session_shell ?? (this.platform === "win32" ? "powershell.exe -NoLogo -NoExit" : `${basename(this.shell || this.baseEnv.SHELL || "bash")} -i`),
      shell_integration: shell?.shell_integration ?? absent("harness_error", "no session shell found in the process table"),
    };
  }

  /** Runner-only diagnostics (D1): nothing for a third-party host. */
  diagnostics() {
    return {};
  }

  /** Wait for our child to exit, by its handle. */
  async waitChildExit(ms) {
    const deadline = Date.now() + ms;
    while (!this.childExit && Date.now() < deadline) await this.sleep(200);
    return Boolean(this.childExit);
  }

  /**
   * The process tree this run PROVED it started, root first, or why none is. The
   * root is claimed only when its start key was read while our child was alive,
   * the process holding the PID now has that same start key, and our child has
   * not exited (an exited child's PID may already belong to a stranger — Windows
   * reuses PIDs quickly). Descendants must chain back to the root with each
   * parent started first (parent links outlive their parents on Windows).
   * Teardown and `emergencyStop` both act only on this.
   *
   * @param {string} who  the caller, for the notes
   * @returns {{owned: {pid: number, startKey: string, name: string}[], unprovable: string | null}}
   */
  provenTree(who) {
    if (!Number.isInteger(this.pid)) return { owned: [], unprovable: null };
    let unprovable = null;
    if (this.startKey === null || this.startKey === undefined) unprovable = "its start key was never recorded";
    else if (this.childExit !== null) unprovable = `our child exited (${JSON.stringify(this.childExit)}) before ${who}`;
    if (unprovable) {
      this.notes.push(`${who}: VS Code pid ${this.pid} cannot be proven this run's (${unprovable}); nothing is signalled`);
      return { owned: [], unprovable };
    }
    try {
      const table = this.list();
      const root = table.get(this.pid);
      if (root && root.startKey === this.startKey) {
        const desc = descendantsOf(table, this.pid).filter((p) => isDescendantOf(table, p.pid, this.pid));
        return { owned: [root, ...desc].map((p) => ({ pid: p.pid, startKey: p.startKey, name: p.name })), unprovable: null };
      }
      this.notes.push(`${who}: pid ${this.pid} no longer holds the process this run started; nothing is signalled`);
    } catch (e) {
      this.notes.push(`${who}: process table unreadable: ${e.message}`);
    }
    return { owned: [], unprovable: null };
  }

  /**
   * A second signal's last act (no teardown follows): SIGKILL the process tree this
   * run proved it started (`provenTree`, the same rule as teardown), children first,
   * each PID re-checked against its start key at the signal. No graceful quit: the
   * run is exiting within seconds. Returns what was done, or null when nothing is
   * provably ours.
   */
  async emergencyStop() {
    const { owned } = this.provenTree("emergency stop");
    if (owned.length === 0) return null;
    const table = this.list();
    let signalled = 0;
    for (const o of [...owned].reverse()) {
      const r = terminatePid(o.pid, { signal: "SIGKILL", table, expect: (p) => p.startKey === o.startKey, ...(this.kill ? { kill: this.kill } : {}) });
      if (r.outcome === "signalled") signalled += 1;
    }
    return `signalled VS Code pid ${this.pid} (${signalled} of ${owned.length} process(es))`;
  }

  async teardown() {
    const closed = [];
    const survivors = [];
    const incidents = [];
    const { owned, unprovable } = this.provenTree("teardown");
    if (this.client) {
      try {
        const r = await this.client.call("close", { all: true }, { timeoutMs: 30000 });
        closed.push(`${r.closed.length} terminal(s)${r.timedOut.length ? ` (${r.timedOut.length} did not confirm)` : ""}`);
      } catch (e) {
        this.notes.push(`teardown: closing the terminals failed: ${e.message}`);
      }
      try {
        await this.client.call("quit", {}, { timeoutMs: 10000 });
      } catch (e) {
        this.notes.push(`teardown: quit failed: ${e.message}`);
      }
    }
    if (this.child && (await this.waitChildExit(this.quitTimeoutMs))) closed.push(`VS Code pid ${this.pid} (quit)`);
    // Whatever this run started and is still running — same PID and start key —
    // is ended by PID, children first; nothing else is touched.
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
    if (owned.length > 0) closed.push(`${owned.length - alive.length} of ${owned.length} owned process(es) gone`);
    for (const o of alive) survivors.push({ pid: o.pid, what: `${o.name} pid ${o.pid}`, detail: "survived quit, SIGTERM and SIGKILL" });
    if (unprovable && this.child && this.childExit === null) {
      survivors.push({ pid: this.pid, what: `VS Code pid ${this.pid}`, detail: `not signalled: ${unprovable}, so it cannot be proven this run's` });
    }
    return { closed, survivors, incidents, notes: [...this.notes] };
  }
}
