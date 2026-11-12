/**
 * Unit tests for the testable half of the session-info oracle.
 *
 *   node --test scripts/__tests__/uibridge-session-info-lib.test.mjs
 *
 * No runner, no `/proc`, no PowerShell: every OS read is an injected fake.
 * Expectations are literals (pids, image names, refusal reasons), never values
 * recomputed with the helper under test.
 *
 * Plan: `2026-11-08-tenant-badge-parity-and-oracle-fixtures` Phases 3-5.
 */

import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  KNOWN_LAUNCH_GATES,
  accountInRoster,
  badgeCardinality,
  buildFixtureTerminalRequest,
  classifyTerminalSpawn,
  compareBadgeElement,
  deriveAccountName,
  imageBasename,
  isRunnerImage,
  killTargetRunner,
  listenerInodeForPort,
  openResidue,
  parseZoneViewState,
  pickFixtureSessionId,
  planMenuAnswer,
  planRosterSeed,
  planSpawnRetry,
  planTargetKill,
  resolvePowerShellInterpreter,
  refusedGateIn,
  resolveTargetRunnerPid,
  sameRoster,
  selfHostingVerdict,
  tenantBadgeElementId,
  tenantBadgeExpectation,
  terminalBufferText,
  verifyRosterSeed,
  zoneIndexOf,
} from "../uibridge-session-info-lib.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));

// ---------------------------------------------------------------------------
// /proc/net/tcp fixtures. Port 9876 = 0x2694, 9877 = 0x2695.
// ---------------------------------------------------------------------------

const TCP_HEADER =
  "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

/** LISTEN on 127.0.0.1:9876 (inode 4242) + an ESTABLISHED client row on the same port (inode 5151). */
const TCP_LISTEN_AND_ESTABLISHED = [
  TCP_HEADER,
  "   0: 0100007F:2694 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0000000000000000 100 0 0 10 0",
  "   1: 0100007F:2694 0100007F:D3A2 01 00000000:00000000 00:00000000 00000000  1000        0 5151 1 0000000000000000 20 4 30 10 -1",
  "   2: 0100007F:D3A2 0100007F:2694 01 00000000:00000000 00:00000000 00000000  1000        0 5152 1 0000000000000000 20 4 30 10 -1",
].join("\n");

/** Only an ESTABLISHED row on 9876 — a client, never a server. */
const TCP_ESTABLISHED_ONLY = [
  TCP_HEADER,
  "   0: 0100007F:2694 0100007F:D3A2 01 00000000:00000000 00:00000000 00000000  1000        0 5151 1 0000000000000000 20 4 30 10 -1",
].join("\n");

/** LISTEN on [::1]:9877 (inode 7777). */
const TCP6_LISTEN_9877 = [
  "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode",
  "   0: 00000000000000000000000001000000:2695 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 7777 1 0000000000000000 100 0 0 10 0",
].join("\n");

describe("listenerInodeForPort", () => {
  test("LISTEN row wins; the ESTABLISHED row on the same port is ignored", () => {
    assert.deepEqual(listenerInodeForPort(TCP_LISTEN_AND_ESTABLISHED, 9876), ["4242"]);
  });
  test("an ESTABLISHED-only port has no listener", () => {
    assert.deepEqual(listenerInodeForPort(TCP_ESTABLISHED_ONLY, 9876), []);
  });
  test("a tcp6 LISTEN row is found", () => {
    assert.deepEqual(listenerInodeForPort(TCP6_LISTEN_9877, 9877), ["7777"]);
  });
  test("a wrong port finds nothing", () => {
    assert.deepEqual(listenerInodeForPort(TCP_LISTEN_AND_ESTABLISHED, 9877), []);
    assert.deepEqual(listenerInodeForPort(TCP6_LISTEN_9877, 9876), []);
  });
  test("garbage and bad ports yield nothing rather than throwing", () => {
    assert.deepEqual(listenerInodeForPort("not a table", 9876), []);
    assert.deepEqual(listenerInodeForPort(TCP_LISTEN_AND_ESTABLISHED, 0), []);
    assert.deepEqual(listenerInodeForPort(undefined, 9876), []);
  });
});

// ---------------------------------------------------------------------------
// The image predicate
// ---------------------------------------------------------------------------

describe("isRunnerImage", () => {
  test("accepts both runner basenames, in every spelling an OS hands back", () => {
    for (const name of [
      "qontinui-runner",
      "qontinui-runner-primary",
      "qontinui-runner.exe",
      "qontinui-runner-primary.exe",
      "QONTINUI-RUNNER.EXE",
      "/home/u/qontinui-runner/src-tauri/target/release/qontinui-runner",
      "/opt/q/qontinui-runner (deleted)",
      "C:\\qontinui\\bin\\qontinui-runner-primary.exe",
    ]) {
      assert.equal(isRunnerImage(name), true, name);
    }
  });
  test("rejects everything else", () => {
    for (const name of [
      "node",
      "node.exe",
      "claude",
      "bash",
      "powershell.exe",
      "qontinui-runner-test",
      "qontinui-supervisor",
      "my-qontinui-runner",
      "",
      null,
      undefined,
    ]) {
      assert.equal(isRunnerImage(name), false, String(name));
    }
  });
  test("imageBasename normalises paths, .exe and ' (deleted)'", () => {
    assert.equal(imageBasename("C:\\x\\Node.EXE"), "node");
    assert.equal(imageBasename("/usr/bin/qontinui-runner (deleted)"), "qontinui-runner");
  });
});

// ---------------------------------------------------------------------------
// resolveTargetRunnerPid with an injected OS
// ---------------------------------------------------------------------------

/** A fake Linux /proc. `procs`: pid -> {fds: {fd: linkTarget}, exe?, comm?}. */
function fakeLinuxIo({ tcp = "", tcp6 = "", procs = {} }) {
  const files = { "/proc/net/tcp": tcp, "/proc/net/tcp6": tcp6 };
  for (const [pid, p] of Object.entries(procs)) {
    if (p.comm !== undefined) files[`/proc/${pid}/comm`] = `${p.comm}\n`;
  }
  return {
    platform: "linux",
    readFile: (path) => {
      if (path in files) return files[path];
      throw new Error(`ENOENT ${path}`);
    },
    readdir: (path) => {
      if (path === "/proc") return [...Object.keys(procs), "self", "net", "sys"];
      const m = /^\/proc\/(\d+)\/fd$/.exec(path);
      if (m && procs[m[1]]) {
        if (procs[m[1]].fds === "EACCES") throw new Error("EACCES");
        return Object.keys(procs[m[1]].fds);
      }
      throw new Error(`ENOENT ${path}`);
    },
    readlink: (path) => {
      let m = /^\/proc\/(\d+)\/fd\/(\d+)$/.exec(path);
      if (m && procs[m[1]]?.fds?.[m[2]]) return procs[m[1]].fds[m[2]];
      m = /^\/proc\/(\d+)\/exe$/.exec(path);
      if (m && procs[m[1]]?.exe) return procs[m[1]].exe;
      throw new Error(`EACCES ${path}`);
    },
    runPowerShell: () => {
      throw new Error("no PowerShell on linux");
    },
  };
}

describe("resolveTargetRunnerPid (linux)", () => {
  test("LISTEN inode -> fd holder -> runner exe basename", async () => {
    const io = fakeLinuxIo({
      tcp: TCP_LISTEN_AND_ESTABLISHED,
      procs: {
        149760: {
          fds: { 0: "/dev/null", 17: "socket:[4242]" },
          exe: "/home/u/qontinui-runner/target-agent/debug/qontinui-runner",
        },
        // The client holding the ESTABLISHED socket must not be picked.
        2000: { fds: { 3: "socket:[5152]" }, exe: "/usr/bin/node" },
      },
    });
    const r = await resolveTargetRunnerPid(9876, io);
    assert.equal(r.pid, 149760);
    assert.equal(r.error, null);
    assert.equal(r.source, "proc-net-tcp");
    assert.equal(r.imageName, "qontinui-runner");
  });
  test("falls back to comm when /proc/<pid>/exe is unreadable", async () => {
    const io = fakeLinuxIo({
      tcp6: TCP6_LISTEN_9877,
      procs: { 333: { fds: { 9: "socket:[7777]" }, comm: "qontinui-runner" } },
    });
    const r = await resolveTargetRunnerPid(9877, io);
    assert.equal(r.pid, 333);
  });
  test("a listener whose image is not a runner yields pid null, but names the holder", async () => {
    const io = fakeLinuxIo({
      tcp: TCP_LISTEN_AND_ESTABLISHED,
      procs: { 4100: { fds: { 5: "socket:[4242]" }, exe: "/usr/bin/python3" } },
    });
    const r = await resolveTargetRunnerPid(9876, io);
    assert.equal(r.pid, null);
    assert.equal(r.holderPid, 4100);
    assert.equal(r.imageName, "python3");
    assert.match(r.error, /python3/);
  });
  test("no listener on the port -> pid null with a reason", async () => {
    const io = fakeLinuxIo({ tcp: TCP_ESTABLISHED_ONLY, procs: {} });
    const r = await resolveTargetRunnerPid(9876, io);
    assert.equal(r.pid, null);
    assert.match(r.error, /no process holds a LISTEN socket on port 9876/);
  });
  test("an inode no readable process holds -> pid null", async () => {
    const io = fakeLinuxIo({
      tcp: TCP_LISTEN_AND_ESTABLISHED,
      procs: { 1: { fds: "EACCES" } },
    });
    const r = await resolveTargetRunnerPid(9876, io);
    assert.equal(r.pid, null);
    assert.match(r.error, /4242/);
  });
  test("two holders of the listener -> ambiguous, pid null", async () => {
    const io = fakeLinuxIo({
      tcp: TCP_LISTEN_AND_ESTABLISHED,
      procs: {
        10: { fds: { 3: "socket:[4242]" }, exe: "/x/qontinui-runner" },
        11: { fds: { 3: "socket:[4242]" }, exe: "/x/qontinui-runner" },
      },
    });
    const r = await resolveTargetRunnerPid(9876, io);
    assert.equal(r.pid, null);
    assert.match(r.error, /ambiguous/);
  });
  test("unreadable /proc/net -> pid null, never a throw", async () => {
    const io = { ...fakeLinuxIo({}), readFile: () => Promise.reject(new Error("EPERM")) };
    const r = await resolveTargetRunnerPid(9876, io);
    assert.equal(r.pid, null);
    assert.match(r.error, /cannot read \/proc\/net\/tcp/);
  });
  test("an invalid port and an unsupported platform are UNKNOWN", async () => {
    assert.equal((await resolveTargetRunnerPid(0, fakeLinuxIo({}))).pid, null);
    const r = await resolveTargetRunnerPid(9876, { platform: "darwin" });
    assert.equal(r.pid, null);
    assert.match(r.error, /not implemented on platform 'darwin'/);
  });
});

describe("resolveTargetRunnerPid (win32)", () => {
  const winIo = (stdout, ok = true) => {
    const scripts = [];
    return {
      scripts,
      platform: "win32",
      runPowerShell: (script) => {
        scripts.push(script);
        return ok ? { ok: true, stdout } : { ok: false, stdout: "", error: "boom" };
      },
    };
  };
  test("Get-NetTCPConnection holder with the primary's image resolves", async () => {
    const io = winIo("8812|C:\\q\\qontinui-runner-primary.exe|qontinui-runner-primary\r\n");
    const r = await resolveTargetRunnerPid(9876, io);
    assert.equal(r.pid, 8812);
    assert.equal(r.source, "Get-NetTCPConnection");
    assert.match(io.scripts[0], /Get-NetTCPConnection -LocalPort 9876 -State Listen/);
    assert.match(io.scripts[0], /Get-Process -Id/);
    assert.doesNotMatch(io.scripts[0], /Get-Process -Name/);
  });
  test("a non-runner holder is refused by name", async () => {
    const r = await resolveTargetRunnerPid(9876, winIo("77|C:\\nodejs\\node.exe|node\r\n"));
    assert.equal(r.pid, null);
    assert.equal(r.holderPid, 77);
  });
  test("PowerShell failure -> pid null", async () => {
    const r = await resolveTargetRunnerPid(9876, winIo("", false));
    assert.equal(r.pid, null);
    assert.match(r.error, /could not run/);
  });
});

// ---------------------------------------------------------------------------
// selfHostingVerdict
// ---------------------------------------------------------------------------

/** The measured 2026-11-08 incident: hosted by the primary, testing a temp runner. */
const INCIDENT_CHAIN = [
  { name: "node", pid: 1200001 },
  { name: "bash", pid: 1200000 },
  { name: "claude", pid: 1199990 },
  { name: "qontinui-runner", pid: 1158459 },
  { name: "systemd", pid: 1500 },
];

describe("selfHostingVerdict", () => {
  test("the measured incident: another runner in the chain is CONTEXT, the verdict is safe", () => {
    const v = selfHostingVerdict({
      chain: INCIDENT_CHAIN,
      target: { pid: 149760, source: "proc-net-tcp", imageName: "qontinui-runner", error: null },
      registryNote: "not checked",
    });
    assert.equal(v.safe, true);
    assert.equal(v.evaluable, true);
    assert.equal(v.selfHosted, false);
    assert.equal(v.targetPid, 149760);
    assert.match(v.detail, /1158459/);
    assert.match(v.detail, /149760/);
  });
  test("a target inside the chain is SELF-HOSTED", () => {
    const v = selfHostingVerdict({
      chain: INCIDENT_CHAIN,
      target: { pid: 1158459, source: "proc-net-tcp", imageName: "qontinui-runner", error: null },
    });
    assert.equal(v.safe, false);
    assert.equal(v.evaluable, true);
    assert.equal(v.selfHosted, true);
    assert.match(v.detail, /^SELF-HOSTED/);
  });
  test("an unresolved target is not evaluable and not safe, naming the error", () => {
    const v = selfHostingVerdict({
      chain: INCIDENT_CHAIN,
      target: { pid: null, error: "no process holds a LISTEN socket on port 9877" },
    });
    assert.equal(v.evaluable, false);
    assert.equal(v.safe, false);
    assert.match(v.detail, /no process holds a LISTEN socket on port 9877/);
  });
  test("an empty chain is not evaluable", () => {
    const v = selfHostingVerdict({ chain: [], target: { pid: 149760 } });
    assert.equal(v.evaluable, false);
    assert.equal(v.safe, false);
  });
  test("no runner anywhere in the chain: safe with no context clause", () => {
    const v = selfHostingVerdict({
      chain: [
        { name: "node", pid: 5 },
        { name: "bash", pid: 4 },
      ],
      target: { pid: 149760 },
    });
    assert.equal(v.safe, true);
    assert.doesNotMatch(v.detail, /context/);
  });
});

// ---------------------------------------------------------------------------
// planTargetKill — every refusal arm and the one ok arm
// ---------------------------------------------------------------------------

const OK_WIN_PRIMARY = {
  gatePid: 8812,
  freshPid: 8812,
  imageName: "qontinui-runner-primary",
  port: 9876,
  devStartPort: 9876,
  devStartExplicit: false,
  platform: "win32",
  doorRunnable: true,
  interpreter: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
  devStart: "C:/qontinui-root/dev-start.ps1",
};

describe("resolvePowerShellInterpreter", () => {
  test("prefers powershell, falls back to pwsh (the Linux user-space install)", () => {
    const onlyPwsh = new Set(["/home/u/.local/bin/pwsh"]);
    assert.equal(
      resolvePowerShellInterpreter({
        pathEnv: "/usr/bin:/home/u/.local/bin",
        platform: "linux",
        exists: (p) => onlyPwsh.has(p),
      }),
      "/home/u/.local/bin/pwsh",
    );
    const both = new Set(["/usr/bin/pwsh", "/opt/ps/powershell"]);
    assert.equal(
      resolvePowerShellInterpreter({
        pathEnv: "/usr/bin:/opt/ps/",
        platform: "linux",
        exists: (p) => both.has(p),
      }),
      "/opt/ps/powershell",
    );
  });
  test("Windows: matches name.exe on a ;-separated PATH", () => {
    const want = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
    assert.equal(
      resolvePowerShellInterpreter({
        pathEnv: "C:\\x;C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\",
        platform: "win32",
        exists: (p) => p === want,
      }),
      want,
    );
  });
  test("none on PATH -> null", () => {
    assert.equal(
      resolvePowerShellInterpreter({ pathEnv: "/usr/bin", platform: "linux", exists: () => false }),
      null,
    );
  });
});

describe("planTargetKill", () => {
  test("ok: the same runner pid still holds the dev-start port on Windows", () => {
    const p = planTargetKill(OK_WIN_PRIMARY);
    assert.equal(p.ok, true);
    assert.equal(p.pid, 8812);
  });
  const OK_LINUX_TEMP = {
    ...OK_WIN_PRIMARY,
    gatePid: 149760,
    freshPid: 149760,
    imageName: "qontinui-runner",
    port: 9877,
    devStartExplicit: true,
    platform: "linux",
    doorRunnable: true,
    interpreter: "/home/u/.local/bin/pwsh",
    devStart: "/home/u/restart-9877.ps1",
  };
  test("ok: an explicit, runnable --dev-start reaches a temp runner on Linux", () => {
    const p = planTargetKill(OK_LINUX_TEMP);
    assert.equal(p.ok, true);
    assert.equal(p.pid, 149760);
  });
  test("refuses on Linux when the door is not proven runnable", () => {
    for (const doorRunnable of [false, undefined]) {
      const p = planTargetKill({ ...OK_LINUX_TEMP, doorRunnable, interpreter: null });
      assert.equal(p.ok, false);
      assert.equal(p.reason, "door-unrunnable");
      assert.match(p.detail, /powershell, pwsh/);
    }
  });
  test("refuses the Linux primary's port 9876 even with an explicit, runnable door", () => {
    const p = planTargetKill({ ...OK_LINUX_TEMP, port: 9876 });
    assert.equal(p.ok, false);
    assert.equal(p.reason, "primary-not-restartable");
  });
  const refusals = [
    ["no-gate-pid", { gatePid: null }],
    ["target-unresolved", { freshPid: null }],
    ["pid-changed", { freshPid: 9001 }],
    ["never-kill", { imageName: "node.exe" }],
    ["never-kill", { imageName: "pwsh" }],
    ["wrong-image", { imageName: "python3" }],
    ["wrong-image", { imageName: null }],
    ["port-not-managed", { port: 9877 }],
    ["no-restart-door", { platform: "linux" }],
    ["no-restart-door", { platform: "darwin", port: 9877 }],
    ["door-unrunnable", { doorRunnable: false }],
    ["primary-not-restartable", { platform: "linux", devStartExplicit: true }],
  ];
  for (const [reason, patch] of refusals) {
    test(`refuses: ${reason} ${JSON.stringify(patch)}`, () => {
      const p = planTargetKill({ ...OK_WIN_PRIMARY, ...patch });
      assert.equal(p.ok, false);
      assert.equal(p.reason, reason);
      assert.match(p.detail, /^REFUSED/);
    });
  }
});

// ---------------------------------------------------------------------------
// killTargetRunner — exactly one kill call on the ok path, none on refusals
// ---------------------------------------------------------------------------

function recordingKill() {
  const calls = [];
  const fn = (pid, signal) => {
    calls.push([pid, signal]);
  };
  return { calls, fn };
}

const resolvesTo = (r) => async () => ({
  pid: null,
  source: "fake",
  error: null,
  holderPid: null,
  imageName: null,
  ...r,
});

describe("killTargetRunner", () => {
  test("ok path: kill(gatePid, 'SIGKILL') exactly once", async () => {
    const k = recordingKill();
    const r = await killTargetRunner(8812, k.fn, {
      port: 9876,
      devStartPort: 9876,
      devStartExplicit: false,
      platform: "win32",
      doorRunnable: true,
      resolve: resolvesTo({ pid: 8812, holderPid: 8812, imageName: "qontinui-runner" }),
    });
    assert.equal(r.ok, true);
    assert.deepEqual(k.calls, [[8812, "SIGKILL"]]);
  });
  test("ok path through the real resolver on a fake /proc", async () => {
    const k = recordingKill();
    const io = fakeLinuxIo({
      tcp6: TCP6_LISTEN_9877,
      procs: { 149760: { fds: { 17: "socket:[7777]" }, exe: "/x/qontinui-runner" } },
    });
    const r = await killTargetRunner(149760, k.fn, {
      port: 9877,
      devStartPort: 9876,
      devStartExplicit: true,
      doorRunnable: true,
      interpreter: "/home/u/.local/bin/pwsh",
      devStart: "/home/u/restart-9877.ps1",
      io,
    });
    assert.equal(r.ok, true);
    assert.deepEqual(k.calls, [[149760, "SIGKILL"]]);
  });
  const refusalCases = [
    ["pid changed", 8812, { pid: 9001, holderPid: 9001, imageName: "qontinui-runner" }, {}],
    ["wrong image", 8812, { holderPid: 8812, imageName: "python3", error: "not a runner" }, {}],
    ["never-kill image", 8812, { holderPid: 8812, imageName: "node.exe", error: "x" }, {}],
    ["unresolved", 8812, { error: "no process holds a LISTEN socket on port 9876" }, {}],
    ["no gate pid", null, { pid: 8812, holderPid: 8812, imageName: "qontinui-runner" }, {}],
    [
      "port not managed",
      8812,
      { pid: 8812, holderPid: 8812, imageName: "qontinui-runner" },
      { port: 9877 },
    ],
    [
      "platform without the door",
      8812,
      { pid: 8812, holderPid: 8812, imageName: "qontinui-runner" },
      { platform: "linux" },
    ],
    [
      "door unrunnable (no interpreter on PATH)",
      8812,
      { pid: 8812, holderPid: 8812, imageName: "qontinui-runner" },
      { doorRunnable: false, interpreter: null },
    ],
    [
      "door runnability not stated (defaults to refuse)",
      8812,
      { pid: 8812, holderPid: 8812, imageName: "qontinui-runner" },
      { doorRunnable: undefined },
    ],
    [
      "linux primary port 9876 with an explicit door",
      8812,
      { pid: 8812, holderPid: 8812, imageName: "qontinui-runner" },
      { platform: "linux", devStartExplicit: true },
    ],
  ];
  for (const [label, gatePid, fresh, ctxPatch] of refusalCases) {
    test(`refusal never calls kill: ${label}`, async () => {
      const k = recordingKill();
      const r = await killTargetRunner(gatePid, k.fn, {
        port: 9876,
        devStartPort: 9876,
        devStartExplicit: false,
        platform: "win32",
        doorRunnable: true,
        resolve: resolvesTo(fresh),
        ...ctxPatch,
      });
      assert.equal(r.ok, false);
      assert.match(r.detail, /^REFUSED/);
      assert.deepEqual(k.calls, []);
    });
  }
  test("a throwing kill is reported, not swallowed as success", async () => {
    const r = await killTargetRunner(
      8812,
      () => {
        throw new Error("ESRCH");
      },
      {
        port: 9876,
        devStartPort: 9876,
        platform: "win32",
        doorRunnable: true,
        resolve: resolvesTo({ pid: 8812, holderPid: 8812, imageName: "qontinui-runner" }),
      },
    );
    assert.equal(r.ok, false);
    assert.match(r.detail, /ESRCH/);
  });
});

// ===========================================================================
// Phase 4 — eval-free fixtures
// ===========================================================================

/**
 * Claude Code v2.1.319 PTY bytes, captured 2026-11-08 and sanitised of host
 * and path. Raw escape sequences are kept on purpose: the decoder is part of
 * what these cases protect.
 */
const CAPS = JSON.parse(readFileSync(join(HERE, "fixtures", "uibridge-claude-gates.json"), "utf8"));
const TRUST = KNOWN_LAUNCH_GATES.find((g) => g.id === "folder-trust");
const MCP = KNOWN_LAUNCH_GATES.find((g) => g.id === "mcp-server");

describe("terminalBufferText", () => {
  test("cursor-column moves become spaces, so the menu reads as words", () => {
    const t = terminalBufferText(CAPS.trustInitial);
    assert.match(t, /Is this a project you created or one you trust\?/);
    assert.match(t, /❯ No, exit\n Yes, I trust this folder/);
    assert.doesNotMatch(t, /\x1b/);
  });
  test("a redraw after Down appends the moved cursor on one line", () => {
    assert.match(terminalBufferText(CAPS.trustAfterDown), /No, exit ❯ Yes, I trust this folder/);
  });
  test("OSC sequences (shell marks, window titles) are dropped whole", () => {
    assert.equal(terminalBufferText("a\x1b]633;P;Cwd=/x\x07b\x1b]0;title\x07c"), "abc");
  });
});

describe("planMenuAnswer", () => {
  test("trust: the cursor opens on 'No, exit', so ONE Down — never Enter", () => {
    const plan = planMenuAnswer(terminalBufferText(CAPS.trustInitial), TRUST);
    assert.deepEqual(plan, { action: "move", keys: ["\x1b[B"], cursor: "No, exit" });
  });
  test("trust: after the Down the re-read shows the cursor on the target, so Enter", () => {
    const plan = planMenuAnswer(terminalBufferText(CAPS.trustAfterDown), TRUST, {
      afterMove: true,
    });
    assert.deepEqual(plan, { action: "confirm", keys: ["\r"], cursor: "Yes, I trust this folder" });
  });
  test("MCP: the default cursor is already on 'Continue without…', so Enter at once", () => {
    const plan = planMenuAnswer(terminalBufferText(CAPS.mcpInitial), MCP);
    assert.deepEqual(plan, {
      action: "confirm",
      keys: ["\r"],
      cursor: "Continue without using this MCP server",
    });
  });
  test("a cursor not on the target after the arrows is refused, Enter withheld", () => {
    // The Down never landed: the re-read still shows the first render.
    const plan = planMenuAnswer(terminalBufferText(CAPS.trustInitial), TRUST, {
      afterMove: true,
    });
    assert.equal(plan.action, "refuse");
    assert.match(plan.reason, /cursor is on 'No, exit', not 'Yes, I trust this folder'/);
  });
  test("an unrecognised menu (an option renamed) is refused, not guessed", () => {
    const changed = terminalBufferText(CAPS.trustInitial).replace(
      "Yes, I trust this folder",
      "Yes, proceed",
    );
    const plan = planMenuAnswer(changed, TRUST);
    assert.equal(plan.action, "refuse");
    assert.match(plan.reason, /unrecognised menu/);
    assert.match(plan.reason, /'Yes, I trust this folder'/);
  });
  test("a menu with no cursor glyph is refused", () => {
    const plan = planMenuAnswer(terminalBufferText(CAPS.trustInitial).replace("❯", " "), TRUST);
    assert.equal(plan.action, "refuse");
    assert.match(plan.reason, /no ❯ cursor/);
  });
  test("the cursor on a line that is no option is refused", () => {
    const tail = `${terminalBufferText(CAPS.mcpInitial)}\n ❯ something else\n`;
    const plan = planMenuAnswer(tail, MCP);
    assert.equal(plan.action, "refuse");
    assert.match(plan.reason, /cursor is on 'something else'/);
  });
  test("an already-answered gate in scrollback is not answered again", () => {
    const all = terminalBufferText(CAPS.trustAfterDown + CAPS.mcpInitial + CAPS.bannerAfterGates);
    // Recorded as answered by the caller:
    assert.deepEqual(planMenuAnswer(all, MCP, { answered: new Set(["mcp-server"]) }), {
      action: "none",
      reason: "mcp-server already answered",
    });
    // And by the tail alone: a later gate was drawn after the trust menu.
    const trust = planMenuAnswer(all, TRUST);
    assert.equal(trust.action, "none");
    assert.match(trust.reason, /folder-trust is scrollback: mcp-server was drawn after it/);
  });
  test("a gate that is not showing is none", () => {
    assert.equal(planMenuAnswer("$ ls\n", TRUST).action, "none");
  });
  test("the external CLAUDE.md imports gate stays refused", () => {
    const r = refusedGateIn("Allow external CLAUDE.md file imports?\n ❯ Yes\n No");
    assert.equal(r?.id, "external-claude-md-imports");
    assert.equal(refusedGateIn(terminalBufferText(CAPS.trustInitial)), null);
  });
});

describe("buildFixtureTerminalRequest", () => {
  test("M1: a bare 'claude', account-pinned by the runner", () => {
    assert.deepEqual(
      buildFixtureTerminalRequest({
        pageId: "page-1",
        title: "uibridge-fixture-a",
        fixtureCwd: "/tmp/qontinui-uibridge-fixtures-x",
        account: "tiohorst",
      }),
      {
        page_id: "page-1",
        title: "uibridge-fixture-a",
        working_dir: "/tmp/qontinui-uibridge-fixtures-x",
        account: "tiohorst",
        initial_command: "claude",
      },
    );
  });
});

describe("pickFixtureSessionId", () => {
  const rows = [
    { claudeSessionId: "s-other", terminalId: "t-9", origin: "authoritative", confirmed: true },
    { claudeSessionId: "s-1", terminalId: "t-1", origin: "authoritative", confirmed: true },
  ];
  test("one row for the terminal -> its session id", () => {
    const r = pickFixtureSessionId(rows, "t-1");
    assert.equal(r.ok, true);
    assert.equal(r.sessionId, "s-1");
  });
  test("no row yet -> pending, not an error", () => {
    const r = pickFixtureSessionId(rows, "t-2");
    assert.equal(r.ok, false);
    assert.equal(r.pending, true);
  });
  test("two rows for one terminal -> refused, naming both", () => {
    const r = pickFixtureSessionId(
      [...rows, { claudeSessionId: "s-phantom", terminalId: "t-1", origin: "observed" }],
      "t-1",
    );
    assert.equal(r.ok, false);
    assert.equal(r.pending, false);
    assert.match(r.error, /2 open records name terminal t-1/);
    assert.match(r.error, /s-1/);
    assert.match(r.error, /s-phantom/);
  });
});

describe("parseZoneViewState", () => {
  const full = {
    maximizedZone: null,
    zoneCount: 4,
    layoutId: "quad",
    pageId: "page-1",
    assignments: { 0: "t-a", 1: "t-b", 2: "t-c" },
    workMode: "grid",
  };
  test("present: page, grid map and mode are read", () => {
    assert.deepEqual(parseZoneViewState(full, "b1"), {
      ok: true,
      view: {
        pageId: "page-1",
        assignments: { 0: "t-a", 1: "t-b", 2: "t-c" },
        workMode: "grid",
        zoneCount: 4,
        layoutId: "quad",
        maximizedZone: null,
      },
    });
  });
  test("absent pageId (today's envelope) refuses, naming the build", () => {
    const r = parseZoneViewState(
      { layoutId: "single", maximizedZone: null, zoneCount: 1 },
      "1730000000-abc",
    );
    assert.equal(r.ok, false);
    assert.match(r.error, /carries no pageId, assignments, workMode/);
    assert.match(r.error, /runner build 1730000000-abc predates/);
  });
  test("absent assignments refuses", () => {
    const { assignments: _drop, ...noAssign } = full;
    const r = parseZoneViewState(noAssign, "b2");
    assert.equal(r.ok, false);
    assert.match(r.error, /carries no assignments — runner build b2/);
  });
  test("tmux work mode refuses fixture creation", () => {
    const r = parseZoneViewState({ ...full, workMode: "tmux" }, "b3");
    assert.equal(r.ok, false);
    assert.equal(r.tmux, true);
    assert.match(r.error, /tmux work mode/);
  });
  test("zoneIndexOf reads the rendered zone from the grid map", () => {
    assert.equal(zoneIndexOf(full.assignments, "t-c"), 2);
    assert.equal(zoneIndexOf(full.assignments, "t-z"), null);
  });
});

describe("account roster", () => {
  test("deriveAccountName / accountInRoster mirror the runner's matching", () => {
    assert.equal(deriveAccountName("/home/u/.claude-tiohorst"), "tiohorst");
    assert.equal(deriveAccountName("C:\\claude\\.claude-hotmail"), "hotmail");
    assert.equal(deriveAccountName("/home/u/.claude"), "default");
    assert.equal(accountInRoster("TIOHORST", ["/home/u/.claude-tiohorst"]), true);
    assert.equal(accountInRoster(".claude-tiohorst", ["/home/u/.claude-tiohorst"]), true);
    assert.equal(accountInRoster("jan", ["/home/u/.claude-tiohorst"]), false);
  });
  test("an EMPTY roster (a fresh temp runner) is detected and refused, naming the flag", () => {
    const p = planRosterSeed({
      current: [],
      accounts: ["tiohorst", "jan"],
      seedDirs: [],
      port: 9877,
      protectedPort: 9876,
    });
    assert.equal(p.action, "refuse");
    assert.deepEqual(p.missing, ["tiohorst", "jan"]);
    assert.match(p.error, /roster \(EMPTY\)/);
    assert.match(p.error, /--seed-roster/);
  });
  test("every account present -> nothing to do", () => {
    const p = planRosterSeed({
      current: ["/h/.claude-tiohorst", "/h/.claude-jan"],
      accounts: ["tiohorst", "jan"],
      seedDirs: ["/h/.claude-x"],
      port: 9877,
      protectedPort: 9876,
    });
    assert.equal(p.action, "none");
  });
  test("--seed-roster: the union, prior recorded for teardown", () => {
    const p = planRosterSeed({
      current: ["/h/.claude-tiohorst"],
      accounts: ["tiohorst", "jan"],
      seedDirs: ["/h/.claude-jan", "/h/.claude-tiohorst"],
      port: 9877,
      protectedPort: 9876,
    });
    assert.deepEqual(p, {
      action: "seed",
      missing: ["jan"],
      prior: ["/h/.claude-tiohorst"],
      next: ["/h/.claude-tiohorst", "/h/.claude-jan"],
    });
  });
  test("--seed-roster on the primary's port is refused", () => {
    const p = planRosterSeed({
      current: [],
      accounts: ["jan"],
      seedDirs: ["/h/.claude-jan"],
      port: 9876,
      protectedPort: 9876,
    });
    assert.equal(p.action, "refuse");
    assert.match(p.error, /primary runner's port/);
  });
  test("the read-back catches a dir the PUT silently dropped", () => {
    const v = verifyRosterSeed({
      accepted: ["/h/.claude-tiohorst"],
      requested: ["/h/.claude-tiohorst", "/h/.claude-jan"],
      accounts: ["tiohorst", "jan"],
    });
    assert.equal(v.ok, false);
    assert.deepEqual(v.dropped, ["/h/.claude-jan"]);
    assert.match(v.error, /no projects\/ subdirectory/);
    assert.equal(sameRoster(["b", "a"], ["a", "b"]), true);
    assert.equal(sameRoster([], ["a"]), false);
  });
});

describe("spawn admission", () => {
  const guard = {
    status: 500,
    json: { success: false, error: "resource_guard:critical:thread_count: 31950 >= 30000" },
  };
  test("a resource-guard 500 (an older runner build) is retried as admission", () => {
    assert.deepEqual(classifyTerminalSpawn(guard), {
      kind: "admission",
      retry: true,
      detail: "resource_guard:critical:thread_count: 31950 >= 30000",
    });
  });
  // The shape a current runner answers: `spawn_refusal_answer` and the
  // pre-allocation `precheck_spawn` in src-tauri/src/mcp/terminals.rs.
  const guard503 = {
    status: 503,
    json: {
      success: false,
      error: "resource_guard:critical:thread_count: 31950 >= 30000",
      code: "RESOURCE_GUARD_CRITICAL",
    },
  };
  test("a typed 503 RESOURCE_GUARD_CRITICAL is retried as admission", () => {
    assert.deepEqual(classifyTerminalSpawn(guard503), {
      kind: "admission",
      retry: true,
      detail: "RESOURCE_GUARD_CRITICAL: resource_guard:critical:thread_count: 31950 >= 30000",
    });
    // The code alone decides, whatever the message text says.
    assert.equal(
      classifyTerminalSpawn({
        status: 503,
        json: { error: "spawn refused", code: "RESOURCE_GUARD_CRITICAL" },
      }).retry,
      true,
    );
    assert.equal(
      planSpawnRetry({ classification: classifyTerminalSpawn(guard503), attempt: 0, elapsedMs: 0 })
        .retry,
      true,
    );
  });
  test("a 503 SPAWN_BLOCKED_DIAL_UNREAD is retried; a 409 SPAWN_BLOCKED is not", () => {
    const dial = classifyTerminalSpawn({
      status: 503,
      json: { error: "trust gate: tenant dial unread", code: "SPAWN_BLOCKED_DIAL_UNREAD" },
    });
    assert.equal(dial.kind, "dial-unread");
    assert.equal(dial.retry, true);
    const blocked = classifyTerminalSpawn({
      status: 409,
      json: { error: "trust gate: workspace not trusted", code: "SPAWN_BLOCKED" },
    });
    assert.equal(blocked.kind, "refused");
    assert.equal(blocked.retry, false);
    assert.match(blocked.detail, /^SPAWN_BLOCKED: /);
    // A 503 with any other code, or none, is not an admission refusal.
    assert.equal(
      classifyTerminalSpawn({ status: 503, json: { error: "x", code: "SOMETHING_ELSE" } }).retry,
      false,
    );
    assert.equal(classifyTerminalSpawn({ status: 503, json: { error: "x" } }).kind, "error");
  });
  test("roster 400, not-logged-in 409 and any other 500 are not retried", () => {
    assert.equal(
      classifyTerminalSpawn({
        status: 400,
        json: { error: "account 'jan' not in roster; available: " },
      }).kind,
      "roster",
    );
    assert.equal(
      classifyTerminalSpawn({ status: 409, json: { error: "x" } }).kind,
      "not-logged-in",
    );
    const other = classifyTerminalSpawn({ status: 500, json: { error: "pty spawn failed" } });
    assert.equal(other.kind, "error");
    assert.equal(other.retry, false);
    assert.equal(classifyTerminalSpawn({ status: 200 }).kind, "ok");
  });
  test("bounded backoff, then the budget expires with the reason named", () => {
    const c = classifyTerminalSpawn(guard);
    assert.deepEqual(planSpawnRetry({ classification: c, attempt: 0, elapsedMs: 0 }), {
      retry: true,
      delayMs: 2000,
    });
    assert.deepEqual(planSpawnRetry({ classification: c, attempt: 5, elapsedMs: 10_000 }), {
      retry: true,
      delayMs: 15000,
    });
    const out = planSpawnRetry({
      classification: c,
      attempt: 9,
      elapsedMs: 110_000,
      admissionVerdict: "critical",
    });
    assert.equal(out.retry, false);
    assert.match(out.reason, /kept refusing for 110s/);
    assert.match(out.reason, /thread_count/);
    assert.match(out.reason, /verdict=critical/);
  });
  test("request + /health time counts against the budget, not just the sleeps", () => {
    // 100s elapsed + 15s sleep fits 120s alone; a 10s attempt on top does not.
    const c = classifyTerminalSpawn(guard);
    assert.equal(planSpawnRetry({ classification: c, attempt: 5, elapsedMs: 100_000 }).retry, true);
    assert.equal(
      planSpawnRetry({ classification: c, attempt: 5, elapsedMs: 100_000, lastAttemptMs: 10_000 })
        .retry,
      false,
    );
  });
  test("a non-retryable refusal never schedules a retry", () => {
    const r = planSpawnRetry({
      classification: classifyTerminalSpawn({ status: 409, json: { error: "logged out" } }),
      attempt: 0,
      elapsedMs: 0,
    });
    assert.equal(r.retry, false);
  });
});

describe("openResidue", () => {
  test("names every owned record still open, by session or by terminal", () => {
    const rows = [
      { claudeSessionId: "s-1", terminalId: "t-1" },
      { claudeSessionId: "s-new", terminalId: "t-2" },
      { claudeSessionId: "s-foreign", terminalId: "t-9" },
    ];
    assert.deepEqual(
      openResidue(rows, { sessionIds: new Set(["s-1"]), terminalIds: new Set(["t-2"]) }),
      ["s-1@t-1", "s-new@t-2"],
    );
    assert.deepEqual(openResidue([], { sessionIds: new Set(["s-1"]), terminalIds: new Set() }), []);
  });
});

describe("no script evaluation in the webview", () => {
  // The exe-mode CSP refuses string eval; the oracle builds fixtures over
  // HTTP and read-only component actions only (plan D5). The needles are
  // assembled so this file does not itself contain them.
  const needles = ["/ui-bridge/control/page/" + "evaluate", "__TA" + "URI__"];
  for (const file of ["uibridge-session-info.test.mjs", "uibridge-session-info-lib.mjs"]) {
    test(`${file} never names the evaluate door or the Tauri global`, () => {
      const src = readFileSync(join(HERE, "..", file), "utf8");
      for (const n of needles) assert.equal(src.includes(n), false, `${file} contains ${n}`);
    });
  }
});

describe("the oracle kills only the target (static)", () => {
  // Plan D3/D4. Needles assembled so this file does not itself contain them.
  // Against ba08d28f1's oracle this fails: it enumerated runners by name and
  // stopped every pid listed, and gated on any runner ancestor.
  const src = () => readFileSync(join(HERE, "..", "uibridge-session-info.test.mjs"), "utf8");
  for (const forbidden of ["Get-Process " + "-Name", "Stop-" + "Process", "task" + "kill"]) {
    test(`the oracle source never contains '${forbidden}' outside its doc comments`, () => {
      // Doc comments may NAME the forbidden spellings (to forbid them); code may not.
      const code = src()
        .replace(/\/\*[\s\S]*?\*\//g, "")
        .replace(/^\s*\/\/.*$/gm, "");
      assert.equal(code.includes(forbidden), false, `oracle code contains ${forbidden}`);
    });
  }
  test("T10 kills through killTargetRunner with the gate's pid", () => {
    assert.equal(src().includes("killTargetRunner(" + "gate.pid"), true);
  });
  test("the self-hosting gate decides through selfHostingVerdict", () => {
    const s = src();
    assert.equal(s.includes("return selfHosting" + "Verdict("), true);
    assert.equal(s.includes("resolveTargetRunner" + "Pid(port)"), true);
  });
});

// ===========================================================================
// Phase 5 — the per-zone tenant badge (T0.2b, T2b)
// ===========================================================================

const T_STAMP = "c231d9da-1111-4222-8333-444455556666";
const T_ACT = "7e0b1a2c-aaaa-4bbb-8ccc-dddddddddddd";
const T_CRED = "99999999-0000-4000-8000-000000000000";

/** A tenancy block shaped like `/control/sessions/info`'s, minus legs no rule reads. */
function tenancy({ stamp = null, diverged = false, display = null } = {}) {
  return { row: { tenantId: stamp }, diverged, display };
}
const DISPLAY_RESOLVED = {
  status: "resolved",
  tenantId: T_ACT,
  source: "credential",
  conflicts: [],
};
const DISPLAY_STAMP_ONLY = {
  status: "stamp_only",
  tenantId: T_STAMP,
  source: "row_stamp",
  conflicts: [],
};
const DISPLAY_UNKNOWN = { status: "unknown", tenantId: null, source: null, conflicts: [] };

describe("tenantBadgeElementId", () => {
  test("the per-zone id shape TenantBadge.tsx owns", () => {
    assert.equal(tenantBadgeElementId(0), "terminal-tenant-badge-0");
    assert.equal(tenantBadgeElementId(7), "terminal-tenant-badge-7");
  });
});

describe("tenantBadgeExpectation", () => {
  test("present: showSwitcher and a spawn stamp", () => {
    const e = tenantBadgeExpectation({
      pickerPresent: true,
      tenancy: tenancy({ stamp: T_STAMP, display: DISPLAY_STAMP_ONLY }),
    });
    assert.equal(e.expect, "present");
    assert.match(e.reason, /a spawn stamp/);
  });
  test("present: showSwitcher and a divergence (no stamp, no resolved tenant)", () => {
    const e = tenantBadgeExpectation({
      pickerPresent: true,
      tenancy: tenancy({ diverged: true, display: DISPLAY_UNKNOWN }),
    });
    assert.equal(e.expect, "present");
    assert.match(e.reason, /a divergence/);
  });
  test("present: showSwitcher and a RESOLVED acting tenant (no stamp)", () => {
    const e = tenantBadgeExpectation({
      pickerPresent: true,
      tenancy: tenancy({ display: DISPLAY_RESOLVED }),
    });
    assert.equal(e.expect, "present");
    assert.match(e.reason, /a resolved acting tenant/);
  });
  test("absent: no picker (<= 1 binding), whatever the tenancy says", () => {
    const e = tenantBadgeExpectation({
      pickerPresent: false,
      tenancy: tenancy({ stamp: T_STAMP, display: DISPLAY_RESOLVED }),
    });
    assert.equal(e.expect, "absent");
  });
  test("absent: showSwitcher, but nothing names a tenant", () => {
    const e = tenantBadgeExpectation({
      pickerPresent: true,
      tenancy: tenancy({ display: DISPLAY_UNKNOWN }),
    });
    assert.equal(e.expect, "absent");
    assert.match(e.reason, /display.status=unknown/);
  });
  test("unknown: the picker's presence could not be read", () => {
    assert.equal(
      tenantBadgeExpectation({ pickerPresent: null, tenancy: tenancy({ stamp: T_STAMP }) }).expect,
      "unknown",
    );
  });
  test("unknown: no tenancy block", () => {
    assert.equal(tenantBadgeExpectation({ pickerPresent: true, tenancy: null }).expect, "unknown");
  });
  test("unknown: nothing to say and tenancy.display absent (runner predates it)", () => {
    const e = tenantBadgeExpectation({ pickerPresent: true, tenancy: tenancy({ display: null }) });
    assert.equal(e.expect, "unknown");
    assert.match(e.reason, /predates/);
  });
});

/** A registry element as `GET /ui-bridge/control/element/<id>` serves it. */
function badge({ label, dataset }) {
  return { id: "terminal-tenant-badge-0", label, state: { dataset } };
}
const PRESENT = { expect: "present", reason: "test" };
const statuses = (r) => Object.fromEntries(r.findings.map((f) => [f.check, f.status]));

describe("compareBadgeElement", () => {
  test("resolved acting tenant: label, diverged and acting all PASS", () => {
    const t = tenancy({ display: DISPLAY_RESOLVED });
    const el = badge({
      label: `This session acts as tenant ${T_ACT} via its coord credential.`,
      dataset: { tenantDiverged: "false", tenantDisplayStatus: "resolved", tenantActing: T_ACT },
    });
    const r = compareBadgeElement(el, PRESENT, t, { build: "b1" });
    assert.equal(r.status, "PASS");
    assert.deepEqual(statuses(r), {
      presence: "PASS",
      label: "PASS",
      diverged: "PASS",
      acting: "PASS",
    });
  });
  test("row_stamp source: data-tenant-acting must be ABSENT — present is a FAIL", () => {
    const t = tenancy({ stamp: T_STAMP, display: DISPLAY_STAMP_ONLY });
    const ok = badge({
      label: `This session is acting as tenant ${T_STAMP}.`,
      dataset: { tenantDiverged: "false", tenantDisplayStatus: "stamp_only" },
    });
    assert.equal(compareBadgeElement(ok, PRESENT, t).status, "PASS");
    const bad = badge({
      label: `This session is acting as tenant ${T_STAMP}.`,
      dataset: {
        tenantDiverged: "false",
        tenantDisplayStatus: "stamp_only",
        tenantActing: T_STAMP,
      },
    });
    const r = compareBadgeElement(bad, PRESENT, t);
    assert.equal(r.status, "FAIL");
    assert.equal(statuses(r).acting, "FAIL");
    assert.match(
      r.findings.find((f) => f.check === "acting").detail,
      /must be ABSENT \(display.source=row_stamp\)/,
    );
  });
  test("acting value differing from display.tenantId is a FAIL", () => {
    const r = compareBadgeElement(
      badge({
        label: `acts as tenant ${T_ACT}`,
        dataset: { tenantDiverged: "false", tenantDisplayStatus: "resolved", tenantActing: T_CRED },
      }),
      PRESENT,
      tenancy({ display: DISPLAY_RESOLVED }),
    );
    assert.equal(statuses(r).acting, "FAIL");
  });
  test("a build predating #2510 (no data-tenant-display-status) is UNKNOWN naming the build, never PASS", () => {
    const r = compareBadgeElement(
      badge({ label: `acts as tenant ${T_ACT}`, dataset: { tenantDiverged: "false" } }),
      PRESENT,
      tenancy({ display: DISPLAY_RESOLVED }),
      { build: "1731000000-ce1235d33" },
    );
    assert.equal(r.status, "UNKNOWN");
    assert.equal(statuses(r).acting, "UNKNOWN");
    assert.match(
      r.findings.find((f) => f.check === "acting").detail,
      /build 1731000000-ce1235d33 predates the badge display attributes \(PR #2787, which carried #2510\)/,
    );
  });
  test("diverged session: the label must start TENANT MISMATCH", () => {
    const t = tenancy({
      stamp: T_STAMP,
      diverged: true,
      display: { ...DISPLAY_RESOLVED, status: "mismatch" },
    });
    const good = badge({
      label: "TENANT MISMATCH — this session does not act as one tenant.",
      dataset: { tenantDiverged: "true", tenantDisplayStatus: "mismatch", tenantActing: T_ACT },
    });
    assert.equal(compareBadgeElement(good, PRESENT, t).status, "PASS");
    const bad = badge({
      label: `This session is acting as tenant ${T_STAMP}.`,
      dataset: { tenantDiverged: "true", tenantDisplayStatus: "mismatch", tenantActing: T_ACT },
    });
    assert.equal(statuses(compareBadgeElement(bad, PRESENT, t)).label, "FAIL");
  });
  test("data-tenant-diverged disagreeing with tenancy.diverged is a FAIL", () => {
    const r = compareBadgeElement(
      badge({
        label: `acting as tenant ${T_STAMP}`,
        dataset: { tenantDiverged: "true", tenantDisplayStatus: "stamp_only" },
      }),
      PRESENT,
      tenancy({ stamp: T_STAMP, display: DISPLAY_STAMP_ONLY }),
    );
    assert.equal(statuses(r).diverged, "FAIL");
  });
  test("a label not naming the stamped tenant is a FAIL", () => {
    const r = compareBadgeElement(
      badge({
        label: `acting as tenant ${T_CRED}`,
        dataset: { tenantDiverged: "false", tenantDisplayStatus: "stamp_only" },
      }),
      PRESENT,
      tenancy({ stamp: T_STAMP, display: DISPLAY_STAMP_ONLY }),
    );
    assert.equal(statuses(r).label, "FAIL");
  });
  test("expected present but not rendered is a FAIL; expected absent but rendered is a FAIL", () => {
    assert.equal(
      compareBadgeElement(null, PRESENT, tenancy({ display: DISPLAY_RESOLVED })).status,
      "FAIL",
    );
    const absent = { expect: "absent", reason: "no picker" };
    assert.equal(
      compareBadgeElement(badge({ label: "x", dataset: {} }), absent, null).status,
      "FAIL",
    );
    assert.equal(compareBadgeElement(null, absent, null).status, "PASS");
  });
  test("an unknown expectation renders the arm UNKNOWN, never PASS or FAIL", () => {
    const r = compareBadgeElement(null, { expect: "unknown", reason: "picker unread" }, null);
    assert.equal(r.status, "UNKNOWN");
    assert.match(r.findings[0].detail, /picker unread/);
  });
});

describe("badgeCardinality", () => {
  const two = [
    { zoneIndex: 0, expect: "present" },
    { zoneIndex: 1, expect: "present" },
  ];
  test("FAILS on one id shared by two zones (the measured defect)", () => {
    const r = badgeCardinality({
      registryIds: ["terminal-zone-header-0", "terminal-zone-header-1", "terminal.tenant-badge"],
      zones: two,
    });
    assert.equal(r.ok, false);
    assert.match(r.detail, /shared pre-per-zone id 'terminal.tenant-badge'/);
    assert.match(r.detail, /1 distinct badge id\(s\) \[terminal.tenant-badge\] for 2 zone\(s\)/);
  });
  test("FAILS on a duplicated per-zone id", () => {
    const r = badgeCardinality({
      registryIds: ["terminal-tenant-badge-0", "terminal-tenant-badge-0"],
      zones: two,
    });
    assert.equal(r.ok, false);
    assert.match(r.detail, /duplicated badge id\(s\): terminal-tenant-badge-0/);
  });
  test("passes with one distinct per-zone id per zone expecting a badge", () => {
    const r = badgeCardinality({
      registryIds: ["terminal-tenant-badge-0", "terminal-tenant-badge-1", "terminal-zone-header-0"],
      zones: [...two, { zoneIndex: 2, expect: "absent" }],
    });
    assert.deepEqual(r, {
      ok: true,
      detail: "2 distinct per-zone badge id(s) for 2 zone(s)",
      unboundBadges: [],
    });
  });
  test("a badge on a zone that expects none is a FAIL", () => {
    const r = badgeCardinality({
      registryIds: ["terminal-tenant-badge-0", "terminal-tenant-badge-1"],
      zones: [
        { zoneIndex: 0, expect: "present" },
        { zoneIndex: 1, expect: "absent" },
      ],
    });
    assert.equal(r.ok, false);
  });
  test("a badge on a zone with no bound session yet is set aside as UNKNOWN, not a count FAIL", () => {
    const r = badgeCardinality({
      registryIds: ["terminal-tenant-badge-0", "terminal-tenant-badge-1"],
      zones: [{ zoneIndex: 0, expect: "present" }],
      unboundZones: [1],
    });
    assert.equal(r.ok, true);
    assert.deepEqual(r.unboundBadges, ["terminal-tenant-badge-1"]);
    // Without the unbound set the same registry is a count FAIL.
    const without = badgeCardinality({
      registryIds: ["terminal-tenant-badge-0", "terminal-tenant-badge-1"],
      zones: [{ zoneIndex: 0, expect: "present" }],
    });
    assert.equal(without.ok, false);
    assert.deepEqual(without.unboundBadges, []);
  });
  test("an UNKNOWN zone expectation makes the count UNKNOWN", () => {
    const r = badgeCardinality({
      registryIds: ["terminal-tenant-badge-0"],
      zones: [
        { zoneIndex: 0, expect: "present" },
        { zoneIndex: 1, expect: "unknown" },
      ],
    });
    assert.equal(r.ok, null);
  });
});
