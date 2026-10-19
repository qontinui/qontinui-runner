/**
 * The VS Code harness extension (`vscode-harness-extension/`), without VS Code:
 * its pure parts — the control protocol (authentication, validation, the request
 * a client builds) and the arrangement planner (the grid each N gets) — and its
 * controller and endpoint driven against a simulated workbench
 * (`helpers/fake-vscode.mjs`) over real HTTP on 127.0.0.1.
 *
 *   node --test scripts/termhost-bench/__tests__/vscode-extension.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { vscodeSettings } from "../adapters/vscode.mjs";
import { createFakeVscode } from "./helpers/fake-vscode.mjs";

/** The simulated workbench runs on the private profile the adapter writes. */
const PROFILE = vscodeSettings();
const workbench = (o = {}) => createFakeVscode({ ...o, settings: { ...PROFILE, ...(o.settings ?? {}) } });

const require = createRequire(import.meta.url);
const protocol = require("../vscode-harness-extension/protocol.js");
const { HarnessController, startHarness, CMD } = require("../vscode-harness-extension/harness.js");

const NONCE = "ab".repeat(32);
const SHELL = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
const ARGS = ["-NoLogo", "-NoExit", "-Command", "& 'node.exe' 'gen.mjs' '--termhost-bench-generator' 'r-u0'"];

function config(over = {}) {
  return { schema: protocol.CONFIG_SCHEMA, nonce: NONCE, controlFile: "/tmp/x/control.json", shellPath: SHELL, platform: "win32", logFile: null, terminalPrefix: "vsc", ...over };
}

function controller(fake, over = {}) {
  return new HarnessController({ vscode: fake.vscode, config: config(over), timing: { stepMs: 1500, processIdMs: 500, closeMs: 500 } });
}

const rpc = (cmd, args = {}) => ({
  method: "POST",
  url: "/rpc",
  headers: { host: "127.0.0.1:4100", [protocol.NONCE_HEADER]: NONCE, "content-type": "application/json" },
  body: JSON.stringify({ cmd, args }),
});

// ---------------------------------------------------------------------------
// The protocol (pure)
// ---------------------------------------------------------------------------

test("a well-formed request is accepted, and its arguments normalised", () => {
  const p = protocol.parseRpc(rpc("spawn", { count: 3, location: "panel", shellPath: SHELL, shellArgs: ARGS }), { port: 4100, nonce: NONCE });
  assert.equal(p.ok, true);
  assert.equal(p.cmd, "spawn");
  assert.deepEqual(p.args, { count: 3, location: "panel", shellPath: SHELL, shellArgs: ARGS, show: false });
});

test("the endpoint refuses every request it cannot authenticate, each with its own status and code", () => {
  const auth = { port: 4100, nonce: NONCE };
  const refused = (req) => {
    const p = protocol.parseRpc(req, auth);
    assert.equal(p.ok, false, JSON.stringify(req));
    return [p.status, p.code];
  };
  const base = rpc("hello");
  assert.deepEqual(refused({ ...base, url: "/other" }), [404, "not_found"]);
  assert.deepEqual(refused({ ...base, method: "GET" }), [405, "method_not_allowed"]);
  assert.deepEqual(refused({ ...base, headers: { ...base.headers, host: "localhost:4100" } }), [403, "bad_host"], "a DNS-rebinding name");
  assert.deepEqual(refused({ ...base, headers: { ...base.headers, host: "127.0.0.1:4101" } }), [403, "bad_host"]);
  const { host: _h, ...noHost } = base.headers;
  assert.deepEqual(refused({ ...base, headers: noHost }), [403, "bad_host"]);
  const { [protocol.NONCE_HEADER]: _n, ...noNonce } = base.headers;
  assert.deepEqual(refused({ ...base, headers: noNonce }), [403, "bad_nonce"]);
  assert.deepEqual(refused({ ...base, headers: { ...base.headers, [protocol.NONCE_HEADER]: "cd".repeat(32) } }), [403, "bad_nonce"]);
  assert.deepEqual(refused({ ...base, headers: { ...base.headers, [protocol.NONCE_HEADER]: "ab" } }), [403, "bad_nonce"], "a length mismatch");
  assert.deepEqual(refused({ ...base, headers: { ...base.headers, "content-type": "text/plain" } }), [415, "unsupported_media_type"], "a browser's simple request");
  assert.deepEqual(refused({ ...base, body: "x".repeat(protocol.MAX_BODY_BYTES + 1) }), [413, "too_large"]);
  assert.deepEqual(refused({ ...base, body: "{" }), [400, "bad_json"]);
  assert.deepEqual(refused({ ...base, body: JSON.stringify({ cmd: "rm", args: {} }) }), [400, "unknown_command"]);
  assert.deepEqual(refused({ ...base, body: JSON.stringify({ cmd: "hello", args: {}, extra: 1 }) }), [400, "bad_request"]);
  assert.deepEqual(refused({ ...base, body: JSON.stringify({ cmd: "show", args: { id: "../x" } }) }), [400, "bad_args"]);
  // A server whose own nonce is malformed refuses everything.
  assert.equal(protocol.parseRpc(base, { port: 4100, nonce: "short" }).code, "bad_nonce");
});

test("argument validation: counts, locations, ids, duplicates, the tiled bound, and unknown keys", () => {
  const ok = (cmd, args) => assert.equal(protocol.validateArgs(cmd, args).ok, true, `${cmd} ${JSON.stringify(args)}`);
  const bad = (cmd, args, re) => {
    const v = protocol.validateArgs(cmd, args);
    assert.equal(v.ok, false, `${cmd} ${JSON.stringify(args)}`);
    assert.match(v.error, re);
  };
  const spawn = { count: 1, location: "panel", shellPath: SHELL, shellArgs: ARGS };
  ok("spawn", spawn);
  ok("spawn", { ...spawn, location: "editor-active", shellArgs: "-NoLogo" });
  bad("spawn", { ...spawn, count: 0 }, /count/);
  bad("spawn", { ...spawn, count: protocol.MAX_SPAWN + 1 }, /count/);
  bad("spawn", { ...spawn, location: "beside" }, /location/);
  bad("spawn", { ...spawn, shellPath: "" }, /shellPath/);
  bad("spawn", { ...spawn, shellArgs: [1] }, /shellArgs/);
  bad("spawn", { ...spawn, cwd: "/" }, /unknown argument/);
  ok("arrange", { arm: "tiled", order: ["vsc-1", "vsc-2"] });
  bad("arrange", { arm: "grid", order: ["vsc-1"] }, /arm/);
  bad("arrange", { arm: "focus", order: [] }, /order/);
  bad("arrange", { arm: "focus", order: ["vsc-1", "vsc-1"] }, /twice/);
  bad("arrange", { arm: "focus", order: ["vsc 1"] }, /match/);
  bad("arrange", { arm: "tiled", order: ["vsc-1"], maxTiled: 10 }, /maxTiled/);
  ok("close", { all: true });
  ok("close", { id: "vsc-3" });
  bad("close", { id: "vsc-3", all: true }, /either/);
  bad("close", { all: false }, /either/);
  bad("hello", { x: 1 }, /no arguments/);
  bad("show", { id: "vsc-1", preserveFocus: "yes" }, /preserveFocus/);
  bad("frobnicate", {}, /unknown command/);
});

test("the client's request is built by the server's own validator and round-trips", () => {
  const req = protocol.buildRpc("arrange", { arm: "focus", order: ["vsc-1"] }, NONCE);
  assert.equal(req.path, "/rpc");
  assert.equal(req.headers[protocol.NONCE_HEADER], NONCE);
  const p = protocol.parseRpc({ method: "POST", url: req.path, headers: { ...req.headers, host: "127.0.0.1:9" }, body: req.body }, { port: 9, nonce: NONCE });
  assert.deepEqual([p.ok, p.cmd, p.args], [true, "arrange", { arm: "focus", order: ["vsc-1"], maxTiled: 9 }]);
  assert.throws(() => protocol.buildRpc("arrange", { arm: "focus", order: [] }, NONCE), /order/);
  assert.throws(() => protocol.buildRpc("hello", {}, "not-hex"), /nonce/);
});

test("the grid for V panes: as square as the count allows, rows filled in order, at most 3 x 3", () => {
  const rows = Object.fromEntries([1, 2, 3, 4, 5, 6, 7, 8, 9].map((v) => [v, protocol.planGrid(v).rows]));
  assert.deepEqual(rows, { 1: [1], 2: [2], 3: [2, 1], 4: [2, 2], 5: [3, 2], 6: [3, 3], 7: [3, 3, 1], 8: [3, 3, 2], 9: [3, 3, 3] });
  assert.deepEqual(protocol.planGrid(1).layout, { orientation: 0, groups: [{}] });
  const nine = protocol.planGrid(9).layout;
  assert.equal(nine.orientation, 1, "rows stack vertically, so groups are numbered row-major");
  assert.equal(nine.groups.length, 3);
  for (const row of nine.groups) {
    assert.equal(row.groups.length, 3);
    assert.ok(Math.abs(row.size - 1 / 3) < 1e-12);
  }
  assert.deepEqual(protocol.planGrid(7).layout.groups[2], { size: 1 / 3 }, "a single trailing pane spans its row");
  for (const v of [0, 10, 2.5]) assert.throws(() => protocol.planGrid(v), /1\.\.9/);
});

test("arrangement planning: focus shows one, tiled shows min(N, 9) in slot order and hides the rest", () => {
  const order = Array.from({ length: 15 }, (_, i) => `vsc-${i + 1}`);
  const focus = protocol.planArrangement({ arm: "focus", order });
  assert.deepEqual([focus.visible, focus.hidden.length, focus.focus, focus.grid], [["vsc-1"], 14, "vsc-1", null]);
  const tiled = protocol.planArrangement({ arm: "tiled", order });
  assert.deepEqual(tiled.visible, order.slice(0, 9));
  assert.deepEqual(tiled.hidden, order.slice(9));
  assert.deepEqual(tiled.grid.rows, [3, 3, 3]);
  assert.deepEqual(protocol.planArrangement({ arm: "tiled", order: order.slice(0, 5) }).grid.rows, [3, 2]);
  assert.deepEqual(protocol.planArrangement({ arm: "tiled", order: order.slice(0, 1) }).grid.rows, [1]);
  assert.deepEqual(protocol.planArrangement({ arm: "tiled", order, maxTiled: 4 }).grid.rows, [2, 2]);
});

test("D13: the first spawn fixes the command line; a different one, or another shell, is refused", () => {
  const allowed = { allowedShellPath: SHELL, caseInsensitivePath: true };
  const first = protocol.checkCommandLine(null, { shellPath: SHELL, shellArgs: ARGS }, allowed);
  assert.equal(first.ok, true);
  assert.equal(protocol.checkCommandLine(first.recorded, { shellPath: SHELL.toUpperCase(), shellArgs: [...ARGS] }, allowed).ok, true, "Windows paths compare case-insensitively");
  const changed = protocol.checkCommandLine(first.recorded, { shellPath: SHELL, shellArgs: [...ARGS.slice(0, 3), "& 'other.exe'"] }, allowed);
  assert.deepEqual([changed.ok, changed.code], [false, "command_line_mismatch"]);
  const other = protocol.checkCommandLine(null, { shellPath: "C:\\Windows\\System32\\cmd.exe", shellArgs: ["/c", "calc"] }, allowed);
  assert.deepEqual([other.ok, other.code], [false, "shell_not_allowed"]);
  assert.equal(protocol.checkCommandLine(null, { shellPath: "/BIN/BASH", shellArgs: [] }, { allowedShellPath: "/bin/bash" }).ok, false, "POSIX paths are case-sensitive");
});

test("the config and control files are validated on read", () => {
  assert.equal(protocol.parseConfig(JSON.stringify(config())).terminalPrefix, "vsc");
  assert.throws(() => protocol.parseConfig(JSON.stringify(config({ schema: "x" }))), /not a/);
  assert.throws(() => protocol.parseConfig(JSON.stringify(config({ nonce: "zz" }))), /nonce/);
  assert.throws(() => protocol.parseConfig(JSON.stringify(config({ shellPath: "" }))), /shellPath/);
  assert.throws(() => protocol.parseConfig(JSON.stringify(config({ terminalPrefix: "a b" }))), /terminalPrefix/);
  const rec = protocol.controlRecord({ port: 4100, pid: 77, ppid: 76, vscodeVersion: "1.139.1", startedAt: "2026-10-12T00:00:00Z" });
  assert.equal(protocol.parseControl(JSON.stringify(rec)).port, 4100);
  assert.throws(() => protocol.parseControl(JSON.stringify({ ...rec, protocol: "v0" })), /speaks/);
  assert.throws(() => protocol.parseControl(JSON.stringify({ ...rec, port: 70000 })), /port/);
});

// ---------------------------------------------------------------------------
// The controller against a simulated workbench
// ---------------------------------------------------------------------------

async function spawned(n, fakeOpts = {}) {
  const fake = workbench(fakeOpts);
  const c = controller(fake);
  const r = await c.handle("spawn", { count: n, location: "panel", shellPath: SHELL, shellArgs: ARGS, show: false });
  return { fake, c, ids: r.terminals.map((t) => t.id), r };
}

test("spawn: n panel terminals on the one command line, each exporting its own id, the config path removed", async () => {
  const { fake, ids, r } = await spawned(3);
  assert.deepEqual(ids, ["vsc-1", "vsc-2", "vsc-3"]);
  assert.ok(r.terminals.every((t) => Number.isInteger(t.pid) && t.location === "panel"));
  for (const t of fake.state.created) {
    const o = t.creationOptions;
    assert.equal(o.shellPath, SHELL);
    assert.deepEqual(o.shellArgs, ARGS);
    assert.equal(o.location, fake.vscode.TerminalLocation.Panel);
    assert.equal(o.env[protocol.TERMINAL_ID_ENV], o.name);
    assert.equal(o.env[protocol.CONFIG_ENV], null, "the shells never see the harness's config path");
  }
  assert.deepEqual(fake.where().panel, ids);
});

test("spawn refuses another command line (409) and another shell (403) within one launch", async () => {
  const { c } = await spawned(1);
  await assert.rejects(c.handle("spawn", { count: 1, location: "panel", shellPath: SHELL, shellArgs: ["-NoLogo"] }), (e) => e.status === 409 && e.code === "command_line_mismatch");
  await assert.rejects(c.handle("spawn", { count: 1, location: "panel", shellPath: "C:\\x\\cmd.exe", shellArgs: ARGS }), (e) => e.status === 403 && e.code === "shell_not_allowed");
});

test("tiled at N = 5: a 3+2 grid, the terminals in groups 1..5 in slot order, the panel closed — read back, not assumed", async () => {
  const { fake, c, ids } = await spawned(5);
  const r = await c.handle("arrange", { arm: "tiled", order: ids });
  assert.equal(r.ok, true, r.failures.join("; "));
  assert.deepEqual(r.rows, [3, 2]);
  assert.deepEqual(r.groups.map((g) => [g.viewColumn, g.id, g.matched]), ids.map((id, i) => [i + 1, id, "tab_label"]));
  const w = fake.where();
  assert.deepEqual(w.groups, ids.map((id) => [id]));
  assert.deepEqual(w.panel, []);
  assert.equal(fake.state.panel.visible, false);
  assert.equal(w.active, "vsc-1", "the first session holds the focus");
  assert.equal(fake.state.sidebar, false);
  assert.ok(fake.state.commands.some((x) => Array.isArray(x) && x[0] === CMD.setLayout && x[1].groups.length === 2));
});

test("tiled at N = 12: nine in a 3 x 3 grid, three left as panel tabs behind a closed panel", async () => {
  const { fake, c, ids } = await spawned(12);
  const r = await c.handle("arrange", { arm: "tiled", order: ids });
  assert.equal(r.ok, true, r.failures.join("; "));
  const w = fake.where();
  assert.deepEqual(w.groups, ids.slice(0, 9).map((id) => [id]));
  assert.deepEqual(w.panel, ids.slice(9));
  assert.equal(fake.state.panel.visible, false);
  assert.deepEqual(r.hidden, ids.slice(9));
});

test("focus after tiled: every terminal back in the panel, one editor group, the panel maximized on the first session", async () => {
  const { fake, c, ids } = await spawned(6);
  assert.equal((await c.handle("arrange", { arm: "tiled", order: ids })).ok, true);
  const r = await c.handle("arrange", { arm: "focus", order: ids });
  assert.equal(r.ok, true, r.failures.join("; "));
  const w = fake.where();
  assert.deepEqual([...w.panel].sort(), [...ids].sort());
  assert.equal(fake.state.groups.length, 1);
  assert.deepEqual(w.groups, [[]]);
  assert.equal(fake.state.panel.visible, true);
  assert.equal(fake.state.panel.maximized, true);
  assert.equal(w.active, "vsc-1");
  // And back again: the arms run in either order on the same sessions.
  assert.equal((await c.handle("arrange", { arm: "tiled", order: ids })).ok, true);
  assert.deepEqual(fake.where().groups, ids.map((id) => [id]));
});

test("focus from a maximized panel re-maximizes deterministically (close first, then one toggle)", async () => {
  const { fake, c, ids } = await spawned(3);
  for (const arm of ["focus", "focus", "tiled", "focus", "focus"]) {
    const r = await c.handle("arrange", { arm, order: ids });
    assert.equal(r.ok, true, `${arm}: ${r.failures.join("; ")}`);
    if (arm === "focus") assert.equal(fake.state.panel.maximized, true, "every focus arrangement ends maximized");
  }
});

test("under VS Code's default opensMaximized \"preserve\" the focus arm refuses rather than toggle blind", async () => {
  const { fake, c, ids } = await spawned(3, { settings: { "workbench.panel.opensMaximized": "preserve" } });
  const r = await c.handle("arrange", { arm: "focus", order: ids });
  assert.equal(r.ok, false);
  assert.match(r.failures[0], /opensMaximized is "preserve", not "never"/);
  assert.equal(PROFILE["workbench.panel.opensMaximized"], "never", "the private profile sets it");
  // What the guard prevents, in the simulated workbench: preserve reopens a
  // panel hidden while maximized as maximized, and one toggle un-maximizes it.
  fake.vscode.window.createTerminal({ name: "x", location: 1 }).show(false);
  await fake.vscode.commands.executeCommand("workbench.action.toggleMaximizedPanel");
  await fake.vscode.commands.executeCommand("workbench.action.closePanel");
  fake.state.panel.active.show(false);
  assert.equal(fake.state.panel.maximized, true, "preserve reopened it maximized");
  await fake.vscode.commands.executeCommand("workbench.action.toggleMaximizedPanel");
  assert.equal(fake.state.panel.maximized, false, "so the blind toggle un-maximizes");
});

test("tabs not labelled with the terminal's name still verify — as the active terminal editor of its group, with a note", async () => {
  const { fake, c, ids } = await spawned(4, { faults: { genericTabLabel: "pwsh" } });
  const r = await c.handle("arrange", { arm: "tiled", order: ids });
  assert.equal(r.ok, true, r.failures.join("; "));
  assert.deepEqual(r.groups.map((g) => g.matched), ["active_terminal", "active_terminal", "active_terminal", "active_terminal"]);
  assert.equal(r.notes.length, 4);
  assert.deepEqual(fake.where().groups, ids.map((id) => [id]));
  const back = await c.handle("arrange", { arm: "focus", order: ids });
  assert.equal(back.ok, true, back.failures.join("; "));
  assert.deepEqual([...fake.where().panel].sort(), [...ids].sort(), "location tracking moves them back without labels");
});

test("a move that never happens is reported as an unrealised arm, never assumed", async () => {
  for (const fault of ["moveToEditorNoop", "moveToEditorWrongGroup", "focusGroupNoop"]) {
    const { c, ids } = await spawned(3, { faults: { [fault]: true } });
    const r = await c.handle("arrange", { arm: "tiled", order: ids });
    assert.equal(r.ok, false, fault);
    assert.ok(r.failures.length > 0, fault);
  }
});

test("show, focus, list, close and an editor-area spawn", async () => {
  const { fake, c, ids } = await spawned(3);
  await c.handle("arrange", { arm: "focus", order: ids });
  const s = await c.handle("show", { id: "vsc-3", preserveFocus: false });
  assert.deepEqual(s, { id: "vsc-3", active: true });
  assert.equal(fake.state.panel.active.name, "vsc-3");
  assert.deepEqual(await c.handle("focus", { id: "vsc-2" }), { id: "vsc-2", active: true });
  await c.handle("arrange", { arm: "tiled", order: ids });
  const one = await c.handle("spawn", { count: 1, location: "editor-active", shellPath: SHELL, shellArgs: ARGS, show: true });
  assert.equal(one.terminals[0].location, "editor");
  assert.ok(fake.where().groups[0].includes(one.terminals[0].id), "a new tab in the active group (group 1 holds the focus)");
  const closed = await c.handle("close", { id: one.terminals[0].id });
  assert.deepEqual(closed, { closed: [one.terminals[0].id], timedOut: [] });
  await assert.rejects(c.handle("show", { id: one.terminals[0].id }), (e) => e.code === "terminal_exited");
  await assert.rejects(c.handle("show", { id: "vsc-99" }), (e) => e.status === 404);
  const all = await c.handle("close", { all: true });
  assert.deepEqual(all.closed, ids);
  assert.equal(fake.vscode.window.terminals.length, 0);
  assert.ok(c.list().terminals.every((t) => t.exited));
});

test("hello reads the terminal settings back with their defaults; quit runs workbench.action.quit", async () => {
  let quit = false;
  const fake = workbench({ settings: { "terminal.integrated.scrollback": 10000 }, onQuit: () => (quit = true) });
  const c = controller(fake);
  const h = c.hello();
  assert.equal(h.protocol, protocol.PROTOCOL);
  assert.equal(h.vscode.version, "1.139.1");
  assert.deepEqual(h.settings.gpuAcceleration, { value: "auto", default: "auto", user: null });
  assert.deepEqual(h.settings.windowsUseConptyDll, { value: true, default: true, user: null });
  assert.deepEqual(h.settings.scrollback, { value: 10000, default: 1000, user: 10000 });
  assert.deepEqual(c.quit(), { quitting: true });
  await new Promise((r) => setTimeout(r, 250));
  assert.equal(quit, true);
});

// ---------------------------------------------------------------------------
// The endpoint over HTTP
// ---------------------------------------------------------------------------

function raw(port, { method = "POST", path = "/rpc", headers = {}, body = "" }) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: "127.0.0.1", port, method, path, headers: { "content-length": Buffer.byteLength(body), ...headers } }, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode, body: JSON.parse(Buffer.concat(chunks).toString("utf8")) }));
    });
    req.on("error", reject);
    req.end(body);
  });
}

async function started(t, fakeOpts = {}) {
  const dir = mkdtempSync(join(tmpdir(), "tb-vsc-ext-"));
  const fake = workbench(fakeOpts);
  const cfgPath = join(dir, "harness-config.json");
  writeFileSync(cfgPath, JSON.stringify(config({ controlFile: join(dir, "control.json"), logFile: join(dir, "harness.log") })));
  const h = await startHarness({ vscode: fake.vscode, env: { [protocol.CONFIG_ENV]: cfgPath }, timing: { stepMs: 1500, processIdMs: 500, closeMs: 500 } });
  t.after(async () => {
    await h.dispose();
    rmSync(dir, { recursive: true, force: true });
  });
  return { dir, fake, h };
}

test("the extension is inert without a harness config: no endpoint, no control file", async () => {
  const fake = createFakeVscode();
  assert.equal(await startHarness({ vscode: fake.vscode, env: {} }), null);
});

test("the endpoint writes its port into the control file and answers only an authenticated 127.0.0.1 request", async (t) => {
  const { dir, h } = await started(t);
  const control = protocol.parseControl(readFileSync(join(dir, "control.json"), "utf8"));
  assert.equal(control.port, h.port);
  assert.equal(control.pid, process.pid);
  const good = { "content-type": "application/json", [protocol.NONCE_HEADER]: NONCE };
  const hello = await raw(h.port, { headers: good, body: JSON.stringify({ cmd: "hello", args: {} }) });
  assert.equal(hello.status, 200);
  assert.equal(hello.body.result.protocol, protocol.PROTOCOL);
  const wrongNonce = await raw(h.port, { headers: { ...good, [protocol.NONCE_HEADER]: "cd".repeat(32) }, body: JSON.stringify({ cmd: "hello", args: {} }) });
  assert.deepEqual([wrongNonce.status, wrongNonce.body.error.code], [403, "bad_nonce"]);
  const rebinding = await raw(h.port, { headers: { ...good, host: `evil.example:${h.port}` }, body: JSON.stringify({ cmd: "hello", args: {} }) });
  assert.deepEqual([rebinding.status, rebinding.body.error.code], [403, "bad_host"]);
  const big = await raw(h.port, { headers: good, body: "x".repeat(protocol.MAX_BODY_BYTES + 10) });
  assert.equal(big.status, 413);
  assert.match(readFileSync(join(dir, "harness.log"), "utf8"), /refused POST \/rpc: bad_nonce/);
});

test("requests execute one at a time: a show that arrives mid-arrangement runs after it", async (t) => {
  // Slow moves (40 ms each) give the show a wide window to arrive in.
  const { fake, h } = await started(t, { asyncMs: 40 });
  const events = [];
  h.server.on("request", () => events.push("arrive"));
  const handle = h.controller.handle.bind(h.controller);
  h.controller.handle = async (cmd, args) => {
    events.push(`start:${cmd}`);
    try {
      return await handle(cmd, args);
    } finally {
      events.push(`end:${cmd}`);
    }
  };
  const good = { "content-type": "application/json", [protocol.NONCE_HEADER]: NONCE };
  const call = (cmd, args) => raw(h.port, { headers: good, body: JSON.stringify({ cmd, args }) });
  const sp = await call("spawn", { count: 4, location: "panel", shellPath: SHELL, shellArgs: ARGS });
  const ids = sp.body.result.terminals.map((x) => x.id);
  const arranging = call("arrange", { arm: "tiled", order: ids });
  // Send the show only once the grid exists: the arrangement is under way.
  const deadline = Date.now() + 10000;
  while (!fake.state.commands.some((c) => Array.isArray(c) && c[0] === CMD.setLayout && c[1].orientation === 1)) {
    assert.ok(Date.now() < deadline, "the arrangement never built its grid within 10 s");
    await new Promise((r) => setTimeout(r, 5));
  }
  const shown = await call("show", { id: ids[2] });
  const arranged = await arranging;
  assert.equal(arranged.body.result.ok, true, JSON.stringify(arranged.body));
  assert.equal(shown.status, 200);
  const third = events.map((e, i) => [e, i]).filter(([e]) => e === "arrive")[2][1];
  assert.ok(third < events.indexOf("end:arrange"), `the show arrived during the arrangement: ${events.join(" ")}`);
  assert.ok(events.indexOf("start:show") > events.indexOf("end:arrange"), `and ran after it: ${events.join(" ")}`);
  // So the shown terminal (a terminal editor in group 3) is the active one; an
  // interleaved show would have been overridden by the arrangement's final focus.
  assert.equal(fake.where().active, ids[2]);
  assert.equal(fake.state.activeGroup, 2);
});
