/**
 * Unit tests for the testable half of the session-info oracle.
 *
 *   node --test scripts/__tests__/uibridge-session-info-lib.test.mjs
 *
 * No runner, no `/proc`, no PowerShell: every OS read is an injected fake.
 * Expectations are literals (pids, image names, refusal reasons), never values
 * recomputed with the helper under test.
 *
 * Plan: `2026-11-08-tenant-badge-parity-and-oracle-fixtures` Phase 3.
 */

import { test, describe } from "node:test";
import assert from "node:assert/strict";

import {
  imageBasename,
  isRunnerImage,
  killTargetRunner,
  listenerInodeForPort,
  planTargetKill,
  resolveTargetRunnerPid,
  selfHostingVerdict,
} from "../uibridge-session-info-lib.mjs";

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
};

describe("planTargetKill", () => {
  test("ok: the same runner pid still holds the dev-start port on Windows", () => {
    const p = planTargetKill(OK_WIN_PRIMARY);
    assert.equal(p.ok, true);
    assert.equal(p.pid, 8812);
  });
  test("ok: an explicit --dev-start reaches a temp runner on Linux", () => {
    const p = planTargetKill({
      ...OK_WIN_PRIMARY,
      gatePid: 149760,
      freshPid: 149760,
      imageName: "qontinui-runner",
      port: 9877,
      devStartExplicit: true,
      platform: "linux",
    });
    assert.equal(p.ok, true);
    assert.equal(p.pid, 149760);
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
      resolve: resolvesTo({ pid: 8812, holderPid: 8812, imageName: "qontinui-runner" }),
    });
    assert.equal(r.ok, true);
    assert.deepEqual(k.calls, [[8812, "SIGKILL"]]);
  });
  test("ok path through the real resolver on a fake /proc", async () => {
    const k = recordingKill();
    const io = fakeLinuxIo({
      tcp: TCP_LISTEN_AND_ESTABLISHED,
      procs: { 149760: { fds: { 17: "socket:[4242]" }, exe: "/x/qontinui-runner" } },
    });
    const r = await killTargetRunner(149760, k.fn, {
      port: 9876,
      devStartPort: 9876,
      devStartExplicit: true,
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
  ];
  for (const [label, gatePid, fresh, ctxPatch] of refusalCases) {
    test(`refusal never calls kill: ${label}`, async () => {
      const k = recordingKill();
      const r = await killTargetRunner(gatePid, k.fn, {
        port: 9876,
        devStartPort: 9876,
        devStartExplicit: false,
        platform: "win32",
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
        resolve: resolvesTo({ pid: 8812, holderPid: 8812, imageName: "qontinui-runner" }),
      },
    );
    assert.equal(r.ok, false);
    assert.match(r.detail, /ESRCH/);
  });
});
