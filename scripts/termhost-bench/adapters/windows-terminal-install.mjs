/**
 * The pinned, portable Windows Terminal the `windows-terminal` and `tmux-wsl`
 * setups measure (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * D5 and Phase 5): one stable release, its ZIP's SHA-256, a cache OUTSIDE any
 * repository, verification on every use, and the archive unpacked into its own
 * new directory inside the run directory, with a `.portable` file beside
 * `WindowsTerminal.exe` so its settings live in a `settings\` folder there and
 * never in the operator's `%LOCALAPPDATA%`.
 *
 * Provenance of the pin (observed 2026-10-16 by the implementing session, and
 * true of this pin only): microsoft/terminal release `v1.25.2733.0`, published
 * 2026-10-02T20:27:27Z, is the repository's Latest release (the
 * `releases/latest` URL redirects to it and its page carries the Latest badge);
 * `v1.26.2734.0`, published six seconds later, carries the Pre-release badge. Its
 * asset `Microsoft.WindowsTerminal_1.25.2733.0_x64.zip` (12 127 272 bytes) was
 * downloaded from the release URL below and hashed with `sha256sum` to the value
 * in the table; the release page's own asset digest
 * (`sha256:bf3ef201…`) names the same value. Read, never run: the ZIP holds one
 * top directory, `terminal-1.25.2733.0/`, with `WindowsTerminal.exe`, `wt.exe`,
 * `OpenConsole.exe`, `TerminalApp.dll` and `defaults.json` at its root, and
 * `TerminalApp.dll`'s command-line parser registers the `--sessionId` option
 * (the ASCII option string sits beside `--title` and `--tabColor`). The source
 * at that tag says how the pieces behave, and the adapter relies on exactly that:
 *   - `src/cascadia/TerminalSettingsModel/FileUtils.cpp`: an unpackaged build is
 *     in portable mode iff a `.portable` file sits beside the module, and then
 *     keeps its settings in the `settings` folder there; without it, an
 *     unpackaged build writes `%LOCALAPPDATA%\Microsoft\Windows Terminal\` —
 *     shared with any other unpackaged copy the operator runs. So the unpack
 *     refuses to hand over a directory without the file.
 *   - `src/cascadia/WindowsTerminal/WindowEmperor.cpp`: the single-instance
 *     mutex and window class of an UNPACKAGED build carry a hash of its full
 *     image path. A copy unpacked into this run's directory therefore never
 *     hands its command line to the operator's Windows Terminal (Store or
 *     otherwise), and every later `wt.exe` invocation from the same path hands
 *     off to the run's own instance. The doctor still PROVES the window's
 *     process is new (D5, Risks) rather than trusting this reading.
 *   - `src/cascadia/wt/shim.cpp`: `wt.exe` starts `WindowsTerminal.exe` beside
 *     it with the same command line and exits at once.
 * To move the pin, download the new release's ZIP into an empty directory,
 * `sha256sum` it, compare with the release page's digest, and replace the
 * table; the tests check its shape, never the network.
 */

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, posix, win32 } from "node:path";

import { assertInside } from "../rundir.mjs";
import { unpackCommand } from "./vscode-install.mjs";

/** The pinned build. */
export const WT_PIN = Object.freeze({
  version: "1.25.2733.0",
  tag: "v1.25.2733.0",
  released: "2026-10-02",
  builds: Object.freeze({
    "win32-x64": Object.freeze({
      key: "win32-x64",
      url: "https://github.com/microsoft/terminal/releases/download/v1.25.2733.0/Microsoft.WindowsTerminal_1.25.2733.0_x64.zip",
      fallbackUrl: null,
      sha256: "bf3ef2012f6c44d8340a4c58125acc9498d19b580f9890dc043cdf831852e796",
      bytes: 12127272,
      archive: "Microsoft.WindowsTerminal_1.25.2733.0_x64.zip",
      kind: "zip",
      topDir: "terminal-1.25.2733.0",
    }),
  }),
});

/** Files an unpacked build must hold at its root. */
export const REQUIRED_FILES = Object.freeze(["WindowsTerminal.exe", "wt.exe", "OpenConsole.exe", "TerminalApp.dll"]);

/** The portable-mode marker and settings folder (`FileUtils.cpp`). */
export const PORTABLE_MARKER = ".portable";
export const PORTABLE_SETTINGS_DIR = "settings";

/** The marker a finished unpack leaves, naming the archive it came from. */
export const UNPACK_MARKER = ".termhost-unpacked.json";

/** The pinned build for this platform, or null. */
export function wtBuildFor(platform = process.platform, arch = process.arch, pin = WT_PIN) {
  return pin.builds[`${platform}-${arch}`] ?? null;
}

/**
 * Where the archive is cached by default — per user, outside any checkout:
 * `%LOCALAPPDATA%\termhost-bench\cache\windows-terminal` on Windows.
 */
export function defaultWtCacheDir({ platform = process.platform, env = process.env, home = homedir() } = {}) {
  if (platform === "win32") {
    return win32.join(env.LOCALAPPDATA || win32.join(home, "AppData", "Local"), "termhost-bench", "cache", "windows-terminal");
  }
  return posix.join(env.XDG_CACHE_HOME || posix.join(home, ".cache"), "termhost-bench", "windows-terminal");
}

/**
 * Whether the build's command-line parser accepts `--sessionId` — read from
 * `TerminalApp.dll`'s bytes, never by running anything (`wt --help` opens a
 * dialog). CLI11 keeps option names as narrow strings, so the option is
 * registered iff the literal is present. The flag is in Windows Terminal's
 * source but not on Microsoft Learn (plan Phase 5), hence the check.
 *
 * @returns {{supported: boolean | null, detail: string}}
 */
export function sessionIdSupport(appDir, { read = readFileSync } = {}) {
  const dll = join(appDir, "TerminalApp.dll");
  let bytes;
  try {
    bytes = read(dll);
  } catch (e) {
    return { supported: null, detail: `cannot read ${dll}: ${e.message}` };
  }
  const supported = Buffer.from(bytes).includes(Buffer.from("--sessionId", "latin1"));
  return {
    supported,
    detail: supported
      ? "TerminalApp.dll registers --sessionId: each pane's WT_SESSION is the GUID the harness chose"
      : "TerminalApp.dll registers no --sessionId: panes are matched to generator slots by order",
  };
}

/**
 * Unpack the verified archive into a NEW directory of this run (once per run;
 * later units reuse it after checking its marker names the same archive), prove
 * it holds the pinned build's files, and put the `.portable` marker beside
 * `WindowsTerminal.exe`.
 *
 * @returns {Promise<string>} the app directory (the one holding WindowsTerminal.exe)
 */
export async function ensureWtUnpacked({ archive, build, runDir, version = WT_PIN.version, run = spawnSync, platform = process.platform, env = process.env, log = () => {}, exists = existsSync }) {
  const parent = assertInside(runDir, join(runDir, "wt-app"));
  const final = join(parent, `${version}-${build.key}`);
  const appDir = join(final, build.topDir);
  const marker = join(final, UNPACK_MARKER);
  if (exists(marker)) {
    const m = JSON.parse(readFileSync(marker, "utf8"));
    if (m.sha256 !== build.sha256) throw new Error(`${final} was unpacked from ${m.sha256}, not the pinned ${build.sha256}`);
    if (!exists(join(appDir, PORTABLE_MARKER))) writeFileSync(join(appDir, PORTABLE_MARKER), "");
    return appDir;
  }
  mkdirSync(parent, { recursive: true });
  const staging = mkdtempSync(join(parent, ".unpack-"));
  try {
    const c = unpackCommand({ kind: build.kind, archive, dest: staging, platform, env, exists });
    log(`windows-terminal: unpacking into ${staging}`);
    const res = run(c.cmd, c.args, { encoding: "utf8", timeout: 300000, windowsHide: true, env: c.env ?? env, maxBuffer: 16 * 1024 * 1024 });
    if (res.error || res.status !== 0) {
      throw new Error(`${c.cmd} ${c.args[0]} failed (${res.error?.message ?? `exit ${res.status}`}): ${String(res.stderr ?? "").trim().slice(-400)}`);
    }
    const stagedApp = join(staging, build.topDir);
    const missing = REQUIRED_FILES.filter((f) => !exists(join(stagedApp, f)));
    if (missing.length > 0) throw new Error(`the archive unpacked without ${missing.join(", ")} under ${build.topDir}/: not the pinned Windows Terminal ${version}`);
    writeFileSync(join(stagedApp, PORTABLE_MARKER), "");
    writeFileSync(join(staging, UNPACK_MARKER), `${JSON.stringify({ sha256: build.sha256, version, archive })}\n`);
    renameSync(staging, final);
    return appDir;
  } catch (e) {
    rmSync(staging, { recursive: true, force: true });
    throw e;
  }
}

/** The bench profiles' GUIDs (fixed, so a settings digest compares across runs). */
export const BENCH_PROFILE_GUID = "{5d1d7a6e-4c0b-4e0c-9a51-7e3b1c2a0b01}";
export const SPAWNED_PROFILE_GUID = "{5d1d7a6e-4c0b-4e0c-9a51-7e3b1c2a0b02}";
export const BENCH_PROFILE = "termhost-bench";
export const SPAWNED_PROFILE = "termhost-bench-spawned";

/**
 * Scrollback lines per pane: the runner's own xterm scrollback, as VS Code gets
 * (`vscode.mjs` SCROLLBACK_LINES), so every host keeps the same history.
 */
export const WT_HISTORY_SIZE = 10000;

/** The fixed font: present on every Windows install, at one size on every machine. */
export const WT_FONT = Object.freeze({ face: "Consolas", size: 10 });

/**
 * The private `settings\settings.json`. What the plan asks for (opacity 100, a
 * fixed font size, `closeOnExit: graceful`), what keeps the measured panes
 * uncovered and teardown free of dialogs (no close-all-tabs confirmation, no
 * headless process left behind, no restored layout), and nothing else: every
 * `rendering.*` setting is left at its default and RECORDED (`render_backend`).
 *
 * The second profile exists for `spawnOne`'s panes only. Windows Terminal has no
 * command-line verb that closes a pane (the pinned parser's subcommands are
 * x-save, new-tab, split-pane, focus-tab, move-focus, move-pane, focus-pane), so
 * `closeOne` ends that pane's session shell by PID, which exits non-zero, and
 * under `graceful` a non-zero exit leaves the pane open showing the exit code
 * (`TerminalPaneContent.cpp`). `always` closes it; the base sessions keep the
 * plan's `graceful`.
 *
 * @param {{shellPath: string, fontSize?: number, historySize?: number}} o
 */
export function wtSettings({ shellPath, fontSize = WT_FONT.size, historySize = WT_HISTORY_SIZE }) {
  if (typeof shellPath !== "string" || shellPath === "") throw new TypeError("wtSettings needs the session shell's path");
  const profile = (guid, name, closeOnExit) => ({
    guid,
    name,
    commandline: `${shellPath} -NoLogo -NoExit`,
    hidden: false,
    closeOnExit,
    opacity: 100,
    useAcrylic: false,
    font: { face: WT_FONT.face, size: fontSize },
    historySize,
    bellStyle: "none",
    colorScheme: "Campbell",
    startingDirectory: null,
  });
  return {
    $schema: "https://aka.ms/terminal-profiles-schema",
    defaultProfile: BENCH_PROFILE_GUID,
    firstWindowPreference: "defaultProfile",
    launchMode: "maximized",
    confirmCloseAllTabs: false,
    "warning.confirmCloseAllTabs": false,
    "compatibility.allowHeadless": false,
    minimizeToNotificationArea: false,
    alwaysShowNotificationIcon: false,
    startOnUserLogin: false,
    disabledProfileSources: ["Windows.Terminal.Wsl", "Windows.Terminal.Azure", "Windows.Terminal.PowershellCore", "Windows.Terminal.VisualStudio", "Windows.Terminal.SSH"],
    profiles: {
      defaults: {},
      list: [profile(BENCH_PROFILE_GUID, BENCH_PROFILE, "graceful"), profile(SPAWNED_PROFILE_GUID, SPAWNED_PROFILE, "always")],
    },
    schemes: [],
    actions: [],
  };
}

/**
 * `render_backend` for a results row: the Atlas engine with the graphics API the
 * settings resolve to (absent = Windows Terminal's default, `automatic`).
 */
export function wtRenderBackend(settings) {
  const api = settings?.["rendering.graphicsAPI"] ?? "automatic";
  const software = settings?.["rendering.software"] === true ? "_software" : "";
  return `atlas_graphics_api_${String(api).replace(/[^A-Za-z0-9]/g, "").toLowerCase()}${software}`;
}
