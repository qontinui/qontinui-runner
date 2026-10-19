/**
 * The `windows-terminal` setup adapter (`adapters/windows-terminal.mjs`,
 * `adapters/windows-terminal-install.mjs`) without running Windows Terminal: the
 * pin and its SHA-256 refusal, the cache outside any repository, the unpack with
 * its `.portable` marker, the `--sessionId` check, the private settings, D13's
 * pane command, the grid and the tab/pane model (checked against a simulation of
 * the pinned source's rules), D5's new-process proof, and the whole launch ->
 * spawn -> arrange -> show -> spawn/close -> focus -> describe -> teardown flow
 * over a simulated instance — teardown closing the window it opened and never a
 * process by image name.
 *
 *   node --test scripts/termhost-bench/__tests__/windows-terminal-adapter.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative, sep, win32 } from "node:path";

import { AdapterRefusal } from "../adapters/index.mjs";
import { d13Shell } from "../adapters/vscode.mjs";
import { ArchiveIntegrityError, ensureArchive } from "../adapters/vscode-install.mjs";
import {
  GENERATOR_FLAG,
  HOST_ID_KEY,
  TILED_MAX_PANES,
  WINDOW_CLOSE,
  WINDOW_QUERY,
  WT_PROCESS_QUERY,
  encodePowerShell,
  powershellArgv,
  WindowsTerminalAdapter,
  WtLayout,
  assertWtSafe,
  gridColumns,
  gridCommands,
  newSessionId,
  paneCommand,
  parseWindows,
  parseWtProcesses,
  pickWindow,
  proveNewWtProcess,
  windowName,
  wtArgv,
} from "../adapters/windows-terminal.mjs";
import {
  BENCH_PROFILE,
  PORTABLE_MARKER,
  REQUIRED_FILES,
  SPAWNED_PROFILE,
  WT_HISTORY_SIZE,
  WT_PIN,
  defaultWtCacheDir,
  ensureWtUnpacked,
  sessionIdSupport,
  wtBuildFor,
  wtRenderBackend,
  wtSettings,
} from "../adapters/windows-terminal-install.mjs";
import { generatorArgv } from "../rundir.mjs";
import { createFakeWt, parseWtArgv } from "./helpers/fake-wt.mjs";

const sha = (b) => createHash("sha256").update(b).digest("hex");
const PS = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
const WIN_ENV = { SystemRoot: "C:\\Windows", PATH: "C:\\Windows\\System32", LOCALAPPDATA: "C:\\Users\\bench\\AppData\\Local" };

function tmp(t, prefix = "tb-wt-") {
  const d = mkdtempSync(join(tmpdir(), prefix));
  t.after(() => rmSync(d, { recursive: true, force: true }));
  return d;
}

function unitDirs(t, name = "u000-windows-terminal-default-none-n5-r0") {
  const runDir = tmp(t, "tb-wt-run-");
  const unitDir = join(runDir, "units", name);
  const genDir = join(unitDir, "gen");
  mkdirSync(genDir, { recursive: true });
  return { runDir, unitDir, genDir };
}

function response(bytes, status = 200) {
  return { ok: status >= 200 && status < 300, status, body: (async function* () { yield bytes.subarray(0, 5); yield bytes.subarray(5); })() };
}

/** An unpack that writes the pinned layout into the staging directory (no archive tool runs). */
function fakeUnpack({ build, calls = [], files = REQUIRED_FILES, sessionId = true }) {
  return (cmd, args, opts) => {
    calls.push({ cmd, args });
    const dest = cmd.endsWith("tar.exe") ? args[args.indexOf("-C") + 1] : opts?.env?.TERMHOST_UNPACK_DEST;
    if (!dest) throw new Error(`fakeUnpack: unexpected command ${cmd} ${args.slice(0, 3).join(" ")}`);
    const app = join(dest, build.topDir);
    mkdirSync(app, { recursive: true });
    for (const f of files) writeFileSync(join(app, f), f === "TerminalApp.dll" ? Buffer.from(sessionId ? "..--title\0--sessionId\0--tabColor.." : "..--title\0--tabColor..", "latin1") : "x");
    return { status: 0, stdout: "", stderr: "" };
  };
}

// ---------------------------------------------------------------------------
// The pin, the cache, the unpack
// ---------------------------------------------------------------------------

test("the pin: one stable release, its ZIP's SHA-256 and size, a URL that names that build", () => {
  assert.match(WT_PIN.version, /^\d+\.\d+\.\d+\.\d+$/);
  assert.equal(WT_PIN.tag, `v${WT_PIN.version}`);
  const b = wtBuildFor("win32", "x64");
  assert.ok(b, "an x64 Windows build is pinned");
  assert.match(b.sha256, /^[0-9a-f]{64}$/);
  assert.ok(Number.isInteger(b.bytes) && b.bytes > 1_000_000);
  assert.equal(b.kind, "zip");
  assert.equal(b.archive, `Microsoft.WindowsTerminal_${WT_PIN.version}_x64.zip`);
  assert.equal(b.url, `https://github.com/microsoft/terminal/releases/download/${WT_PIN.tag}/${b.archive}`);
  assert.equal(b.topDir, `terminal-${WT_PIN.version}`);
  assert.equal(wtBuildFor("linux", "x64"), null, "no Linux build: Windows Terminal is Windows-only");
});

test("the cache defaults outside any checkout, under the user's local app data", () => {
  assert.equal(defaultWtCacheDir({ platform: "win32", env: WIN_ENV }), "C:\\Users\\bench\\AppData\\Local\\termhost-bench\\cache\\windows-terminal");
});

test("a download whose SHA-256 differs from the pin is refused, nothing kept, named as Windows Terminal's", async (t) => {
  const cacheDir = tmp(t);
  const build = { ...wtBuildFor("win32", "x64"), url: "https://example.invalid/wt.zip", sha256: sha(Buffer.from("other")), bytes: 100 };
  await assert.rejects(
    ensureArchive({ build, cacheDir, version: "9", fetch: async () => response(Buffer.from("wrong bytes")), what: "Windows Terminal" }),
    (e) => e instanceof ArchiveIntegrityError && /SHA-256 mismatch/.test(e.message),
  );
  assert.ok(!existsSync(join(cacheDir, "9", build.archive)));
  await assert.rejects(
    ensureArchive({ build: { ...build, sha256: sha(Buffer.from("x")) }, cacheDir, version: "9", fetch: async () => response(Buffer.from("x"), 404), what: "Windows Terminal" }),
    /could not download the pinned Windows Terminal archive/,
  );
});

test("unpacking: a new directory of the run with .portable beside WindowsTerminal.exe; refused without the build's files; reused by later units", async (t) => {
  const { runDir } = unitDirs(t);
  const archive = join(tmp(t), "wt.zip");
  writeFileSync(archive, "zip");
  const build = wtBuildFor("win32", "x64");
  const calls = [];
  const app = await ensureWtUnpacked({ archive, build, runDir, run: fakeUnpack({ build, calls }), platform: "win32", env: WIN_ENV });
  assert.equal(app, join(runDir, "wt-app", `${WT_PIN.version}-win32-x64`, build.topDir));
  assert.ok(existsSync(join(app, PORTABLE_MARKER)), "portable mode: settings never reach the operator's %LOCALAPPDATA%");
  assert.equal(await ensureWtUnpacked({ archive, build, runDir, run: fakeUnpack({ build, calls }), platform: "win32", env: WIN_ENV }), app);
  assert.equal(calls.length, 1, "unpacked once per run");
  const { runDir: other } = unitDirs(t);
  await assert.rejects(
    ensureWtUnpacked({ archive, build, runDir: other, run: fakeUnpack({ build, files: ["wt.exe"] }), platform: "win32", env: WIN_ENV }),
    /without WindowsTerminal\.exe, OpenConsole\.exe, TerminalApp\.dll/,
  );
});

test("--sessionId support is read from TerminalApp.dll's bytes, never by running wt", (t) => {
  const d = tmp(t);
  writeFileSync(join(d, "TerminalApp.dll"), Buffer.from("x--title\0--sessionId\0", "latin1"));
  assert.equal(sessionIdSupport(d).supported, true);
  writeFileSync(join(d, "TerminalApp.dll"), Buffer.from("x--title\0--tabColor\0", "latin1"));
  assert.equal(sessionIdSupport(d).supported, false);
  assert.equal(sessionIdSupport(join(d, "none")).supported, null, "unreadable is unknown, not unsupported");
});

// ---------------------------------------------------------------------------
// Settings, the pane command, the grid
// ---------------------------------------------------------------------------

test("private settings: opacity 100, a fixed font size, closeOnExit graceful, no dialogs, and the renderer left at its default", () => {
  const s = wtSettings({ shellPath: PS });
  const [bench, spawned] = s.profiles.list;
  assert.equal(s.defaultProfile, bench.guid);
  assert.deepEqual([bench.name, spawned.name], [BENCH_PROFILE, SPAWNED_PROFILE]);
  for (const p of [bench, spawned]) {
    assert.equal(p.opacity, 100);
    assert.equal(p.useAcrylic, false);
    assert.equal(p.font.size, 10, "one font size on every machine");
    assert.equal(p.historySize, WT_HISTORY_SIZE);
    assert.equal(p.commandline, `${PS} -NoLogo -NoExit`);
  }
  assert.equal(bench.closeOnExit, "graceful");
  assert.equal(spawned.closeOnExit, "always", "a spawned pane closes when closeOne ends its shell");
  assert.equal(s.confirmCloseAllTabs, false);
  assert.equal(s["warning.confirmCloseAllTabs"], false);
  assert.equal(s["compatibility.allowHeadless"], false, "the process exits with its last window");
  assert.equal(s.firstWindowPreference, "defaultProfile", "no restored layout");
  assert.equal(s.launchMode, "maximized");
  assert.ok(!Object.keys(s).some((k) => k.startsWith("rendering.")), "rendering is left at Windows Terminal's default");
  assert.equal(wtRenderBackend(s), "atlas_graphics_api_automatic");
  assert.equal(wtRenderBackend({ "rendering.graphicsAPI": "direct3d11", "rendering.software": true }), "atlas_graphics_api_direct3d11_software");
  assert.throws(() => wtSettings({}), /session shell/);
});

test("D13: the pane runs Windows PowerShell -NoLogo -NoExit -Command with the same line VS Code and the runner get", () => {
  const argv = generatorArgv({ node: "C:\\node\\node.exe", script: "C:\\b\\ai-session.mjs", genDir: "C:\\Temp\\run\\gen", profile: "claude-tui", seed: 7, genRunId: "r1-u0" });
  const cmd = paneCommand(argv, { shellPath: PS });
  const vsc = d13Shell({ platform: "win32", argv, env: { SystemRoot: "C:\\Windows" } });
  assert.deepEqual(cmd, [vsc.shellPath, ...vsc.shellArgs]);
  assert.deepEqual(cmd.slice(0, 4), [PS, "-NoLogo", "-NoExit", "-Command"]);
  assert.throws(() => assertWtSafe(["a;b"]), (e) => e instanceof AdapterRefusal && /splits commands/.test(e.detail));
  assert.throws(() => assertWtSafe(['a"b']), /does not escape/);
  assert.throws(() => paneCommand([...argv.slice(0, 3), "C:\\Temp\\a;b"], { shellPath: PS }), /splits commands/);
});

test("session ids: a {guid} argument, and the bare lowercase GUID WT_SESSION carries", () => {
  const s = newSessionId(() => "1B4E28BA-2FA1-11D2-883F-0016D3CCA427");
  assert.deepEqual(s, { arg: "{1b4e28ba-2fa1-11d2-883f-0016d3cca427}", env: "1b4e28ba-2fa1-11d2-883f-0016d3cca427" });
  assert.throws(() => newSessionId(() => "nope"), /not a GUID/);
  assert.equal(windowName("r1-u3"), "termhost-r1-u3");
});

test("the grid: at most three columns of near-equal panes, for every N up to nine", () => {
  assert.deepEqual(gridColumns(1), [1]);
  assert.deepEqual(gridColumns(2), [1, 1]);
  assert.deepEqual(gridColumns(4), [2, 2]);
  assert.deepEqual(gridColumns(5), [2, 2, 1]);
  assert.deepEqual(gridColumns(9), [3, 3, 3]);
  assert.throws(() => gridColumns(10), /1\.\.9/);
  assert.equal(TILED_MAX_PANES, 9);
});

/** Run gridCommands through the simulator; each pane's rectangle by session index. */
function simulateGrid(t, v) {
  const appDir = tmp(t);
  const wt = createFakeWt({ appDir, operator: false });
  const panes = Array.from({ length: v }, (_, i) => ({ title: `s${i}`, sessionId: `{00000000-0000-0000-0000-00000000000${i}}`, cmd: ["pwsh"] }));
  const g = gridCommands(panes);
  wt.run(join(appDir, "wt.exe"), wtArgv("w", g.commands, { global: ["-M"] }));
  const tab = wt.layout()[0];
  return { g, tab, panes };
}

test("the grid's commands, run under the pinned source's split rules, give every pane 1/cols x 1/rows, with the ids the model predicts", (t) => {
  for (const v of [1, 2, 3, 5, 9]) {
    const { g, tab } = simulateGrid(t, v);
    assert.equal(tab.length, v, `v=${v}: every pane in tab 0`);
    const cols = gridColumns(v);
    for (let i = 0; i < v; i += 1) {
      const pane = tab.find((p) => p.sid === `00000000-0000-0000-0000-00000000000${i}`);
      assert.equal(pane.id, g.ids[i], `v=${v}: pane ${i}'s id`);
      const col = cols.findIndex((_, ci) => cols.slice(0, ci + 1).reduce((a, b) => a + b, 0) > i);
      assert.ok(Math.abs(pane.rect.w - 1920 / cols.length) < 1, `v=${v}: pane ${i} width ${pane.rect.w}`);
      assert.ok(Math.abs(pane.rect.h - 1080 / cols[col]) < 1, `v=${v}: pane ${i} height ${pane.rect.h}`);
    }
  }
});

test("the grid can fail: a build that ignored focus-pane would stack every split into the last column", (t) => {
  const appDir = tmp(t);
  const wt = createFakeWt({ appDir, operator: false, faults: { ignoreFocusPane: true } });
  const panes = Array.from({ length: 9 }, (_, i) => ({ title: `s${i}`, sessionId: `{00000000-0000-0000-0000-00000000000${i}}`, cmd: ["pwsh"] }));
  wt.run(join(appDir, "wt.exe"), wtArgv("w", gridCommands(panes).commands));
  const heights = wt.layout()[0].map((p) => Math.round(p.rect.h));
  assert.notDeepEqual(new Set(heights), new Set([360]), "the equal-grid check above would catch it");
});

test("wt.exe argv: the window, global flags, and subcommands separated by a lone ';' (parsed back by the simulator)", () => {
  const argv = wtArgv("termhost-r1", [["focus-tab", "-t", "0"], ["focus-pane", "-t", "2"]], { global: ["-M"] });
  assert.deepEqual(argv, ["-w", "termhost-r1", "-M", "focus-tab", "-t", "0", ";", "focus-pane", "-t", "2"]);
  const p = parseWtArgv(argv);
  assert.equal(p.window, "termhost-r1");
  assert.deepEqual(p.global, ["-M"]);
  assert.deepEqual(p.commands.map((c) => [c.name, c.opts["-t"]]), [["focus-tab", "0"], ["focus-pane", "2"]]);
});

test("the tab/pane model follows the simulator through focus -> tiled -> focus and spawned panes", (t) => {
  const appDir = tmp(t);
  const wt = createFakeWt({ appDir, operator: false });
  const panes = Array.from({ length: 12 }, (_, i) => ({ title: `s${i}`, sessionId: `{00000000-0000-0000-0000-0000000000${String(i).padStart(2, "0")}}`, cmd: ["pwsh"] }));
  const sid = (i) => `00000000-0000-0000-0000-0000000000${String(i).padStart(2, "0")}`;
  const g = gridCommands(panes.slice(0, 9));
  const exe = join(appDir, "wt.exe");
  wt.run(exe, wtArgv("w", g.commands));
  wt.run(exe, wtArgv("w", panes.slice(9).map((p) => ["new-tab", "--sessionId", p.sessionId, ...p.cmd])));
  const model = WtLayout.afterSpawn(panes.slice(0, 9).map((_, i) => sid(i)), g.ids, [9, 10, 11].map(sid));
  const same = (what) => {
    const sim = wt.layout().map((tab) => tab.map((p) => `${p.id}:${p.sid}`).sort());
    const mod = model.tabs.map((tab) => tab.panes.map((p) => `${p.id}:${p.hostId}`).sort());
    assert.deepEqual(mod, sim, what);
  };
  same("after the spawn");
  assert.equal(model.shape, "tiled");
  wt.run(exe, wtArgv("w", model.toFocus()));
  same("after the focus arrangement");
  assert.equal(model.shape, "focus");
  assert.equal(wt.layout().length, 12, "one tab per session");
  wt.run(exe, wtArgv("w", model.toTiled([3, 0, 7, 1].map(sid))));
  same("after re-tiling four sessions");
  assert.equal(wt.layout()[model.tabOf(sid(3))].length, 4);
  const tabs = model.tabs.length;
  wt.run(exe, wtArgv("w", [["new-tab", "--sessionId", "{00000000-0000-0000-0000-0000000000aa}", "pwsh"]]));
  assert.equal(model.addTab("00000000-0000-0000-0000-0000000000aa"), tabs);
  wt.run(exe, wtArgv("w", [["focus-tab", "-t", "0"], ["split-pane", "--sessionId", "{00000000-0000-0000-0000-0000000000bb}", "pwsh"]]));
  model.addPane(0, "00000000-0000-0000-0000-0000000000bb");
  same("after a spawned tab and a spawned pane");
  model.remove("00000000-0000-0000-0000-0000000000aa");
  assert.equal(model.tabs.length, tabs, "a tab whose last pane closed is gone");
  assert.equal(model.tabOf("00000000-0000-0000-0000-0000000000aa"), -1);
});

// ---------------------------------------------------------------------------
// D5: the new-process proof, and the read-only scripts
// ---------------------------------------------------------------------------

test("D5: the window's WindowsTerminal.exe must be new and run from the run's copy; the operator's same-named process is never chosen", () => {
  const exePath = "C:\\Temp\\run\\wt-app\\x\\terminal\\WindowsTerminal.exe";
  const operator = { pid: 100, exe: "C:\\Program Files\\WindowsApps\\WT\\WindowsTerminal.exe", startKey: "5" };
  const ours = { pid: 200, exe: exePath, startKey: "9" };
  const handoff = { pid: 210, exe: exePath.toUpperCase(), startKey: "12" };
  assert.deepEqual(proveNewWtProcess({ before: [operator], after: [operator, handoff, ours], exePath }), { ok: true, pid: 200, startKey: "9" }, "the earliest new one is the instance");
  const pre = proveNewWtProcess({ before: [operator, ours], after: [operator, ours], exePath });
  assert.deepEqual([pre.ok, pre.reason], [false, "shared_process"]);
  const none = proveNewWtProcess({ before: [operator], after: [operator], exePath });
  assert.deepEqual([none.ok, none.reason], [false, "harness_error"]);
  const recycled = proveNewWtProcess({ before: [{ ...ours, startKey: "1" }], after: [ours], exePath });
  assert.equal(recycled.ok, true, "a PID that was reused is a new process (its start key differs)");
});

test("the process and window queries read; only WINDOW_CLOSE posts a message, after re-checking owner and class", () => {
  for (const q of [WT_PROCESS_QUERY, WINDOW_QUERY]) {
    assert.doesNotMatch(q, /PostMessage\(|Stop-|Set-|Remove-|Kill\(|Terminate/i);
  }
  assert.match(WT_PROCESS_QUERY, /ExecutablePath/);
  assert.match(WINDOW_CLOSE, /Owner\(\$h\) -eq \[uint32\]\$env:TB_WT_PID -and \[TbWtWin\]::Cls\(\$h\) -eq 'CASCADIA_HOSTING_WINDOW_CLASS'/);
  assert.match(WINDOW_CLOSE, /\[TbWtPost\]::PostMessage\(\$h, 0x0010/, "WM_CLOSE");
  assert.match(WINDOW_CLOSE, /\$env:TB_WT_HWND/, "inputs arrive in the environment, never spliced");
  assert.match(WINDOW_CLOSE, /CreationDate\.ToFileTimeUtc\(\) -eq \$env:TB_WT_START/, "the PID must still be the proven process");
  assert.match(WINDOW_CLOSE, /if \(\$same -and /, "and the post is conditional on it");
  assert.deepEqual(parseWtProcesses('{"p":5,"pp":4,"x":"C:\\\\a\\\\WindowsTerminal.exe","t":"7"}'), [{ pid: 5, ppid: 4, exe: "C:\\a\\WindowsTerminal.exe", startKey: "7" }]);
  assert.deepEqual(parseWtProcesses(""), []);
  const w = parseWindows("123\t0\tCASCADIA_HOSTING_WINDOW_CLASS\tx\r\n456\t1\tCASCADIA_HOSTING_WINDOW_CLASS\ts0\njunk");
  assert.equal(w.length, 2);
  assert.equal(pickWindow(w).hwnd, 456, "the visible one");
  assert.equal(pickWindow([{ hwnd: 1, visible: true, cls: "Other", title: "" }]), null);
});

// ---------------------------------------------------------------------------
// The adapter end to end, over the simulator
// ---------------------------------------------------------------------------

function rig(t, { faults = {}, sessionId = true, operator = true, dirs = unitDirs(t), wtSim = null, genRunId = "r1-u0" } = {}) {
  const bytes = Buffer.from("a synthetic Windows Terminal zip");
  const build = { ...wtBuildFor("win32", "x64"), url: "https://example.invalid/wt.zip", sha256: sha(bytes), bytes: bytes.length };
  const pin = { ...WT_PIN, builds: { "win32-x64": build } };
  const appDir = join(dirs.runDir, "wt-app", `${WT_PIN.version}-win32-x64`, build.topDir);
  const wt = wtSim ?? createFakeWt({ appDir, genDir: dirs.genDir, genRunId, faults, operator });
  const unpack = fakeUnpack({ build, sessionId });
  let n = 0;
  const adapter = new WindowsTerminalAdapter({
    platform: "win32",
    arch: "x64",
    env: WIN_ENV,
    pin,
    cacheDir: tmp(t),
    fetch: async () => response(bytes),
    run: (cmd, args, opts) => (String(cmd).endsWith("wt.exe") ? wt.run(cmd, args, opts) : unpack(cmd, args, opts)),
    exists: (p) => p === PS || existsSync(p),
    list: wt.list,
    kill: wt.kill,
    queryWt: wt.queryWt,
    queryWindows: wt.queryWindows,
    postClose: wt.postClose,
    sleep: (ms) => new Promise((r) => setTimeout(r, Math.min(ms, 5))),
    uuid: () => `00000000-0000-4000-8000-${String(++n).padStart(12, "0")}`,
    launchTimeoutMs: 2000,
    closeTimeoutMs: 50,
    generatorWaitMs: 50,
  });
  const argv = generatorArgv({ node: "C:\\node\\node.exe", script: "C:\\b\\ai-session.mjs", genDir: "C:\\Temp\\gen", profile: "claude-tui", seed: 1, genRunId });
  return { ...dirs, wt, adapter, argv, ctx: { ...dirs, genRunId } };
}

const slotMap = (wt, sessions) => new Map(sessions.map((s) => [wt.sessions.find((x) => x.sid === s.hostId).slot, s.hostId]));

test("end to end: portable launch proven new, D13 panes with session ids, tiled grid, focus tabs, switch, spawn/close, focus, describe, teardown by WM_CLOSE", async (t) => {
  const r = rig(t);
  const facts = (await r.adapter.check()).mechanisms;
  assert.match(facts["setup.windows_terminal"].reason, /not cached; the first launch downloads it/);
  assert.equal(facts["setup.windows_terminal_shell"].state, "OPERATIVE");
  await r.adapter.launch(r.ctx);
  const settings = JSON.parse(readFileSync(join(r.wt.exe, "..", "settings", "settings.json"), "utf8"));
  assert.equal(settings.profiles.list[0].closeOnExit, "graceful");
  const spawned = await r.adapter.spawnSessions(5, r.argv);
  assert.equal(spawned.sessions.length, 5);
  assert.equal(r.adapter.wt.pid, r.wt.state.pid, "the proven instance");
  assert.notEqual(r.adapter.wt.pid, r.wt.operatorPid);
  assert.equal(r.wt.state.maximized, true, "-M on the launch");
  const first = parseWtArgv(r.wt.invocations[0]);
  assert.equal(first.window, "termhost-r1-u0");
  assert.equal(first.commands[0].name, "new-tab");
  assert.equal(first.commands[0].opts["-p"], BENCH_PROFILE);
  assert.match(first.commands[0].opts["--sessionId"], /^\{[0-9a-f-]{36}\}$/);
  assert.deepEqual(first.commands[0].cmd.slice(0, 4), [PS, "-NoLogo", "-NoExit", "-Command"]);
  assert.equal(new Set(r.wt.sessions.map((s) => s.cmd.at(-1))).size, 1, "D13: one identical command line in every pane");
  // The run loop maps slots to panes through WT_SESSION.
  for (const s of spawned.sessions) assert.ok(r.wt.sessions.some((x) => x.sid === s.hostId), `${s.hostId} is a WT_SESSION`);
  assert.equal(r.adapter.hostIdKey, HOST_ID_KEY);
  const slots = slotMap(r.wt, spawned.sessions);
  const order = [...slots.keys()].sort((a, b) => a - b);

  const tiled = await r.adapter.arrange("tiled", { slots, n: 5 });
  assert.equal(tiled.ok, true);
  assert.equal(tiled.vTarget, 5);
  assert.equal(r.wt.layout()[0].length, 5, "the five panes share tab 0");
  assert.equal(r.wt.state.active, 0);

  const looks = [];
  const focus = await r.adapter.arrange("focus", { slots, n: 5, look: async () => (looks.push(1), { ok: true, sessions: new Map([[order[0], {}]]), detail: "" }) });
  assert.equal(focus.ok, true, JSON.stringify(focus.failure));
  assert.deepEqual([focus.vTarget, focus.focusSession], [1, order[0]]);
  assert.equal(looks.length, 1, "the arrangement is read back through the probe");
  assert.equal(r.wt.layout().length, 5, "one tab per session");
  const shownTab = r.wt.state.active;
  assert.equal(r.wt.layout()[shownTab][0].sid, slots.get(order[0]));

  const sw = await r.adapter.show(order[3]);
  assert.equal(sw.ok, true);
  assert.equal(r.wt.layout()[r.wt.state.active][0].sid, slots.get(order[3]), "focus-tab showed session k");
  assert.equal(typeof sw.tCmdNs, "bigint");

  const one = await r.adapter.spawnOne();
  assert.equal(one.ok, true);
  assert.equal(r.wt.layout().length, 6);
  const sp = parseWtArgv(r.wt.invocations.at(-1)).commands[0];
  assert.deepEqual([sp.name, sp.opts["-p"]], ["new-tab", SPAWNED_PROFILE]);
  const closed = await r.adapter.closeOne(one.hostId);
  assert.equal(closed.ok, true, closed.detail);
  assert.equal(r.wt.layout().length, 5, "the spawned pane's shell ended by PID, its pane closed");
  const spawnedShell = r.wt.sessions.find((s) => s.sid === one.hostId).shell;
  assert.ok(r.wt.signalled.some(([pid]) => pid === spawnedShell));

  const f = await r.adapter.focusObserved();
  assert.deepEqual(f, { ok: true, session: order[3] });

  assert.deepEqual(r.adapter.processRoots(), [r.wt.state.pid]);
  assert.deepEqual(r.adapter.windowTarget(), { hwnd: r.wt.state.hwnd });
  const d = r.adapter.describe(new Map());
  assert.equal(d.id, "windows-terminal");
  assert.equal(d.launch_mode, "portable_pinned");
  assert.equal(d.pty_host_impl, "bundled_openconsole", "measured: an OpenConsole.exe under the instance");
  assert.equal(d.render_backend, "atlas_graphics_api_automatic");
  assert.match(d.settings_digest, /^sha256:[0-9a-f]{64}$/);
  assert.match(d.app_version, new RegExp(`Windows Terminal ${WT_PIN.version.replace(/\./g, "\\.")} \\(portable; --sessionId accepted\\)`));

  r.wt.endGenerators();
  const signalledBefore = r.wt.signalled.length;
  const td = await r.adapter.teardown();
  assert.deepEqual(r.wt.posted, [[r.adapter.wt.pid, r.wt.state.hwnd]], "WM_CLOSE to the window this run opened, and only it");
  assert.equal(r.wt.signalled.length, signalledBefore, "a window that closes needs no signal");
  assert.deepEqual(td.survivors, []);
  assert.ok(td.closed.some((c) => /exited/.test(c)));
  assert.ok(td.closed.some((c) => c === `WM_CLOSE to window ${r.wt.state.hwnd}`), "the close is recorded");
  assert.ok(r.wt.procs.has(r.wt.operatorPid), "the operator's WindowsTerminal.exe is untouched");
});

test("teardown never kills by image name: a window that ignores WM_CLOSE is ended by its proven PID; the operator's process is never signalled", async (t) => {
  const r = rig(t, { faults: { ignoreClose: true } });
  await r.adapter.launch(r.ctx);
  await r.adapter.spawnSessions(2, r.argv);
  r.wt.endGenerators();
  const td = await r.adapter.teardown();
  const pids = r.wt.signalled.map(([p]) => p);
  assert.ok(pids.includes(r.adapter.wt.pid), "the proven instance, by PID");
  assert.ok(!pids.includes(r.wt.operatorPid), "never the same-named operator process");
  assert.deepEqual(td.survivors, []);
});

test("a survivor is reported, and a recycled PID is never signalled", async (t) => {
  const r = rig(t, { faults: { ignoreClose: true, unkillable: ["WindowsTerminal.exe"] } });
  await r.adapter.launch(r.ctx);
  await r.adapter.spawnSessions(1, r.argv);
  r.wt.endGenerators();
  const td = await r.adapter.teardown();
  assert.ok(td.survivors.some((s) => s.pid === r.adapter.wt.pid), "the instance that would not exit is named");

  const q = rig(t, { faults: { ignoreClose: true } });
  await q.adapter.launch(q.ctx);
  await q.adapter.spawnSessions(1, q.argv);
  // The instance exits and a stranger takes its PID before teardown.
  const pid = q.adapter.wt.pid;
  q.wt.procs.set(pid, { ...q.wt.procs.get(pid), startKey: "999999999", exe: "C:\\other.exe" });
  q.wt.endGenerators();
  const posted = q.wt.posted.length;
  await q.adapter.teardown();
  assert.ok(!q.wt.signalled.some(([p]) => p === pid), "a PID whose start key changed is not ours");
  assert.equal(q.wt.posted.length, posted, "no WM_CLOSE to a window whose PID is a stranger's");
});

test("D5: a launch whose command line was handed to a Windows Terminal that is not this run's never yields a window to measure", async (t) => {
  const r = rig(t, { faults: { handoffToOperator: true } });
  await r.adapter.launch(r.ctx);
  await assert.rejects(r.adapter.spawnSessions(1, r.argv), (e) => e instanceof AdapterRefusal && e.reason === "harness_error" && /no window/.test(e.detail));
});

test("an earlier unit's instance that survived teardown: the next unit refuses BEFORE invoking wt.exe, with an incident naming it", async (t) => {
  const first = rig(t, { faults: { ignoreClose: true, unkillable: ["WindowsTerminal.exe"] } });
  await first.adapter.launch(first.ctx);
  await first.adapter.spawnSessions(1, first.argv);
  first.wt.endGenerators();
  const td = await first.adapter.teardown();
  const survivor = first.adapter.wt.pid;
  assert.ok(td.survivors.some((x) => x.pid === survivor));
  // Unit 2 of the same run: the same unpacked copy, so the same single-instance key.
  const unit2 = join(first.runDir, "units", "u001-windows-terminal-default-none-n5-r0");
  mkdirSync(join(unit2, "gen"), { recursive: true });
  const second = rig(t, { dirs: { runDir: first.runDir, unitDir: unit2, genDir: join(unit2, "gen") }, wtSim: first.wt, genRunId: "r1-u1" });
  await second.adapter.launch(second.ctx);
  const invocations = first.wt.invocations.length;
  await assert.rejects(second.adapter.spawnSessions(1, second.argv), (e) => {
    assert.ok(e instanceof AdapterRefusal, String(e));
    assert.equal(e.reason, "shared_process");
    assert.match(e.detail, new RegExp(`pid ${survivor} is already running from`));
    assert.equal(e.incident?.kind, "harness_error");
    assert.deepEqual(e.incident.data.survivors, [survivor]);
    assert.equal(e.incident.data.unit, "u001-windows-terminal-default-none-n5-r0", "the unit it refused");
    assert.equal(e.incident.data.setup, "windows-terminal");
    return true;
  });
  assert.equal(first.wt.invocations.length, invocations, "wt.exe was never invoked: the survivor received no panes");
  assert.equal(first.wt.sessions.length, 1, "no pane of unit 2 exists anywhere");
});

test("one rule for harness PowerShell: every script travels -EncodedCommand; the only -Command left is the D13 session shell's", async () => {
  const { WINDOWS_PROCESS_QUERY, readWindowsProcessTable } = await import("../proc.mjs");
  const { GPU_QUERY, detectGpu } = await import("../machine.mjs");
  const { SCREEN_QUERY, readScreen } = await import("../envs/windows-hosted.mjs");
  const { VM_PROCESS_QUERY, VM_SAMPLER_SCRIPT } = await import("../wsl.mjs");
  const { unpackCommand } = await import("../adapters/vscode-install.mjs");
  for (const script of [WINDOWS_PROCESS_QUERY, GPU_QUERY, SCREEN_QUERY, VM_PROCESS_QUERY, VM_SAMPLER_SCRIPT]) {
    assert.equal(Buffer.from(powershellArgv(script)[3], "base64").toString("utf16le"), script);
  }
  const seen = [];
  const run = (cmd, args) => (seen.push([cmd, ...args]), { status: 0, stdout: "[]" });
  readWindowsProcessTable({ run });
  readScreen({ run: (cmd, args) => (seen.push([cmd, ...args]), { status: 0, stdout: "{}" }) });
  detectGpu({ platform: "win32", run: (cmd, args) => (seen.push([cmd, ...args]), { status: 0, stdout: "[]" }) });
  assert.equal(seen.length, 3);
  const ps = unpackCommand({ kind: "zip", archive: "C:\\a.zip", dest: "C:\\d", platform: "win32", env: { SystemRoot: "C:\\Windows" }, exists: () => false });
  seen.push([ps.cmd, ...ps.args]);
  for (const call of seen) {
    assert.equal(call[0], "powershell.exe");
    assert.deepEqual(call.slice(1, 4), ["-NoProfile", "-NonInteractive", "-EncodedCommand"], call.slice(0, 4).join(" "));
  }
});

/**
 * Every argv element that PowerShell would read as -Command: the parameter is
 * case-insensitive and takes any unambiguous prefix (`-c`, `-co`, … `-command`),
 * so match each, as its own quoted element, in code with comments stripped.
 */
const COMMAND_FLAG = /["'`]-(c|co|com|comm|comma|comman|command)["'`]/i;
const stripComments = (text) => text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/.*$/gm, "$1");

/** `file: line` for every COMMAND_FLAG hit in `files`, paths relative to `root` with "/" separators on every OS. */
function commandFlagHits(root, files) {
  return files.flatMap((f) =>
    stripComments(readFileSync(f, "utf8"))
      .split("\n")
      .filter((l) => COMMAND_FLAG.test(l))
      .map((l) => `${relative(root, f).split(sep).join("/")}: ${l.trim()}`),
  );
}

/**
 * Where a quoted -c-like flag is allowed, and why: the D13 session shell's own
 * command line, and flags of programs that are not PowerShell (git -C, tar -C,
 * a POSIX shell's -c).
 */
const ALLOWED_COMMAND_FLAGS = [
  [/^scripts\/termhost-bench\/adapters\/vscode\.mjs: shellArgs: \["-NoLogo", "-NoExit", "-Command", commandLine\(argv, "powershell"\)\],$/, "D13 session shell"],
  [/^scripts\/termhost-bench\/adapters\/windows-terminal\.mjs: const out = \[shellPath, "-NoLogo", "-NoExit", "-Command", commandLine\(argv, "powershell"\)\];$/, "D13 session shell"],
  [/^scripts\/termhost-bench\/(cli|run|adapters\/runner)\.mjs: .*run\("git", \["-C", /, "git -C"],
  [/^scripts\/termhost-bench\/adapters\/vscode-install\.mjs: .*args: \["-xz?f", archive, "-C", dest\]/, "tar -C"],
  [/^scripts\/termhost-bench\/adapters\/tmux\.mjs: return \[shell, "-i", "-c", /, "POSIX shell -c"],
];

test("one rule for harness PowerShell, scanned: no -Command (any case, any abbreviation) outside the D13 session shell, in the bench, scripts/lib and the frame builders", async () => {
  const { readdirSync, statSync: st } = await import("node:fs");
  const scripts = join(import.meta.dirname, "..", "..");
  const files = [join(scripts, "tui-repaint-generator.mjs")];
  const walk = (d) => {
    for (const n of readdirSync(d)) {
      const p = join(d, n);
      if (st(p).isDirectory()) {
        if (n !== "__tests__" && n !== "node_modules") walk(p);
      } else if (/\.(mjs|js|cjs)$/.test(n)) files.push(p);
    }
  };
  walk(join(scripts, "termhost-bench"));
  walk(join(scripts, "lib"));
  const root = join(scripts, "..");
  const rel = files.map((f) => relative(root, f).split(sep).join("/"));
  for (const want of ["scripts/termhost-bench/proc.mjs", "scripts/lib/runner-clients.mjs", "scripts/tui-repaint-generator.mjs", "scripts/termhost-bench/adapters/vscode.mjs"]) {
    assert.ok(rel.includes(want), `${want} is scanned`);
  }
  const hits = commandFlagHits(root, files);
  const unexplained = hits.filter((h) => !ALLOWED_COMMAND_FLAGS.some(([re]) => re.test(h)));
  assert.deepEqual(unexplained, [], "a -Command outside the D13 session shell");
  assert.equal(hits.filter((h) => /-NoExit", "-Command"/.test(h)).length, 2, "the two D13 session shells");
});

test("the -Command scan can fail: an abbreviated, lower-case, or Windows-separated hit is caught", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-cmdscan-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const bad = join(dir, "bad.mjs");
  for (const flag of ["-Command", "-command", "-c", "-Comm", "-CO"]) {
    writeFileSync(bad, `run("powershell.exe", ["-NoProfile", "${flag}", script]);\n// "-Command" in a comment is fine\n`);
    const hits = commandFlagHits(dir, [bad]);
    assert.equal(hits.length, 1, flag);
    assert.ok(!ALLOWED_COMMAND_FLAGS.some(([re]) => re.test(hits[0])), `${flag} is not explained away`);
  }
  writeFileSync(bad, `const x = "ConvertTo-Json -Compress";\n`);
  assert.deepEqual(commandFlagHits(dir, [bad]), [], "-Compress inside a string is not a flag element");
  // The separator: a Windows path relative to the root reads with "/".
  assert.equal(win32.relative("D:\\r", "D:\\r\\scripts\\a.mjs").split(win32.sep).join("/"), "scripts/a.mjs");
});

test("multi-line PowerShell travels as -EncodedCommand (UTF-16LE base64) and decodes to the same script", () => {
  for (const script of [WINDOW_QUERY, WINDOW_CLOSE, WT_PROCESS_QUERY, "a 'quoted' \"double\" ü\n@'\nhere\n'@\n"]) {
    const argv = powershellArgv(script);
    assert.deepEqual(argv.slice(0, 3), ["-NoProfile", "-NonInteractive", "-EncodedCommand"]);
    assert.equal(Buffer.from(argv[3], "base64").toString("utf16le"), script);
    assert.equal(argv[3], encodePowerShell(script));
    assert.match(argv[3], /^[A-Za-z0-9+/]+=*$/, "nothing a command line could re-quote");
  }
  assert.ok(powershellArgv(WINDOW_CLOSE)[3].length < 32000, "inside Windows' 32 767-character command line");
});

test("without --sessionId the panes still launch, and the run loop falls back to slot order (noted)", async (t) => {
  const r = rig(t, { sessionId: false });
  await r.adapter.launch(r.ctx);
  const s = await r.adapter.spawnSessions(2, r.argv);
  assert.ok(!r.wt.invocations[0].includes("--sessionId"));
  assert.match(s.sessions[0].hostId, /^termhost-r1-u0:s0$/);
  assert.match(r.adapter.notes.join(" "), /registers no --sessionId/);
  assert.match(r.adapter.describe().app_version, /no --sessionId/);
});

test("the doctor's facts: not on Linux; a cache inside a checkout; a missing session shell", async (t) => {
  const linux = new WindowsTerminalAdapter({ platform: "linux" });
  assert.equal((await linux.check()).mechanisms["setup.windows_terminal"].state, "INOPERATIVE-ON-THIS-MACHINE");
  const repo = tmp(t);
  mkdirSync(join(repo, ".git"));
  const inRepo = new WindowsTerminalAdapter({ platform: "win32", arch: "x64", env: WIN_ENV, cacheDir: join(repo, "cache"), exists: (p) => p === PS || existsSync(p) });
  assert.match((await inRepo.check()).mechanisms["setup.windows_terminal"].reason, /inside the git checkout/);
  const noShell = new WindowsTerminalAdapter({ platform: "win32", arch: "x64", env: WIN_ENV, cacheDir: tmp(t), exists: () => false });
  assert.equal((await noShell.check()).mechanisms["setup.windows_terminal_shell"].state, "INOPERATIVE-ON-THIS-MACHINE");
  const arm = new WindowsTerminalAdapter({ platform: "win32", arch: "arm64", env: WIN_ENV, cacheDir: tmp(t) });
  assert.match((await arm.check()).mechanisms["setup.windows_terminal"].reason, /no pinned Windows Terminal build for win32-arm64/);
});

test("closeOne refuses a shell that is not under the proven instance (it never signals a stranger)", async (t) => {
  const r = rig(t);
  await r.adapter.launch(r.ctx);
  await r.adapter.spawnSessions(1, r.argv);
  const one = await r.adapter.spawnOne();
  const s = r.wt.sessions.find((x) => x.sid === one.hostId);
  r.wt.procs.get(s.shell).ppid = r.wt.operatorPid;
  const res = await r.adapter.closeOne(one.hostId);
  assert.equal(res.ok, false);
  assert.match(res.detail, /not a descendant/);
  assert.ok(!r.wt.signalled.some(([p]) => p === s.shell || p === s.gen));
  assert.equal(GENERATOR_FLAG, "--termhost-bench-generator");
});
