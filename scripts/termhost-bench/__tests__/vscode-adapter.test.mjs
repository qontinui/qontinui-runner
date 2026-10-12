/**
 * The `vscode` setup adapter (`adapters/vscode.mjs`, `adapters/vscode-install.mjs`,
 * `ptyhost.mjs`) without launching VS Code: the pinned build and its SHA-256
 * refusal, the cache outside any repository, the unpack into a new directory,
 * the isolated instance's argv / environment / private settings, D13's session
 * shell, the pty-host census, and the whole launch -> spawn -> arrange -> show ->
 * spawn/close -> focus -> describe -> teardown flow driven through the REAL
 * harness extension code (`vscode-harness-extension/harness.js`) running in this
 * process over a simulated workbench, with the "launch" faked at the process
 * spawn.
 *
 *   node --test scripts/termhost-bench/__tests__/vscode-adapter.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { EventEmitter } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { AdapterRefusal } from "../adapters/index.mjs";
import {
  ARGV_JSON,
  HARNESS_EXTENSION_DIR,
  HOST_ID_KEY,
  SCROLLBACK_LINES,
  VscodeAdapter,
  d13Shell,
  isDescendantOf,
  profileLayout,
  protocol,
  renderBackend,
  sessionShellPath,
  settingPtyHostImpl,
  shellFacts,
  vscodeArgv,
  vscodeEnv,
  vscodeSettings,
  windowsPowerShellPath,
} from "../adapters/vscode.mjs";
import {
  ArchiveIntegrityError,
  UNPACK_MARKER,
  VSCODE_PIN,
  buildFor,
  defaultCacheDir,
  downloadVerified,
  ensureArchive,
  ensureUnpacked,
  findEnclosingRepo,
  linuxSandboxVerdict,
  metadataUrl,
  resolveAppPath,
  unpackCommand,
} from "../adapters/vscode-install.mjs";
import { ptyHostCensus, ptyHostFromCensus, ptyHostImpl } from "../ptyhost.mjs";
import { generatorArgv } from "../rundir.mjs";
import { createFakeVscode } from "./helpers/fake-vscode.mjs";

const require = createRequire(import.meta.url);
const { startHarness } = require("../vscode-harness-extension/harness.js");

const sha = (b) => createHash("sha256").update(b).digest("hex");
const WIN_ENV = { SystemRoot: "C:\\Windows", PATH: "C:\\Windows\\System32" };
const PS = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";

function tmp(t, prefix = "tb-vsc-") {
  const d = mkdtempSync(join(tmpdir(), prefix));
  t.after(() => rmSync(d, { recursive: true, force: true }));
  return d;
}

/** A run directory with one unit, as `rundir.mjs` lays it out. */
function unitDirs(t) {
  const runDir = tmp(t, "tb-vsc-run-");
  const unitDir = join(runDir, "units", "u000-vscode-default-none-n5-r0");
  const genDir = join(unitDir, "gen");
  mkdirSync(genDir, { recursive: true });
  return { runDir, unitDir, genDir };
}

/** A response whose body streams `bytes`, as `fetch` returns one. */
function response(bytes, status = 200) {
  return { ok: status >= 200 && status < 300, status, body: (async function* () { yield bytes.subarray(0, 7); yield bytes.subarray(7); })() };
}

// ---------------------------------------------------------------------------
// The pinned build
// ---------------------------------------------------------------------------

test("the pin: one stable version, an official SHA-256 per archive, URLs that name that build", () => {
  assert.match(VSCODE_PIN.version, /^\d+\.\d+\.\d+$/);
  assert.match(VSCODE_PIN.commit, /^[0-9a-f]{40}$/);
  for (const key of ["win32-x64", "linux-x64"]) {
    const b = VSCODE_PIN.builds[key];
    assert.equal(b.key, key);
    assert.match(b.sha256, /^[0-9a-f]{64}$/);
    assert.ok(b.url.startsWith("https://") && b.url.includes(VSCODE_PIN.commit), b.url);
    assert.equal(b.fallbackUrl, `https://update.code.visualstudio.com/${VSCODE_PIN.version}/${b.updatePlatform}/stable`);
    assert.equal(metadataUrl(b), `https://update.code.visualstudio.com/api/versions/${VSCODE_PIN.version}/${b.updatePlatform}/stable`);
    assert.ok(b.url.endsWith(b.archive));
  }
  assert.equal(VSCODE_PIN.builds["win32-x64"].kind, "zip");
  assert.equal(VSCODE_PIN.builds["win32-x64"].product, `${VSCODE_PIN.commit.slice(0, 10)}/resources/app/product.json`, "the Windows ZIP keeps its app under the short commit");
  assert.equal(VSCODE_PIN.builds["linux-x64"].kind, "tar.gz");
  assert.equal(buildFor("win32", "x64"), VSCODE_PIN.builds["win32-x64"]);
  assert.equal(buildFor("darwin", "arm64"), null);
});

test("the archive cache defaults outside any checkout, and one inside a checkout is found", (t) => {
  assert.equal(defaultCacheDir({ platform: "win32", env: { LOCALAPPDATA: "C:\\Users\\u\\AppData\\Local" } }), "C:\\Users\\u\\AppData\\Local\\termhost-bench\\cache\\vscode");
  assert.equal(defaultCacheDir({ platform: "linux", env: { XDG_CACHE_HOME: "/x/cache" } }), "/x/cache/termhost-bench/vscode");
  assert.equal(defaultCacheDir({ platform: "linux", env: {}, home: "/home/u" }), "/home/u/.cache/termhost-bench/vscode");
  const repo = tmp(t);
  mkdirSync(join(repo, ".git"));
  mkdirSync(join(repo, "deep", "cache"), { recursive: true });
  assert.equal(findEnclosingRepo(join(repo, "deep", "cache")), repo);
  assert.equal(findEnclosingRepo(tmp(t)), null);
});

test("a download whose SHA-256 differs from the pin is REFUSED: nothing kept, no partial file, no retry elsewhere", async (t) => {
  const dir = tmp(t);
  const dest = join(dir, "1.0.0", "a.zip");
  const good = Buffer.from("the archive Microsoft published");
  const urls = [];
  const fetch = async (url) => {
    urls.push(url);
    return response(Buffer.from("something else entirely"));
  };
  await assert.rejects(downloadVerified({ urls: ["https://a/1", "https://a/2"], dest, expected: sha(good), fetch }), (e) => {
    assert.ok(e instanceof ArchiveIntegrityError);
    assert.equal(e.expected, sha(good));
    assert.equal(e.actual, sha(Buffer.from("something else entirely")));
    assert.match(e.message, /refused, nothing unpacked/);
    return true;
  });
  assert.deepEqual(urls, ["https://a/1"], "a wrong archive is not fetched again from the fallback");
  assert.equal(existsSync(dest), false);
  assert.deepEqual(readdirSync(dirname(dest)), [], "the partial download is deleted");
});

test("a matching download is kept; an unreachable URL falls back to the next", async (t) => {
  const dir = tmp(t);
  const dest = join(dir, "v", "a.zip");
  const good = Buffer.from("archive bytes");
  const fetch = async (url) => {
    if (url.endsWith("/down")) throw new Error("ECONNRESET");
    return response(good);
  };
  const r = await downloadVerified({ urls: ["https://a/down", "https://a/up"], dest, expected: sha(good), fetch });
  assert.equal(r.url, "https://a/up");
  assert.equal(r.bytes, good.length);
  assert.deepEqual(readFileSync(dest), good);
  await assert.rejects(downloadVerified({ urls: ["https://a/x"], dest: join(dir, "b"), expected: sha(good), fetch: async () => response(good, 404) }), /HTTP 404/);
});

test("the cache: a verified hit is reused, a corrupted file is replaced, a cache inside a checkout is refused", async (t) => {
  const cacheDir = tmp(t);
  const good = Buffer.from("pinned bytes");
  const build = { ...VSCODE_PIN.builds["linux-x64"], url: "https://a/b", fallbackUrl: null, archive: "a.tar.gz", sha256: sha(good) };
  let fetched = 0;
  const fetch = async () => {
    fetched += 1;
    return response(good);
  };
  const first = await ensureArchive({ build, cacheDir, version: "9.9.9", fetch });
  assert.deepEqual([first.cached, fetched], [false, 1]);
  const second = await ensureArchive({ build, cacheDir, version: "9.9.9", fetch });
  assert.deepEqual([second.cached, fetched], [true, 1]);
  writeFileSync(first.path, "tampered");
  const third = await ensureArchive({ build, cacheDir, version: "9.9.9", fetch });
  assert.deepEqual([third.cached, fetched], [false, 2]);
  assert.deepEqual(readFileSync(third.path), good);
  const repo = tmp(t);
  mkdirSync(join(repo, ".git"));
  await assert.rejects(ensureArchive({ build, cacheDir: join(repo, "cache"), version: "9.9.9", fetch }), /inside the git checkout/);
});

/**
 * A fake extractor writing the build's layout into the destination: tar's `-C`
 * directory, or Expand-Archive's (passed in the environment) where a Windows path
 * is exercised on a host without `%SystemRoot%\System32\tar.exe`.
 */
function fakeUnpack({ build, version = VSCODE_PIN.version, commit = VSCODE_PIN.commit, calls = [] }) {
  return (cmd, args, opts) => {
    calls.push([cmd, ...args]);
    const dest = args.includes("-C") ? args[args.indexOf("-C") + 1] : opts?.env?.TERMHOST_UNPACK_DEST;
    assert.ok(dest, `the extraction names its destination: ${cmd} ${args.join(" ")}`);
    const file = (rel, text) => {
      const p = join(dest, ...rel.split("/"));
      mkdirSync(dirname(p), { recursive: true });
      writeFileSync(p, text);
    };
    file(build.exe, "binary");
    file(build.product, JSON.stringify({ version, commit, nameShort: "Code" }));
    return { status: 0, stdout: "", stderr: "" };
  };
}

test("unpacking: a new directory per run, proven to be the pinned build, reused by the run's later units", async (t) => {
  const { runDir } = unitDirs(t);
  const build = VSCODE_PIN.builds["linux-x64"];
  const calls = [];
  const app = await ensureUnpacked({ archive: "/c/a.tar.gz", build, runDir, run: fakeUnpack({ build, calls }), platform: "linux" });
  assert.equal(app, join(runDir, "vscode-app", `${VSCODE_PIN.version}-linux-x64`));
  assert.equal(JSON.parse(readFileSync(join(app, UNPACK_MARKER), "utf8")).sha256, build.sha256);
  assert.deepEqual(calls[0].slice(0, 3), ["tar", "-xzf", "/c/a.tar.gz"]);
  assert.match(calls[0][4], /vscode-app[\\/]\.unpack-/, "extracted into a fresh staging directory first");
  await ensureUnpacked({ archive: "/c/a.tar.gz", build, runDir, run: () => assert.fail("a second unit must reuse it"), platform: "linux" });
  const { runDir: other } = unitDirs(t);
  await assert.rejects(ensureUnpacked({ archive: "/c/a.tar.gz", build, runDir: other, run: fakeUnpack({ build, version: "1.0.0" }), platform: "linux" }), /not the pinned/);
  assert.deepEqual(readdirSync(join(other, "vscode-app")), [], "a refused unpack leaves nothing behind");
});

test("the extraction command: Windows' own tar.exe, else Expand-Archive with paths in the environment; GNU tar on Linux", () => {
  const env = { SystemRoot: "C:\\Windows" };
  assert.deepEqual(unpackCommand({ kind: "zip", archive: "C:\\c\\a.zip", dest: "C:\\d", platform: "win32", env, exists: () => true }), {
    cmd: "C:\\Windows\\System32\\tar.exe",
    args: ["-xf", "C:\\c\\a.zip", "-C", "C:\\d"],
  });
  const ps = unpackCommand({ kind: "zip", archive: "C:\\c\\a'b.zip", dest: "C:\\d", platform: "win32", env, exists: () => false });
  assert.equal(ps.cmd, "powershell.exe");
  assert.ok(!ps.args.join(" ").includes("a'b.zip"), "a path is never spliced into the script");
  assert.equal(ps.env.TERMHOST_UNPACK_ARCHIVE, "C:\\c\\a'b.zip");
  assert.deepEqual(unpackCommand({ kind: "tar.gz", archive: "/c/a.tgz", dest: "/d", platform: "linux" }), { cmd: "tar", args: ["-xzf", "/c/a.tgz", "-C", "/d"] });
  assert.throws(() => unpackCommand({ kind: "zip", archive: "a", dest: "d", platform: "linux" }), /Windows/);
});

test("Chromium's sandbox on Linux: root and a missing namespace sandbox need --no-sandbox; userns or a setuid helper do not", () => {
  const files = (map) => (p) => {
    if (p in map) return map[p];
    throw new Error("ENOENT");
  };
  assert.equal(linuxSandboxVerdict({ getuid: () => 0, read: files({}) }).noSandbox, true);
  const userns = linuxSandboxVerdict({ getuid: () => 1000, read: files({ "/proc/sys/user/max_user_namespaces": "63000" }) });
  assert.deepEqual([userns.state, userns.noSandbox], ["OPERATIVE", false]);
  const ubuntu = linuxSandboxVerdict({ getuid: () => 1000, read: files({ "/proc/sys/kernel/apparmor_restrict_unprivileged_userns": "1", "/proc/sys/user/max_user_namespaces": "63000" }) });
  assert.deepEqual([ubuntu.state, ubuntu.noSandbox], ["DEGRADED", true]);
  const suid = linuxSandboxVerdict({
    getuid: () => 1000,
    read: files({ "/proc/sys/kernel/unprivileged_userns_clone": "0" }),
    chromeSandbox: "/a/chrome-sandbox",
    stat: () => ({ uid: 0, mode: 0o104755 }),
  });
  assert.deepEqual([suid.state, suid.noSandbox], ["OPERATIVE", false]);
  assert.equal(linuxSandboxVerdict({ getuid: () => 1000, read: files({}) }).noSandbox, true, "unreadable means --no-sandbox: a window that never opens measures nothing");
});

test("--vscode-app-path accepts the install directory or its executable", (t) => {
  const dir = tmp(t);
  writeFileSync(join(dir, "Code.exe"), "x");
  mkdirSync(join(dir, "abc", "resources", "app"), { recursive: true });
  writeFileSync(join(dir, "abc", "resources", "app", "product.json"), JSON.stringify({ version: "1.2.3", commit: "c".repeat(40) }));
  const a = resolveAppPath(dir, { platform: "win32" });
  assert.equal(a.exe, join(dir, "Code.exe"));
  assert.equal(a.productJson, join(dir, "abc", "resources", "app", "product.json"));
  assert.equal(resolveAppPath(join(dir, "Code.exe"), { platform: "win32" }).exe, join(dir, "Code.exe"));
  assert.throws(() => resolveAppPath(join(dir, "missing")), /does not exist/);
  assert.throws(() => resolveAppPath(tmp(t), { platform: "win32" }), /no Code\.exe/);
});

// ---------------------------------------------------------------------------
// The isolated instance
// ---------------------------------------------------------------------------

test("private settings: telemetry off, the runner's scrollback, and GPU acceleration and the ConPTY DLL left at their defaults", () => {
  const s = vscodeSettings();
  assert.equal(s["telemetry.telemetryLevel"], "off");
  assert.equal(s["terminal.integrated.scrollback"], SCROLLBACK_LINES);
  assert.equal(SCROLLBACK_LINES, 10000);
  assert.equal(s["update.mode"], "none");
  assert.equal(s["terminal.integrated.confirmOnKill"], "never");
  assert.ok(!("terminal.integrated.gpuAcceleration" in s), "gpuAcceleration stays at its default (recorded, not set)");
  assert.ok(!("terminal.integrated.windowsUseConptyDll" in s), "windowsUseConptyDll stays at its default (recorded, not set)");
  assert.deepEqual(ARGV_JSON, { "enable-crash-reporter": false });
});

test("the argv: a private user-data and extensions dir, the harness extension, workspace trust off, no folder, GPU untouched", () => {
  const argv = vscodeArgv({ exe: "C:\\vsc\\Code.exe", userDataDir: "U", extensionsDir: "E", extensionDir: "X" });
  assert.deepEqual(argv.slice(0, 8), ["C:\\vsc\\Code.exe", "--user-data-dir", "U", "--extensions-dir", "E", "--extensionDevelopmentPath", "X", "--disable-workspace-trust"]);
  for (const f of ["--disable-telemetry", "--disable-updates", "--disable-experiments", "--use-inmemory-secretstorage", "--skip-welcome"]) assert.ok(argv.includes(f), f);
  assert.ok(!argv.some((a) => /gpu|hardware-acceleration|enable-proposed-api/.test(a)), "never forces GPU either way, never a proposed API");
  assert.ok(!argv.includes("--no-sandbox"));
  assert.equal(vscodeArgv({ exe: "c", userDataDir: "U", extensionsDir: "E", extensionDir: "X", noSandbox: true }).at(-1), "--no-sandbox");
  const p = profileLayout("/r/units/u0");
  assert.equal(p.userData.replace(/\\/g, "/"), "/r/units/u0/vscode/portable/user-data");
  assert.equal(p.settings.replace(/\\/g, "/"), "/r/units/u0/vscode/portable/user-data/User/settings.json");
});

test("the environment: portable mode and the harness config in; another editor's IPC, ELECTRON_RUN_AS_NODE and session ids out", () => {
  const env = vscodeEnv(
    { PATH: "/bin", ELECTRON_RUN_AS_NODE: "1", VSCODE_IPC_HOOK_CLI: "/run/x.sock", VSCODE_PORTABLE: "/elsewhere", TERM_PROGRAM: "vscode", QONTINUI_TERMINAL_ID: "t", CLAUDECODE: "1", TMUX: "/tmp/t", [HOST_ID_KEY]: "x" },
    { portableDir: "/u/vscode/portable", configPath: "/u/vscode/harness-config.json", display: ":91", platform: "linux" },
  );
  assert.deepEqual(env, { PATH: "/bin", VSCODE_PORTABLE: "/u/vscode/portable", [protocol.CONFIG_ENV]: "/u/vscode/harness-config.json", DISPLAY: ":91" });
  assert.equal(vscodeEnv({}, { portableDir: "p", configPath: "c", display: ":91", platform: "win32" }).DISPLAY, undefined, "DISPLAY is Linux's");
});

test("D13: Windows PowerShell -NoLogo -NoExit -Command <line> on Windows, <shell> -i -c on Linux — one identical line", () => {
  const argv = generatorArgv({ node: "C:\\node\\node.exe", genDir: "C:\\t\\gen", profile: "claude-tui", seed: 1, genRunId: "r-u0" });
  const w = d13Shell({ platform: "win32", argv, env: WIN_ENV });
  assert.equal(w.shellPath, PS);
  assert.deepEqual(w.shellArgs.slice(0, 3), ["-NoLogo", "-NoExit", "-Command"]);
  assert.equal(w.shellArgs[3], "& 'C:\\node\\node.exe' " + `'${argv[1]}'` + " '--run-dir' 'C:\\t\\gen' '--profile' 'claude-tui' '--seed' '1' '--echo' '--termhost-bench-generator' 'r-u0'");
  assert.equal(w.sessionShell, "powershell.exe -NoLogo -NoExit");
  assert.equal(windowsPowerShellPath({}), PS);
  const l = d13Shell({ platform: "linux", argv: ["node", "/g.mjs", "--seed", "1"], shell: "/bin/bash" });
  assert.deepEqual([l.shellPath, ...l.shellArgs], ["/bin/bash", "-i", "-c", "node /g.mjs --seed 1; exec /bin/bash -i"]);
  assert.equal(sessionShellPath({ platform: "linux", env: { SHELL: "/bin/zsh" } }), "/bin/zsh");
});

test("recorded settings: render backend from gpuAcceleration, pty host from windowsUseConptyDll", () => {
  assert.equal(renderBackend({ value: "auto" }), "gpu_acceleration_auto");
  assert.equal(renderBackend(undefined).state, "unknown");
  assert.equal(settingPtyHostImpl({ value: true }), "bundled_openconsole");
  assert.equal(settingPtyHostImpl({ value: false }), "system_conhost");
  assert.equal(settingPtyHostImpl({ value: null }), null);
});

const P = (pid, ppid, name, startKey, cmdline = name) => [pid, { pid, ppid, name, argv: null, cmdline, startKey: String(startKey) }];

test("the pty-host census counts ConPTY hosts the host itself started, not ones under a tool it ran", () => {
  const table = new Map([
    P(100, 1, "Code.exe", 10),
    P(101, 100, "Code.exe", 11), // the pty host utility process
    P(102, 101, "OpenConsole.exe", 12),
    P(103, 101, "powershell.exe", 13),
    P(104, 100, "git.exe", 14),
    P(105, 104, "conhost.exe", 15), // a console tool's host: not the terminal's
    P(200, 1, "qontinui-runner.exe", 20),
    P(201, 200, "conhost.exe", 21),
  ]);
  assert.deepEqual(ptyHostCensus(table, [100]), { openconsole: 1, conhost: 0, roots: 1 });
  assert.equal(ptyHostFromCensus(ptyHostCensus(table, [100])), "bundled_openconsole");
  assert.equal(ptyHostFromCensus(ptyHostCensus(table, [200])), "system_conhost");
  assert.equal(ptyHostImpl({ platform: "linux", census: null }), "unix_pty");
  assert.equal(ptyHostImpl({ platform: "win32", census: { openconsole: 0, conhost: 0, roots: 1 }, settingImpl: "bundled_openconsole" }), "bundled_openconsole", "the setting when the census saw nothing");
  assert.equal(ptyHostImpl({ platform: "win32", census: { openconsole: 0, conhost: 2, roots: 1 }, settingImpl: "bundled_openconsole" }), "system_conhost", "the census wins over the setting");
  assert.equal(ptyHostImpl({ platform: "win32", census: null }).state, "unknown");
});

test("the runner's Windows rows read the same census: its portable-pty sessions run on the system conhost", async (t) => {
  const { RunnerAdapter } = await import("../adapters/runner.mjs");
  const root = tmp(t);
  const table = new Map([P(300, 1, "qontinui-runner.exe", 30), P(301, 300, "conhost.exe", 31), P(302, 300, "powershell.exe", 32), P(303, 1, "OpenConsole.exe", 33)]);
  const a = new RunnerAdapter({ mode: "subject", build: { role: "single", commit: "f".repeat(40), exe: "x" }, repoRoot: ".", platform: "win32", list: () => new Map(table) });
  a.pid = 300;
  a.root = root;
  assert.equal(a.describe(new Map()).pty_host_impl, "system_conhost", "an OpenConsole.exe that is not the runner's does not count");
  const linux = new RunnerAdapter({ mode: "subject", build: { role: "single", commit: "f".repeat(40), exe: "x" }, repoRoot: ".", platform: "linux", list: () => new Map() });
  linux.root = root;
  assert.equal(linux.describe(new Map()).pty_host_impl, "unix_pty");
  const blind = new RunnerAdapter({ mode: "subject", build: { role: "single", commit: "f".repeat(40), exe: "x" }, repoRoot: ".", platform: "win32", list: () => new Map([P(300, 1, "qontinui-runner.exe", 30)]) });
  blind.pid = 300;
  blind.root = root;
  assert.equal(blind.describe(new Map()).pty_host_impl.state, "unknown", "no ConPTY host seen: unknown, never assumed");
});

test("exclusivity: a descendant by parent links, each parent started first; a recycled link breaks the chain", () => {
  const table = new Map([P(100, 1, "Code.exe", 10), P(101, 100, "Code.exe", 11), P(102, 101, "Code.exe", 12), P(300, 99, "Code.exe", 5)]);
  assert.equal(isDescendantOf(table, 102, 100), true);
  assert.equal(isDescendantOf(table, 300, 100), false, "the operator's editor is not ours");
  const recycled = new Map([P(100, 1, "Code.exe", 50), P(101, 100, "Code.exe", 11)]);
  assert.equal(isDescendantOf(recycled, 101, 100), false, "a parent that started after its child is a recycled PID");
});

test("shell facts are measured from the session shell's command line", () => {
  const slots = new Map([[0, { slot: 0, pid: 501, ppid: 500 }]]);
  const plain = new Map([P(500, 101, "powershell.exe", 1, "powershell.exe -NoLogo -NoExit -Command & 'node.exe'")]);
  assert.deepEqual(shellFacts(slots, plain, "win32"), { session_shell: "powershell.exe -NoLogo -NoExit", shell_integration: "not_injected" });
  const injected = new Map([P(500, 101, "powershell.exe", 1, "powershell.exe -noexit -command . shellIntegration.ps1 -NoLogo")]);
  assert.equal(shellFacts(slots, injected, "win32").shell_integration, "injected");
  assert.equal(shellFacts(new Map(), plain, "win32"), null);
});

// ---------------------------------------------------------------------------
// The adapter end to end, through the real harness code
// ---------------------------------------------------------------------------

const MAIN = 900100;

/**
 * A "launch": the adapter's spawn is faked, and in its place the REAL harness
 * starts in this process over a simulated workbench, reading the config path the
 * adapter put in the child's environment. The process table holds the launched
 * main process, this process as its extension host, a pty host with its
 * OpenConsole, and a session shell.
 */
function launchRig(t, { platform = "win32", fakeOpts = {}, hostPid = process.pid, exitOnQuit = true, earlyExit = null } = {}) {
  const table = new Map([
    P(MAIN, 1, platform === "win32" ? "Code.exe" : "code", 1000),
    P(hostPid, MAIN, platform === "win32" ? "Code.exe" : "code", 1001, "--type=utility extensionHost"),
    P(900102, MAIN, platform === "win32" ? "Code.exe" : "code", 1002, "--type=utility ptyHost"),
    P(900103, 900102, "OpenConsole.exe", 1003),
    P(900104, 900102, platform === "win32" ? "powershell.exe" : "bash", 1004, platform === "win32" ? "powershell.exe -NoLogo -NoExit -Command & 'node.exe'" : "bash -i -c node"),
  ]);
  const rig = { table, spawns: [], kills: [], fake: null, harness: null, child: null };
  rig.list = () => new Map(table);
  rig.kill = (pid, sig) => {
    rig.kills.push([pid, sig]);
    table.delete(pid);
  };
  rig.spawnFn = (cmd, args, opts) => {
    rig.spawns.push({ cmd, args, opts });
    const child = new EventEmitter();
    child.pid = MAIN;
    rig.child = child;
    if (earlyExit !== null) {
      setTimeout(() => child.emit("exit", earlyExit, null), 20);
      return child;
    }
    // The simulated workbench runs on the settings file the adapter wrote.
    const userData = args[args.indexOf("--user-data-dir") + 1];
    const profile = JSON.parse(readFileSync(join(userData, "User", "settings.json"), "utf8"));
    rig.fake = createFakeVscode({
      ...fakeOpts,
      settings: { ...profile, ...(fakeOpts.settings ?? {}) },
      onQuit: () => {
        if (!exitOnQuit) return;
        setTimeout(async () => {
          table.delete(MAIN);
          await rig.harness?.dispose();
          child.emit("exit", 0, null);
        }, 10);
      },
    });
    startHarness({ vscode: rig.fake.vscode, env: opts.env, timing: { stepMs: 1500, processIdMs: 500, closeMs: 500 } }).then((h) => {
      rig.harness = h;
    });
    return child;
  };
  t.after(async () => {
    await rig.harness?.dispose();
  });
  return rig;
}

/** An installed app directory for `--vscode-app-path` (nothing in it ever runs). */
function installedApp(t, platform = "win32", extra = {}) {
  const dir = tmp(t, "tb-vsc-app-");
  writeFileSync(join(dir, platform === "win32" ? "Code.exe" : "code"), "not a binary");
  mkdirSync(join(dir, "04c0d99f4f", "resources", "app"), { recursive: true });
  writeFileSync(join(dir, "04c0d99f4f", "resources", "app", "product.json"), JSON.stringify({ version: VSCODE_PIN.version, commit: VSCODE_PIN.commit, ...extra }));
  return dir;
}

function winAdapter(rig, over = {}) {
  return new VscodeAdapter({
    platform: "win32",
    arch: "x64",
    env: { ...WIN_ENV, ELECTRON_RUN_AS_NODE: "1", VSCODE_IPC_HOOK_CLI: "x" },
    exists: (p) => p === PS || existsSync(p),
    spawnFn: rig.spawnFn,
    list: rig.list,
    kill: rig.kill,
    sleep: (ms) => new Promise((r) => setTimeout(r, Math.min(ms, 20))),
    launchTimeoutMs: 5000,
    quitTimeoutMs: 1000,
    paneSettleMs: 0,
    ...over,
  });
}

test("end to end (Windows, installed app): isolated launch, D13 sessions, both arms, switch, spawn/close, focus, describe, teardown", async (t) => {
  const { runDir, unitDir, genDir } = unitDirs(t);
  const rig = launchRig(t);
  const a = winAdapter(rig, { appPath: installedApp(t), cacheDir: tmp(t) });
  await a.launch({ runDir, unitDir, genDir, genRunId: "r1-u0", env: { env: {} } });

  // The isolated instance: argv, environment and private profile, all inside the unit.
  assert.equal(rig.spawns.length, 1);
  const { args, opts } = rig.spawns[0];
  const p = profileLayout(unitDir);
  assert.deepEqual(args.slice(0, 8), ["--user-data-dir", p.userData, "--extensions-dir", p.extensions, "--shared-data-dir", p.sharedData, "--extensionDevelopmentPath", HARNESS_EXTENSION_DIR]);
  assert.equal(opts.env.VSCODE_PORTABLE, p.portable);
  assert.equal(opts.env[protocol.CONFIG_ENV], p.config);
  assert.equal(opts.env.ELECTRON_RUN_AS_NODE, undefined);
  assert.equal(opts.env.VSCODE_IPC_HOOK_CLI, undefined);
  assert.equal(opts.windowsHide, false, "the window must be visible to be measured");
  assert.equal(opts.cwd, p.base);
  assert.deepEqual(JSON.parse(readFileSync(p.settings, "utf8")), vscodeSettings());
  assert.deepEqual(JSON.parse(readFileSync(p.argvJson, "utf8")), ARGV_JSON);
  const cfg = JSON.parse(readFileSync(p.config, "utf8"));
  assert.match(cfg.nonce, /^[0-9a-f]{64}$/);
  assert.equal(cfg.shellPath, PS);
  if (process.platform !== "win32") assert.equal(statSync(p.config).mode & 0o077, 0, "the nonce file is the user's alone");
  assert.equal(rig.fake.state.sidebar, false, "prepare closed the side bar");

  // D13 sessions.
  const argv = generatorArgv({ genDir, profile: "claude-tui", seed: 1, genRunId: "r1-u0" });
  const s = await a.spawnSessions(5, argv);
  assert.deepEqual(s.sessions.map((x) => x.hostId), ["vsc-1", "vsc-2", "vsc-3", "vsc-4", "vsc-5"]);
  assert.equal(s.admission, null);
  for (const term of rig.fake.state.created) {
    assert.equal(term.creationOptions.shellPath, PS);
    assert.deepEqual(term.creationOptions.shellArgs, d13Shell({ platform: "win32", argv, env: WIN_ENV }).shellArgs);
  }
  const slots = new Map([0, 1, 2, 3, 4].map((k) => [k, `vsc-${k + 1}`]));
  const sees = (ks) => async () => ({ ok: true, sessions: new Map(ks.map((k) => [k, { x: 0, y: 0, w: 160, h: 16 }])), detail: "fake" });

  // tiled: five panes in a 3+2 grid.
  // Each session reports its pane size: a start size, then the host's resize.
  for (const k of [0, 1, 2, 3, 4]) {
    const recs = [{ type: "start", session: k, terminal_size: { cols: 80, rows: 30 } }, { type: "resize", t_ns: "1", cols: 70 + k, rows: 20 }];
    writeFileSync(join(genDir, `gen-${k}.jsonl`), `${recs.map((r) => JSON.stringify(r)).join(String.fromCharCode(10))}${String.fromCharCode(10)}`);
  }
  const tiled = await a.arrange("tiled", { slots, n: 5, look: sees([0, 1, 2, 3, 4]) });
  assert.deepEqual(tiled.paneSizes, [0, 1, 2, 3, 4].map((k) => ({ cols: 70 + k, rows: 20 })), "the size each shown session last reported");
  assert.equal(tiled.ok, true, JSON.stringify(tiled));
  assert.deepEqual([tiled.vTarget, tiled.focusSession, tiled.switchable], [5, 0, []]);
  assert.deepEqual(tiled.layout.rows, [3, 2]);
  assert.deepEqual(rig.fake.where().groups, [["vsc-1"], ["vsc-2"], ["vsc-3"], ["vsc-4"], ["vsc-5"]]);

  // focus: back to panel tabs, verified by what the probe sees.
  const focus = await a.arrange("focus", { slots, n: 5, look: sees([0]) });
  assert.equal(focus.ok, true, JSON.stringify(focus));
  assert.deepEqual([focus.vTarget, focus.focusSession, focus.switchable], [1, 0, [0, 1, 2, 3, 4]]);
  assert.equal(rig.fake.state.panel.maximized, true);
  assert.deepEqual(focus.paneSizes, [{ cols: 70, rows: 20 }]);
  const wrong = await a.arrange("focus", { slots, n: 5, look: sees([0, 3]) });
  assert.deepEqual([wrong.ok, wrong.failure.reason], [false, "sessions_not_rendered"], "two panes on screen is not the focus arm");

  // switch, spawn/close, focus.
  const sw = await a.show(2);
  assert.equal(sw.ok, true);
  assert.equal(typeof sw.tCmdNs, "bigint");
  assert.equal(rig.fake.where().active, "vsc-3");
  const one = await a.spawnOne();
  assert.equal(one.ok, true);
  assert.equal(one.hostId, "vsc-6");
  assert.ok(rig.fake.where().panel.includes("vsc-6"), "the focus arm spawns a panel tab");
  assert.deepEqual(rig.fake.state.created.at(-1).creationOptions.shellArgs, rig.fake.state.created[0].creationOptions.shellArgs, "the same command line (D13)");
  assert.deepEqual(await a.closeOne(one.hostId), { ok: true });
  assert.ok(!rig.fake.where().panel.includes("vsc-6"));
  assert.deepEqual(await a.focusObserved(), { ok: true, session: 2 });

  // describe: measured where it can be, recorded where the plan says to record.
  writeFileSync(join(genDir, "slot-0"), `${JSON.stringify({ slot: 0, pid: 900105, ppid: 900104 })}\n`);
  const d = a.describe(new Map([[0, { slot: 0, pid: 900105, ppid: 900104 }]]));
  assert.equal(d.id, "vscode");
  assert.equal(d.launch_mode, "installed_app_path");
  assert.equal(d.pty_host_impl, "bundled_openconsole", "the census saw OpenConsole.exe under VS Code's pty host");
  assert.equal(d.render_backend, "gpu_acceleration_auto");
  assert.match(d.app_version, new RegExp(`^VS Code ${VSCODE_PIN.version.replace(/\./g, "\\.")} \\(04c0d99f4fb0, Electron`));
  assert.match(d.settings_digest, /^sha256:[0-9a-f]{64}$/);
  assert.equal(d.session_shell, "powershell.exe -NoLogo -NoExit");
  assert.equal(d.shell_integration, "not_injected");
  assert.deepEqual(d.build, { state: "not_applicable", reason: "not_applicable_to_setup" });
  assert.deepEqual(a.processRoots(), [MAIN]);
  assert.deepEqual(a.windowTarget(), { pid: MAIN });

  // teardown: terminals disposed, quit, then the run's own leftovers ended by PID + start key.
  const td = await a.teardown();
  assert.equal(rig.fake.state.quit, true);
  assert.ok(td.closed.some((c) => /5 terminal\(s\)/.test(c)), td.closed.join("; "));
  assert.ok(td.closed.some((c) => /quit/.test(c)));
  assert.deepEqual(rig.kills.map(([pid]) => pid), [900104, 900103, 900102], "children first; this process (the fake extension host) is never a target");
  assert.deepEqual(td.survivors, []);
});

test("Linux, pinned build: download verified, unpacked into the run, --no-sandbox when needed, the environment's DISPLAY; the next unit reuses both", async (t) => {
  const { runDir, unitDir, genDir } = unitDirs(t);
  const bytes = Buffer.from("a synthetic tarball");
  const build = { ...VSCODE_PIN.builds["linux-x64"], url: "https://example.invalid/vsc.tar.gz", fallbackUrl: null, archive: "vsc.tar.gz", sha256: sha(bytes) };
  const pin = { ...VSCODE_PIN, builds: { "linux-x64": build } };
  const cacheDir = tmp(t);
  let fetched = 0;
  const unpacks = [];
  const make = (rig) =>
    new VscodeAdapter({
      platform: "linux",
      arch: "x64",
      pin,
      cacheDir,
      env: { PATH: "/bin" },
      shell: "/bin/bash",
      exists: (p) => p === "/bin/bash" || existsSync(p),
      fetch: async () => {
        fetched += 1;
        return response(bytes);
      },
      run: fakeUnpack({ build, calls: unpacks }),
      sandbox: () => ({ state: "DEGRADED", reason: "fake: no userns", noSandbox: true }),
      spawnFn: rig.spawnFn,
      list: rig.list,
      kill: rig.kill,
      sleep: (ms) => new Promise((r) => setTimeout(r, Math.min(ms, 20))),
      launchTimeoutMs: 5000,
      quitTimeoutMs: 1000,
    });
  const rig = launchRig(t, { platform: "linux" });
  const a = make(rig);
  const facts = (await a.check()).mechanisms;
  assert.match(facts["setup.vscode"].reason, /not cached; the first launch downloads it/);
  assert.equal(facts["setup.vscode_sandbox"].state, "DEGRADED");
  await a.launch({ runDir, unitDir, genDir, genRunId: "r2-u0", env: { env: { DISPLAY: ":93" } } });
  assert.equal(rig.spawns[0].args.at(-1), "--no-sandbox");
  assert.equal(rig.spawns[0].opts.env.DISPLAY, ":93");
  assert.equal(rig.spawns[0].cmd, join(runDir, "vscode-app", `${VSCODE_PIN.version}-linux-x64`, "VSCode-linux-x64", "code"));
  assert.equal(a.describe().launch_mode, "portable_pinned");
  assert.equal(a.describe().pty_host_impl, "unix_pty");
  assert.deepEqual([fetched, unpacks.length], [1, 1]);
  assert.match((await a.check()).mechanisms["setup.vscode"].reason, /cached at .*SHA-256 verified/);
  await a.teardown();

  const unit2 = join(runDir, "units", "u001-vscode-default-none-n5-r0");
  mkdirSync(join(unit2, "gen"), { recursive: true });
  const rig2 = launchRig(t, { platform: "linux" });
  const b = make(rig2);
  await b.launch({ runDir, unitDir: unit2, genDir: join(unit2, "gen"), genRunId: "r2-u1", env: { env: { DISPLAY: ":93" } } });
  assert.deepEqual([fetched, unpacks.length], [1, 1], "the cached archive and the run's unpacked app are reused");
  await b.teardown();
});

test("a pinned launch refuses a mismatched archive before anything runs, and an instance reporting another version", async (t) => {
  const bytes = Buffer.from("the right bytes");
  const build = { ...VSCODE_PIN.builds["win32-x64"], url: "https://example.invalid/vsc.zip", fallbackUrl: null, archive: "vsc.zip", sha256: sha(bytes) };
  const pin = { ...VSCODE_PIN, builds: { "win32-x64": build } };
  {
    const { runDir, unitDir, genDir } = unitDirs(t);
    const rig = launchRig(t);
    const cacheDir = tmp(t);
    const a = winAdapter(rig, { pin, cacheDir, fetch: async () => response(Buffer.from("tampered bytes")), run: () => assert.fail("nothing is unpacked") });
    await assert.rejects(a.launch({ runDir, unitDir, genDir, genRunId: "r3-u0", env: { env: {} } }), (e) => {
      assert.ok(e instanceof AdapterRefusal);
      assert.equal(e.reason, "harness_error");
      assert.match(e.detail, /refusing the VS Code archive: SHA-256 mismatch/);
      return true;
    });
    assert.equal(rig.spawns.length, 0, "VS Code never starts from an unverified archive");
    assert.equal(existsSync(join(cacheDir, VSCODE_PIN.version, "vsc.zip")), false);
  }
  {
    const { runDir, unitDir, genDir } = unitDirs(t);
    const rig = launchRig(t, { fakeOpts: { version: "1.140.0" } });
    const a = winAdapter(rig, { pin, cacheDir: tmp(t), fetch: async () => response(bytes), run: fakeUnpack({ build }) });
    await assert.rejects(a.launch({ runDir, unitDir, genDir, genRunId: "r3-u1", env: { env: {} } }), /reports VS Code 1\.140\.0, not the pinned/);
    await a.teardown();
  }
});

test("exclusivity (D5): an extension host outside the launched tree, or a launch that hands off, is unknown(shared_process)", async (t) => {
  {
    const { runDir, unitDir, genDir } = unitDirs(t);
    const rig = launchRig(t);
    rig.table.set(process.pid, { pid: process.pid, ppid: 4242, name: "Code.exe", argv: null, cmdline: "", startKey: "1001" });
    const a = winAdapter(rig, { appPath: installedApp(t), cacheDir: tmp(t) });
    await assert.rejects(a.launch({ runDir, unitDir, genDir, genRunId: "r4-u0", env: { env: {} } }), (e) => e instanceof AdapterRefusal && e.reason === "shared_process");
    await a.teardown();
  }
  {
    const { runDir, unitDir, genDir } = unitDirs(t);
    const rig = launchRig(t, { earlyExit: 0 });
    const a = winAdapter(rig, { appPath: installedApp(t), cacheDir: tmp(t) });
    await assert.rejects(a.launch({ runDir, unitDir, genDir, genRunId: "r4-u1", env: { env: {} } }), (e) => e instanceof AdapterRefusal && e.reason === "shared_process" && /handed off/.test(e.detail));
  }
  {
    const { runDir, unitDir, genDir } = unitDirs(t);
    const rig = launchRig(t, { earlyExit: 3 });
    const a = winAdapter(rig, { appPath: installedApp(t), cacheDir: tmp(t) });
    await assert.rejects(a.launch({ runDir, unitDir, genDir, genRunId: "r4-u2", env: { env: {} } }), (e) => e instanceof AdapterRefusal && e.reason === "harness_error" && /exited/.test(e.detail));
  }
});

test("an unrealised arm is reported with its reason; an extension that never listens times out with one", async (t) => {
  {
    const { runDir, unitDir, genDir } = unitDirs(t);
    const rig = launchRig(t, { fakeOpts: { faults: { moveToEditorNoop: true } } });
    const a = winAdapter(rig, { appPath: installedApp(t), cacheDir: tmp(t) });
    await a.launch({ runDir, unitDir, genDir, genRunId: "r5-u0", env: { env: {} } });
    await a.spawnSessions(3, generatorArgv({ genDir, profile: "claude-tui", seed: 1, genRunId: "r5-u0" }));
    const r = await a.arrange("tiled", { slots: new Map([[0, "vsc-1"], [1, "vsc-2"], [2, "vsc-3"]]), n: 3 });
    assert.deepEqual([r.ok, r.failure.reason, r.vTarget], [false, "arm_not_realisable", 3]);
    await a.teardown();
  }
  {
    const { runDir, unitDir, genDir } = unitDirs(t);
    const rig = launchRig(t);
    const silent = (cmd, args, opts) => {
      const child = rig.spawnFn(cmd, args, { ...opts, env: { ...opts.env, [protocol.CONFIG_ENV]: "" } });
      return child;
    };
    const a = winAdapter(rig, { appPath: installedApp(t), cacheDir: tmp(t), spawnFn: silent, launchTimeoutMs: 400 });
    await assert.rejects(a.launch({ runDir, unitDir, genDir, genRunId: "r5-u1", env: { env: {} } }), /wrote no control file within/);
  }
});

test("the doctor's facts: an unsupported platform, the harness extension, the session shell, a cache inside a checkout", async (t) => {
  assert.equal((await new VscodeAdapter({ platform: "darwin" }).check()).mechanisms["setup.vscode"].state, "INOPERATIVE-ON-THIS-MACHINE");
  const repo = tmp(t);
  mkdirSync(join(repo, ".git"));
  const m = (await new VscodeAdapter({ platform: "win32", arch: "x64", env: WIN_ENV, cacheDir: join(repo, "c"), exists: (p) => p !== PS && existsSync(p) }).check()).mechanisms;
  assert.match(m["setup.vscode"].reason, /inside the git checkout/);
  assert.equal(m["setup.vscode"].state, "INOPERATIVE-ON-THIS-MACHINE");
  assert.equal(m["setup.vscode_extension"].state, "OPERATIVE", m["setup.vscode_extension"].reason);
  assert.match(m["setup.vscode_extension"].reason, /termhost-bench-harness@/);
  assert.equal(m["setup.vscode_shell"].state, "INOPERATIVE-ON-THIS-MACHINE");
  assert.ok(Object.keys(m).every((id) => id.startsWith("setup.")), "every VS Code fact is a setup.* mechanism, so the public redaction drops it (D7)");
  assert.equal((await new VscodeAdapter({ platform: "linux", arch: "arm64", cacheDir: tmp(t), shell: "/bin/sh" }).check()).mechanisms["setup.vscode"].state, "INOPERATIVE-ON-THIS-MACHINE", "no pinned build for linux-arm64");
});

// ---------------------------------------------------------------------------
// Review round 1
// ---------------------------------------------------------------------------

test("the private profile opens the panel un-maximized, so the focus arm's one toggle always maximizes", () => {
  assert.equal(vscodeSettings()["workbench.panel.opensMaximized"], "never");
});

test("an installed build with an install target runs without portable mode, and says so", async (t) => {
  const { runDir, unitDir, genDir } = unitDirs(t);
  const rig = launchRig(t);
  const app = installedApp(t, "win32", { target: "user" });
  const a = winAdapter(rig, { appPath: app, cacheDir: tmp(t) });
  assert.match((await a.check()).mechanisms["setup.vscode"].reason, /portable mode OFF \(product target "user"\).*argv\.json/);
  await a.launch({ runDir, unitDir, genDir, genRunId: "r6-u0", env: { env: {} } });
  const { args, opts } = rig.spawns[0];
  const p = profileLayout(unitDir);
  assert.equal(opts.env.VSCODE_PORTABLE, undefined, "VS Code would ignore it anyway");
  assert.equal(args[args.indexOf("--shared-data-dir") + 1], p.sharedData, "shared data is kept private explicitly");
  assert.equal(existsSync(p.argvJson), false, "no portable argv.json is pretended");
  assert.match(a.describe().app_version, /portable mode off \(product target "user"\)$/);
  await a.teardown();
  const plain = launchRig(t);
  const b = winAdapter(plain, { appPath: installedApp(t), cacheDir: tmp(t) });
  const u2 = join(runDir, "units", "u001");
  mkdirSync(join(u2, "gen"), { recursive: true });
  await b.launch({ runDir, unitDir: u2, genDir: join(u2, "gen"), genRunId: "r6-u1", env: { env: {} } });
  assert.equal(plain.spawns[0].opts.env.VSCODE_PORTABLE, profileLayout(u2).portable);
  assert.doesNotMatch(b.describe().app_version, /portable mode off/);
  await b.teardown();
});

test("a download is cut off once it passes the pinned size: refused, and the rest is never read", async (t) => {
  const dir = tmp(t);
  let pulled = 0;
  const chunk = Buffer.alloc(1024, 7);
  const fetch = async () => ({ ok: true, status: 200, body: (async function* () { for (let i = 0; i < 100; i += 1) { pulled += 1; yield chunk; } })() });
  await assert.rejects(downloadVerified({ urls: ["https://a/big"], dest: join(dir, "v", "a.zip"), expected: "0".repeat(64), fetch, maxBytes: 4096 }), (e) => e instanceof ArchiveIntegrityError && /more than 4096 bytes/.test(e.message));
  assert.ok(pulled <= 5, `stopped after the pinned size (${pulled} chunks pulled of 100)`);
  assert.deepEqual(readdirSync(join(dir, "v")), []);
});

test("a cached archive the doctor just hashed is not hashed again at launch", async (t) => {
  const { runDir, unitDir, genDir } = unitDirs(t);
  const bytes = Buffer.from("synthetic archive");
  const build = { ...VSCODE_PIN.builds["win32-x64"], url: "https://example.invalid/v.zip", fallbackUrl: null, archive: "v.zip", sha256: sha(bytes), bytes: bytes.length };
  const pin = { ...VSCODE_PIN, builds: { "win32-x64": build } };
  const cacheDir = tmp(t);
  mkdirSync(join(cacheDir, VSCODE_PIN.version));
  writeFileSync(join(cacheDir, VSCODE_PIN.version, "v.zip"), bytes);
  let hashes = 0;
  const hash = async (p) => {
    hashes += 1;
    return sha(readFileSync(p));
  };
  const rig = launchRig(t);
  const a = winAdapter(rig, { pin, cacheDir, hash, fetch: () => assert.fail("cached"), run: fakeUnpack({ build }) });
  await a.launch({ runDir, unitDir, genDir, genRunId: "r7-u0", env: { env: {} } });
  assert.equal(hashes, 1, "check() hashed it; ensureArchive reused that hash");
  await a.teardown();
  // A file that changed after the doctor's hash is hashed again.
  const changed = await ensureArchive({ build, cacheDir, fetch: async () => response(bytes), hash, verified: { path: join(cacheDir, VSCODE_PIN.version, "v.zip"), size: 1, mtimeMs: 0, sha256: build.sha256 } });
  assert.equal(changed.cached, true);
  assert.equal(hashes, 2);
});

test("pane sizes come from what each session reported: the latest resize, else its start size", async () => {
  const { latestPaneSize } = await import("../adapters/vscode.mjs");
  assert.deepEqual(latestPaneSize([{ type: "start", terminal_size: { cols: 80, rows: 30 } }]), { cols: 80, rows: 30 });
  assert.deepEqual(latestPaneSize([{ type: "start", terminal_size: { cols: 80, rows: 30 } }, { type: "resize", cols: 60, rows: 18 }, { type: "marker" }]), { cols: 60, rows: 18 });
  assert.equal(latestPaneSize([{ type: "start", terminal_size: null }]), null);
  const { watchResize, resizeRecord } = await import("../generator/ai-session.mjs");
  const { EventEmitter: E } = await import("node:events");
  const stdout = new E();
  const logged = [];
  let size = { cols: 100, rows: 40 };
  const stop = watchResize(stdout, (r) => logged.push(r), { nowNs: () => 5n }, () => size);
  stdout.emit("resize");
  size = null;
  stdout.emit("resize");
  stop();
  size = { cols: 1, rows: 1 };
  stdout.emit("resize");
  assert.deepEqual(logged, [resizeRecord({ cols: 100, rows: 40 }, 5n)]);
  assert.deepEqual(logged[0], { type: "resize", t_ns: "5", cols: 100, rows: 40 });
});
