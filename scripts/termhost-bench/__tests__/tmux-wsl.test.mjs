/**
 * The `tmux-wsl` setup (`adapters/tmux-wsl.mjs`, `wsl.mjs`, `wsl-sampler.mjs`)
 * and the `windows-desktop` environment without Windows or WSL: the doctor's WSL
 * verdicts, the `wsl.exe --exec` argv of every tmux command, the generator argv
 * translated into the distro with its flags unchanged (D13), the clock offset
 * handed to the panes through the private server's environment, the samplers,
 * the KPIs they feed (the WSL-side tmux server joined into the host, the shared
 * VM recorded apart), teardown that reports what it cannot prove instead of
 * signalling it, and the generator's clock offset.
 *
 *   node --test scripts/termhost-bench/__tests__/tmux-wsl.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { EventEmitter } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { AdapterRefusal } from "../adapters/index.mjs";
import { CLOCK_OFFSET_ENV, CLOCK_ROUNDS, MAX_CLOCK_RTT_NS, TmuxWslAdapter, wslClockVerdict, WSL_FLAG_SCAN, attachCommand, samplerArgv } from "../adapters/tmux-wsl.mjs";
import { REQUIRED_FILES, WT_PIN, wtBuildFor } from "../adapters/windows-terminal-install.mjs";
import { blockingMechanisms, mapDoctor } from "../doctor.mjs";
import { desktopVerdict, startWindowsDesktop } from "../envs/windows-desktop.mjs";
import { CLOCK_OFFSET_ENV as GEN_OFFSET_ENV, clockOffsetFromEnv, createRealClock, runVirtual } from "../generator/ai-session.mjs";
import { computeRowKpis, generatorWindow, indexGenerators } from "../kpis.mjs";
import { loadProfile } from "../profile.mjs";
import { generatorArgv } from "../rundir.mjs";
import {
  CLOCK_ECHO_SCRIPT,
  VM_PROCESS_QUERY,
  VM_SAMPLER_SCRIPT,
  chooseDistro,
  estimateClockOffset,
  isWindowsPath,
  mapWslFacts,
  measureClockOffset,
  parseWslList,
  pickVmProcess,
  toWslPath,
  vmKpis,
  wslGeneratorArgv,
  wslHostKpis,
  wslInvocation,
  wslRowAdjustments,
} from "../wsl.mjs";
import { NeutralityGuard } from "../neutrality.mjs";
import { machineBlock } from "../machine.mjs";
import { runBench } from "../run.mjs";
import { validateResults } from "../validate.mjs";
import { incident } from "../results.mjs";
import { classify, cpuTicks, groupReading, parseArgs as samplerParseArgs, pssKb, readReadings, statReading } from "../wsl-sampler.mjs";
import { MS, genLog, operativeCapabilities, watchSegment } from "./helpers/bench-fixtures.mjs";
import { createFakeWt } from "./helpers/fake-wt.mjs";
import { FakeAdapter, VirtualClock, fakeProbe, fakeRunOptions, healthyFacts } from "./helpers/fake-bench.mjs";

const WIN_ENV = { SystemRoot: "C:\\Windows", PATH: "C:\\Windows\\System32", LOCALAPPDATA: "C:\\Users\\bench\\AppData\\Local" };
const INOP = "INOPERATIVE-ON-THIS-MACHINE";

function tmp(t, prefix = "tb-wsl-") {
  const d = mkdtempSync(join(tmpdir(), prefix));
  t.after(() => rmSync(d, { recursive: true, force: true }));
  return d;
}

// ---------------------------------------------------------------------------
// The distro, and the doctor's verdicts
// ---------------------------------------------------------------------------

test("wsl.exe -l -v is UTF-16LE: parsed with or without a BOM, the default marked", () => {
  const text = "  NAME            STATE           VERSION\r\n* Ubuntu-24.04    Running         2\r\n  Debian          Stopped         1\r\n";
  for (const buf of [Buffer.from(text, "utf16le"), Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, "utf16le")]), text]) {
    const l = parseWslList(buf);
    assert.deepEqual(l, [
      { name: "Ubuntu-24.04", isDefault: true, state: "Running", version: 2 },
      { name: "Debian", isDefault: false, state: "Stopped", version: 1 },
    ]);
  }
  const l = parseWslList(text);
  assert.equal(chooseDistro(l).name, "Ubuntu-24.04");
  assert.equal(chooseDistro(l, "Debian").name, "Debian");
  assert.equal(chooseDistro(l, "Arch"), null);
});

test("the doctor checks the distro (WSL 2), tmux and node inside it; an absence is INOPERATIVE, an unasked tool UNKNOWN", () => {
  const list = [{ name: "Ubuntu", isDefault: true, state: "Running", version: 2 }];
  const ok = mapWslFacts({ list, tmux: "tmux 3.4", node: "v22.11.0", clkTck: "100", bash: true });
  assert.deepEqual(Object.values(ok).map((x) => x.state), ["OPERATIVE", "OPERATIVE", "OPERATIVE"]);
  const none = mapWslFacts({ list: null, listError: "ENOENT" });
  assert.equal(none["setup.wsl"].state, INOP);
  assert.equal(none["setup.wsl_tmux"].state, "UNKNOWN");
  assert.match(mapWslFacts({ list, wanted: "Arch" })["setup.wsl"].reason, /no WSL distro named Arch \(installed: Ubuntu\)/);
  assert.match(mapWslFacts({ list: [{ ...list[0], isDefault: false }] })["setup.wsl"].reason, /--wsl-distro/);
  assert.equal(mapWslFacts({ list: [{ ...list[0], version: 1 }], tmux: "tmux 3.4", node: "v22.0.0" })["setup.wsl"].state, INOP);
  assert.equal(mapWslFacts({ list, tmux: null, node: "v22.0.0" })["setup.wsl_tmux"].state, INOP);
  assert.match(mapWslFacts({ list, tmux: "tmux 3.4", node: "v18.19.0" })["setup.wsl_node"].reason, /older than v20/);
  assert.equal(mapWslFacts({ list, tmux: "tmux 3.4", node: null })["setup.wsl_node"].state, INOP);
  assert.equal(mapWslFacts({ list, tmux: "tmux 3.4", node: "v22.0.0", bash: false })["setup.wsl"].state, INOP);
});

test("every WSL command is wsl.exe -d <distro> --exec <argv>: no shell inside the distro reinterprets an argument", () => {
  assert.deepEqual(wslInvocation("Ubuntu", ["tmux", "-L", "s", "kill-server"]), ["wsl.exe", ["-d", "Ubuntu", "--exec", "tmux", "-L", "s", "kill-server"]]);
  assert.throws(() => wslInvocation("", ["x"]), /needs a distro/);
});

test("D13 inside WSL: the distro's node, Windows paths translated, every flag and its order unchanged", () => {
  assert.equal(toWslPath("C:\\Users\\a b\\gen"), "/mnt/c/Users/a b/gen");
  assert.equal(toWslPath("D:/x/"), "/mnt/d/x");
  assert.throws(() => toWslPath("relative\\p"), /not an absolute Windows path/);
  const argv = generatorArgv({ node: "C:\\node\\node.exe", script: "C:\\r\\scripts\\termhost-bench\\generator\\ai-session.mjs", genDir: "C:\\Temp\\run\\gen", profile: "claude-tui", seed: 3, genRunId: "r1-u0" });
  const w = wslGeneratorArgv(argv, { node: "node", translate: toWslPath });
  assert.deepEqual(w, ["node", "/mnt/c/r/scripts/termhost-bench/generator/ai-session.mjs", "--run-dir", "/mnt/c/Temp/run/gen", ...argv.slice(4)]);
  assert.deepEqual(w.filter((a) => a.startsWith("--")), argv.filter((a) => a.startsWith("--")));
});

// ---------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------

test("the clock offset comes from the fastest round trip, bounded by half of it", () => {
  const est = estimateClockOffset([
    { t0: 1000n, t1: 9000n, remote: 50_000n },
    { t0: 20_000n, t1: 21_000n, remote: 70_600n },
    { t0: 30_000n, t1: 34_000n, remote: 80_000n },
  ]);
  assert.deepEqual(est, { offsetNs: 50_100n, rttNs: 1000n, n: 3 });
  assert.equal(estimateClockOffset([]), null);
  assert.equal(estimateClockOffset([{ t0: 5n, t1: 1n, remote: 3n }]), null, "a reversed pair is no sample");
});

test("measureClockOffset pings the distro's node over a pipe and keeps the best round trip", async () => {
  const spawned = [];
  const spawnFn = (cmd, args) => {
    spawned.push([cmd, args]);
    const child = new EventEmitter();
    child.stdout = new EventEmitter();
    child.stdin = {
      write: () => setImmediate(() => child.stdout.emit("data", Buffer.from(`${remote}\n`))),
      end: () => {},
    };
    return child;
  };
  let host = 0n;
  let remote = 0n;
  const now = () => {
    host += 100n;
    remote = host + 5_000n;
    return host;
  };
  const est = await measureClockOffset({ distro: "Ubuntu", spawnFn, now, rounds: 5 });
  assert.equal(est.n, 5);
  assert.ok(est.offsetNs > 4_000n && est.offsetNs < 5_100n, String(est.offsetNs));
  assert.deepEqual(spawned[0], ["wsl.exe", ["-d", "Ubuntu", "--exec", "node", "-e", CLOCK_ECHO_SCRIPT]]);
});

test("a clock round trip that never answers ends the wsl.exe child it started, and fails", async () => {
  const kills = [];
  const spawnFn = () => {
    const child = new EventEmitter();
    child.stdout = new EventEmitter();
    child.stdin = { write: () => {}, end: () => {} };
    child.kill = (sig) => kills.push(sig ?? "SIGTERM");
    return child;
  };
  await assert.rejects(measureClockOffset({ distro: "Ubuntu", spawnFn, timeoutMs: 30 }), /did not finish within/);
  assert.equal(kills.length, 1);
});

test("the generator reads its clock offset from the environment, never its command line, and refuses a malformed one", () => {
  assert.equal(GEN_OFFSET_ENV, CLOCK_OFFSET_ENV);
  assert.equal(clockOffsetFromEnv({}), 0n);
  assert.equal(clockOffsetFromEnv({ [CLOCK_OFFSET_ENV]: "-123456" }), -123456n);
  assert.throws(() => clockOffsetFromEnv({ [CLOCK_OFFSET_ENV]: "12ms" }), /integer nanosecond offset/);
  const c = createRealClock({ offsetNs: 10n ** 12n });
  const plain = process.hrtime.bigint();
  assert.ok(c.nowNs() - plain >= 10n ** 12n - 10n ** 9n, "the offset is applied");
  assert.equal(createRealClock().offsetNs, 0n);
});

// ---------------------------------------------------------------------------
// The adapter, over fakes
// ---------------------------------------------------------------------------

function rig(t, { clock = { offsetNs: -7_000_000n, rttNs: 400_000n, n: 15 }, clockEnd = null, scan = () => "", wslFacts = null } = {}) {
  let clockCalls = 0;
  const runDir = tmp(t, "tb-wsl-run-");
  const unitDir = join(runDir, "units", "u000-tmux-wsl-default-none-n3-r0");
  const genDir = join(unitDir, "gen");
  mkdirSync(genDir, { recursive: true });
  const bytes = Buffer.from("zip");
  const build = { ...wtBuildFor("win32", "x64"), url: "https://example.invalid/wt.zip", sha256: createHash("sha256").update(bytes).digest("hex"), bytes: 3 };
  const pin = { ...WT_PIN, builds: { "win32-x64": build } };
  const appDir = join(runDir, "wt-app", `${WT_PIN.version}-win32-x64`, build.topDir);
  const wt = createFakeWt({ appDir, genDir: null, operator: true });
  const wslCalls = [];
  const spawns = [];
  let windows = 0;
  const tmuxReply = (args) => {
    const sub = args.find((a) => /^[a-z]+-[a-z]+$|^kill-server$/.test(a));
    if (args.includes("new-session")) return "%0";
    if (sub === "new-window") return `%${++windows}`;
    if (sub === "display-message" && args.includes("#{pid}")) return "4242";
    if (sub === "list-clients") return wt.state.pid ? "5151" : "";
    if (sub === "list-panes") return "%0\t@1\t0\t100\t40\t1";
    return "";
  };
  const run = (cmd, args, opts) => {
    if (String(cmd).endsWith("wt.exe")) return wt.run(cmd, args, opts);
    if (cmd === "wsl.exe" || String(cmd).endsWith("wsl.exe")) {
      wslCalls.push(args);
      const inner = args.slice(3);
      if (inner[0] === "tmux") return { status: 0, stdout: `${tmuxReply(inner)}\n`, stderr: "" };
      if (inner[0] === "wslpath") return { status: 0, stdout: `${translate(inner[2])}\n` };
      if (inner[0] === "node" && inner[2] === WSL_FLAG_SCAN) {
        const out = scan(inner.slice(4));
        return out === null ? { status: 1, stdout: "", stderr: "scan failed" } : { status: 0, stdout: out };
      }
      return { status: 0, stdout: "" };
    }
    const script = cmd === "powershell.exe" && args.at(-2) === "-EncodedCommand" ? Buffer.from(args.at(-1), "base64").toString("utf16le") : null;
    if (script === VM_PROCESS_QUERY) return { status: 0, stdout: '{"p":9100,"n":"vmmemWSL","t":"3"}' };
    // the unpack — and nothing else: an unrecognised command is a test bug, never a write into the cwd
    const dest = opts?.env?.TERMHOST_UNPACK_DEST ?? (args.includes("-C") ? args[args.indexOf("-C") + 1] : null);
    if (!dest) throw new Error(`rig: unexpected command ${cmd} ${args.slice(0, 3).join(" ")}`);
    mkdirSync(join(dest, build.topDir), { recursive: true });
    for (const f of REQUIRED_FILES) writeFileSync(join(dest, build.topDir, f), f === "TerminalApp.dll" ? "--sessionId" : "x");
    return { status: 0, stdout: "" };
  };
  const spawnFn = (cmd, args, opts) => {
    const child = new EventEmitter();
    child.pid = 8000 + spawns.length;
    child.stdin = { end: () => setImmediate(() => child.emit("exit", 0)) };
    spawns.push({ cmd, args, opts, child });
    return child;
  };
  const adapter = new TmuxWslAdapter({
    platform: "win32",
    arch: "x64",
    env: WIN_ENV,
    pin,
    cacheDir: tmp(t),
    fetch: async () => ({ ok: true, status: 200, body: (async function* () { yield bytes; })() }),
    run,
    spawnFn,
    list: wt.list,
    kill: wt.kill,
    queryWt: wt.queryWt,
    queryWindows: wt.queryWindows,
    postClose: wt.postClose,
    exists: (p) => existsSync(p),
    sleep: (ms) => new Promise((r) => setTimeout(r, Math.min(ms, 5))),
    wslFacts: () => wslFacts ?? { list: [{ name: "Ubuntu", isDefault: true, state: "Running", version: 2 }], tmux: "tmux 3.4", node: "v22.11.0", clkTck: "100", bash: true },
    clockOffset: async () => {
      clockCalls += 1;
      const v = clockCalls > 1 && clockEnd ? clockEnd : clock;
      if (v instanceof Error) throw v;
      return v;
    },
    translate,
    launchTimeoutMs: 2000,
    closeTimeoutMs: 50,
    generatorWaitMs: 10,
  });
  const argv = generatorArgv({ node: "C:\\node\\node.exe", script: "C:\\r\\ai-session.mjs", genDir: "C:\\Temp\\gen", profile: "claude-tui", seed: 1, genRunId: "r1-u0" });
  return { runDir, unitDir, genDir, wt, adapter, wslCalls, spawns, argv, ctx: { runDir, unitDir, genDir, genRunId: "r1-u0" }, clockCalls: () => clockCalls };
}

/** The tests' paths are POSIX already; a Windows one is translated as the default automount would. */
const translate = (p) => (isWindowsPath(p) ? toWslPath(p) : p);

test("the doctor reports the measured WSL clock round trip beside the 2 ms threshold, and the launch reuses that reading", async (t) => {
  const r = rig(t, { clock: { offsetNs: -7_000_000n, rttNs: 1_250_000n, n: 60 } });
  const m = (await r.adapter.check()).mechanisms;
  assert.equal(m["setup.wsl_clock"].state, "OPERATIVE");
  assert.equal(r.clockCalls(), 1);
  // launch runs check() again and uses ITS reading: exactly one more measurement, none of its own.
  const l = rig(t, { clock: { offsetNs: -7_000_000n, rttNs: 1_250_000n, n: 60 } });
  await l.adapter.launch(l.ctx);
  assert.equal(l.clockCalls(), 1, "one round-trip measurement serves the doctor's verdict and the launch");
  assert.match(m["setup.wsl_clock"].reason, /fastest of 60 round trips through Ubuntu: 1\.25 ms \(offset bound ±0\.625 ms\); threshold 2 ms/);
  const slow = rig(t, { clock: { offsetNs: 0n, rttNs: 3_000_000n, n: 60 } });
  const ms = (await slow.adapter.check()).mechanisms["setup.wsl_clock"];
  assert.equal(ms.state, "INOPERATIVE-ON-THIS-MACHINE");
  assert.match(ms.reason, /3 ms .*refuses as clock_unaligned/);
  assert.equal(wslClockVerdict(new Error("x"), "U").state, "UNKNOWN");
  const noNode = rig(t, { wslFacts: { list: [{ name: "Ubuntu", isDefault: true, state: "Running", version: 2 }], tmux: "tmux 3.4", node: null } });
  assert.equal((await noNode.adapter.check()).mechanisms["setup.wsl_clock"].state, "UNKNOWN");
});

test("tmux-wsl on Linux is the tmux setup: its doctor says so", async () => {
  const a = new TmuxWslAdapter({ platform: "linux" });
  assert.equal((await a.check()).mechanisms["setup.wsl"].state, INOP);
});

test("launch: a clock that cannot be bounded refuses the unit as clock_unaligned; the offset reaches the panes through the private server's environment", async (t) => {
  const slow = rig(t, { clock: { offsetNs: 1n, rttNs: MAX_CLOCK_RTT_NS + 1n, n: 3 } });
  await assert.rejects(slow.adapter.launch(slow.ctx), (e) => e instanceof AdapterRefusal && e.reason === "clock_unaligned");
  const broken = rig(t, { clock: new Error("no node") });
  await assert.rejects(broken.adapter.launch(broken.ctx), (e) => e instanceof AdapterRefusal && e.reason === "clock_unaligned" && /no node/.test(e.detail));
  const noTmux = rig(t, { wslFacts: { list: [{ name: "Ubuntu", isDefault: true, state: "Running", version: 2 }], tmux: null, node: "v22.0.0" } });
  await assert.rejects(noTmux.adapter.launch(noTmux.ctx), (e) => e instanceof AdapterRefusal && /setup\.wsl_tmux/.test(e.detail));
  const r = rig(t);
  await r.adapter.launch(r.ctx);
  const conf = readFileSync(join(r.unitDir, "tmux.conf"), "utf8");
  assert.match(conf, new RegExp(`^set-environment -g ${CLOCK_OFFSET_ENV} 7000000$`, "m"), "WSL time plus 7 ms is the host's");
});

test("end to end: tmux in the distro through wsl.exe --exec, the portable Windows Terminal attached, samplers started, teardown by socket and WM_CLOSE", async (t) => {
  const r = rig(t, { scan: ([flag]) => (flag === "-L" ? "777\n" : "") });
  await r.adapter.launch(r.ctx);
  const s = await r.adapter.spawnSessions(3, r.argv);
  assert.equal(s.sessions.length, 3);
  const created = r.wslCalls.find((a) => a.includes("new-session"));
  assert.deepEqual(created.slice(0, 7), ["-d", "Ubuntu", "--exec", "tmux", "-L", "termhost-r1-u0", "-f"]);
  assert.equal(created[7], translate(join(r.unitDir, "tmux.conf")), "the config by its WSL path");
  const shellArgv = created.slice(created.indexOf("--") + 1);
  assert.deepEqual(shellArgv.slice(0, 3), ["/bin/bash", "-i", "-c"]);
  assert.match(shellArgv[3], /^node \/mnt\/c\/r\/ai-session\.mjs --run-dir \/mnt\/c\/Temp\/gen --profile claude-tui --seed 1 --echo --termhost-bench-generator r1-u0; exec \/bin\/bash -i$/);
  // The window: one tab running the tmux client.
  const tab = r.wt.sessions[0].cmd;
  assert.deepEqual(tab, attachCommand({ wslExe: r.adapter.wslExe, distro: "Ubuntu", socket: "termhost-r1-u0" }));
  assert.deepEqual(tab.slice(1), ["-d", "Ubuntu", "--exec", "tmux", "-L", "termhost-r1-u0", "attach-session", "-t", "bench"], "no shell in the distro between wsl.exe and tmux");
  assert.match(tab[0], /System32[\\/]wsl\.exe$/);
  assert.deepEqual(r.adapter.processRoots(), [r.wt.state.pid]);
  assert.deepEqual(r.adapter.windowTarget(), { hwnd: r.wt.state.hwnd });
  // The samplers.
  const wslSampler = r.spawns.find((x) => x.args.includes("--server-pid"));
  assert.ok(wslSampler, "the /proc sampler runs inside the distro");
  assert.deepEqual(wslSampler.args.slice(0, 4), ["-d", "Ubuntu", "--exec", "node"]);
  const sa = wslSampler.args.slice(5);
  assert.deepEqual(sa.slice(0, 2), ["--server-pid", "4242"]);
  assert.equal(sa[sa.indexOf("--clock-offset-ns") + 1], "7000000");
  const vm = r.spawns.find((x) => x.cmd === "powershell.exe");
  assert.equal(vm.opts.env.TB_VM_PID, "9100");
  assert.equal(vm.args.at(-2), "-EncodedCommand");
  assert.equal(Buffer.from(vm.args.at(-1), "base64").toString("utf16le"), VM_SAMPLER_SCRIPT, "the multi-line script travels encoded");
  const d = r.adapter.describe();
  assert.equal(d.id, "tmux-wsl");
  assert.equal(d.launch_mode, "private_server");
  assert.equal(d.session_shell, "bash -i");
  assert.match(d.app_version, /tmux 3\.4 in WSL Ubuntu \(node v22\.11\.0\) in Windows Terminal .*clock offset 7000000 ns/);
  assert.match(d.settings_digest, /^sha256:/);
  // Teardown.
  const td = await r.adapter.teardown();
  const kill = r.wslCalls.find((a) => a.includes("kill-server"));
  assert.deepEqual(kill, ["-d", "Ubuntu", "--exec", "tmux", "-L", "termhost-r1-u0", "kill-server"]);
  assert.ok(existsSync(join(r.unitDir, "wsl", "stop")), "the samplers' stop file");
  assert.deepEqual(r.wt.posted, [[r.wt.state.pid ?? r.wt.posted[0][0], r.wt.state.hwnd]]);
  assert.ok(td.survivors.some((x) => x.pid === 777 && /tmux -L termhost-r1-u0/.test(x.what)), "a WSL-side survivor is reported");
  assert.ok(!td.survivors.some((x) => /sampler/.test(x.what)), "both samplers stopped on their stop file / stdin EOF");
  assert.ok(!r.wt.signalled.some(([pid]) => pid === 777), "and never signalled: no PID door reaches into the distro");
  assert.ok(!r.wt.signalled.some(([pid]) => pid === r.wt.operatorPid));
});

test("the scan for WSL survivors reads /proc and passes its flag after `--`, so node does not take `-L` as its own", () => {
  assert.match(WSL_FLAG_SCAN, /readdirSync\('\/proc'\)/);
  assert.doesNotMatch(WSL_FLAG_SCAN, /kill/);
  const argv = samplerArgv({ script: "/s.mjs", serverPid: 5, socket: "s", runId: "r", out: "/o", stopFile: "/x", clkTck: 100, clockOffsetNs: -3n });
  const parsed = samplerParseArgs(argv.slice(2));
  assert.deepEqual([parsed.serverPid, parsed.socket, parsed.runId, parsed.clockOffsetNs, parsed.clkTck], [5, "s", "r", -3n, 100]);
  assert.throws(() => samplerParseArgs(["--socket", "s"]), /is required/);
});

test("the WSL survivor scan, EXECUTED with real node over this box's /proc: it finds a process carrying the pair, and never itself", { skip: process.platform !== "linux" && "reads /proc: Linux only" }, async (t) => {
  const id = `scan-test-${process.pid}-${Date.now()}`;
  const scan = (flag, value) => {
    const r = spawnSync(process.execPath, ["-e", WSL_FLAG_SCAN, "--", flag, value], { encoding: "utf8" });
    assert.equal(r.status, 0, r.stderr);
    return r.stdout.split("\n").filter(Boolean).map(Number);
  };
  assert.deepEqual(scan("--termhost-bench-generator", `${id}-nothing`), [], "a run id nothing carries finds nothing — the scan's own argv is not a hit");
  assert.deepEqual(scan("-L", `termhost-${id}-nothing`), []);
  const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)", "--", "--termhost-bench-generator", id, "-L", `termhost-${id}`], { stdio: "ignore" });
  t.after(() => child.kill());
  await new Promise((r) => setTimeout(r, 300));
  assert.deepEqual(scan("--termhost-bench-generator", id), [child.pid]);
  assert.deepEqual(scan("-L", `termhost-${id}`), [child.pid]);
  // A shell wrapper around the scan carries the same pair: skipped as the scan's parent.
  const wrapped = spawnSync("/bin/sh", ["-c", '"$0" -e "$1" -- "$2" "$3"; :', process.execPath, WSL_FLAG_SCAN, "--termhost-bench-generator", `${id}-nothing`], { encoding: "utf8" });
  assert.equal(wrapped.stdout.trim(), "", "the wrapping shell is not reported");
});

// ---------------------------------------------------------------------------
// The WSL-side sampler
// ---------------------------------------------------------------------------

test("the sampler parses CPU ticks after the last ')' and PSS from smaps_rollup", () => {
  assert.equal(cpuTicks("42 (tmux: server) S 1 42 42 0 -1 4194560 100 0 0 0 7 3 0 0 20 0 1 0 900 1 1"), 10);
  assert.equal(cpuTicks("42 (a) b) S 1 42 42 0 -1 0 0 0 0 0 5 6 0 0 20 0 1 0 1 1 1"), 11, "a ')' in the name");
  assert.equal(cpuTicks("garbage"), null);
  assert.equal(pssKb("Rss: 10 kB\nPss:                1234 kB\n"), 1234);
  assert.equal(pssKb(""), null);
});

test("classify: the server, its non-workload descendants and its clients are host; generators and their shells are workload; another socket is neither", () => {
  const p = (pid, ppid, argv, startKey = "100") => [pid, { pid, ppid, argv, startKey, name: argv[0], cmdline: argv.join(" ") }];
  const table = new Map([
    p(10, 1, ["tmux", "-L", "termhost-r1", "-f", "/c", "new-session"], "50"),
    p(11, 10, ["/bin/bash", "-i", "-c", "node g"]),
    p(12, 11, ["node", "/g.mjs", "--termhost-bench-generator", "r1-u0"]),
    p(13, 10, ["/bin/bash", "-i"]),
    p(20, 5, ["tmux", "-L", "termhost-r1", "attach-session", "-t", "bench"]),
    p(30, 1, ["tmux", "-L", "operator", "attach"]),
    p(31, 10, ["stale"], "10"),
  ]);
  const c = classify(table, { serverPid: 10, serverStartKey: "50", socket: "termhost-r1", runId: "r1-u0" });
  assert.deepEqual(c.host, [10, 13, 20], "a 'child' started before the server is a recycled link, not ours");
  assert.deepEqual(c.workload, [11, 12]);
  assert.equal(c.serverAlive, true);
  assert.equal(classify(table, { serverPid: 10, serverStartKey: "49", socket: "termhost-r1", runId: "r1-u0" }).serverAlive, false, "a recycled server PID is not the server");
});

test("a group's reading: tick deltas per interval; an unreadable member makes the reading null, never a smaller number", () => {
  const prev = new Map([[1, 100], [2, 50]]);
  const now = new Map([[1, 110], [2, 55], [3, 4]]);
  const pss = new Map([[1, 1024], [2, 2048], [3, 1024]]);
  const g = groupReading([1, 2, 3], prev, now, pss, { dtMs: 500, clkTck: 100 });
  assert.deepEqual([g.cpu_ms, g.cpu_pct, g.pss_mb, g.unreadable], [150, 30, 4, 0], "pid 3, first seen, contributes nothing");
  const bad = groupReading([1, 9], prev, now, pss, { dtMs: 500, clkTck: 100 });
  assert.deepEqual([bad.cpu_ms, bad.cpu_pct, bad.pss_mb, bad.unreadable], [null, null, null, 1]);
  assert.equal(groupReading([1], prev, now, pss, { dtMs: null, clkTck: 100 }).cpu_pct, null, "the first sample has no interval");
});

test("a process that appears mid-window adds only the ticks it spends after its first sample, not its lifetime", () => {
  // Sample 1: generator 200 (1000 ticks). Sample 2: 200 at 1010, and 300 appears already holding 90000 ticks.
  const key = (pid) => `${pid}:${pid === 300 ? "77" : "11"}`;
  const s1 = new Map([["200:11", 1000]]);
  const s2 = new Map([["200:11", 1010], ["300:77", 90000]]);
  const s3 = new Map([["200:11", 1020], ["300:77", 90030]]);
  const pss = new Map([[200, 1024], [300, 1024]]);
  const first = groupReading([200, 300], s1, s2, pss, { dtMs: 500, clkTck: 100, keyOf: key });
  assert.deepEqual([first.cpu_ms, first.cpu_pct], [100, 20], "300's 90000 lifetime ticks are not this interval's (was 180020 %)");
  const next = groupReading([200, 300], s2, s3, pss, { dtMs: 500, clkTck: 100, keyOf: key });
  assert.deepEqual([next.cpu_ms, next.cpu_pct], [400, 80], "from its second sample on, its delta counts");
});

test("a reused PID is a new process: its smaller tick count neither subtracts from the group nor carries the old process's ticks", () => {
  const pss = new Map([[200, 1024], [201, 1024]]);
  // 200 (start 11) had 5000 ticks; it exits and a new 200 (start 99) holds 3 ticks. 201 runs 10 ticks.
  const before = new Map([["200:11", 5000], ["201:12", 100]]);
  const after = new Map([["200:99", 3], ["201:12", 110]]);
  const keys = { 200: "99", 201: "12" };
  const g = groupReading([200, 201], before, after, pss, { dtMs: 500, clkTck: 100, keyOf: (pid) => `${pid}:${keys[pid]}` });
  assert.deepEqual([g.cpu_ms, g.cpu_pct], [100, 20], "only 201's 10 ticks: the new 200 is first-seen, and nothing goes negative");
  // Keyed by pid alone (the old behaviour) the reuse would subtract 4997 ticks from the group.
  const byPid = groupReading([200, 201], new Map([[200, 5000], [201, 100]]), new Map([[200, 3], [201, 110]]), pss, { dtMs: 500, clkTck: 100 });
  assert.equal(byPid.cpu_ms, 100, "and even keyed by pid, each delta is clamped on its own");
});

// ---------------------------------------------------------------------------
// The KPIs
// ---------------------------------------------------------------------------

const T0 = 1_000_000_000_000n;
const win = { t0: T0, t1: T0 + 20_000n * MS };
const wslSeries = (pct, { from = 0, to = 41, shift = 100n } = {}) =>
  Array.from({ length: to - from }, (_, i) => ({ type: "sample", t_ns: String(T0 + BigInt(from + i) * 500n * MS + shift * MS), dt_ms: 500, host: { cpu_pct: pct, pss_mb: 12 } }));

test("the host's cpu_pct joins each probe sample with the nearest WSL-side one; too many unpaired samples is unknown", () => {
  const probeCpu = Array.from({ length: 40 }, (_, i) => ({ t: T0 + BigInt(i + 1) * 500n * MS, v: 20 }));
  const ok = wslHostKpis({ probeCpu, probeMemEnd: 300, wslRecords: wslSeries(5), win });
  assert.equal(ok.cpu_pct.state, "measured");
  assert.equal(ok.cpu_pct.stats.mean, 25, "Windows-side 20 % plus the tmux server's 5 %");
  assert.equal(ok.memory_mb.value, 312);
  const gap = wslHostKpis({ probeCpu, probeMemEnd: 300, wslRecords: wslSeries(5, { from: 0, to: 20 }), win });
  assert.equal(gap.cpu_pct.state, "unknown", "half the window has no WSL-side sample");
  const none = wslHostKpis({ probeCpu, probeMemEnd: 300, wslRecords: [], win });
  assert.match(none.cpu_pct.detail, /wrote nothing/);
  assert.equal(wslHostKpis({ probeCpu, probeMemEnd: null, wslRecords: wslSeries(5), win }).memory_mb.state, "unknown");
});

test("the shared WSL VM: CPU from its cumulative time, memory from its last private bytes; a refused read is unknown, never 0", () => {
  const rec = (i, cpu) => ({ type: "vm", t_ns: String(T0 + BigInt(i) * 500n * MS), cpu_ms: cpu, private_mb: 2048 + i });
  const vm = Array.from({ length: 30 }, (_, i) => rec(i, 1000 + i * 50));
  const k = vmKpis(vm, win);
  assert.equal(k.wsl_vm_cpu_pct.state, "measured");
  assert.equal(k.wsl_vm_cpu_pct.stats.mean, 10);
  assert.equal(k.wsl_vm_memory_mb.value, 2048 + 29);
  const denied = vmKpis([{ type: "error", t_ns: String(T0 + 500n * MS), detail: "Access is denied" }], win);
  assert.equal(denied.wsl_vm_cpu_pct.state, "unknown");
  assert.match(denied.wsl_vm_cpu_pct.detail, /Access is denied/);
  assert.equal(vmKpis(vm, null).wsl_vm_memory_mb.reason, "harness_error");
  assert.match(VM_PROCESS_QUERY, /vmmemWSL/);
  assert.doesNotMatch(VM_SAMPLER_SCRIPT, /Stop-Process|Kill\(/);
  assert.deepEqual(pickVmProcess('[{"p":1,"n":"vmmem","t":"1"},{"p":2,"n":"vmmemWSL","t":"2"}]'), { pid: 2, name: "vmmemWSL", startKey: "2" });
  assert.equal(pickVmProcess('[{"p":1,"n":"vmmem","t":"1"},{"p":3,"n":"vmmem","t":"1"}]'), null, "two Hyper-V VMs: which is WSL's is unknown");
});

test("a tmux-wsl row: the samples reach cpu_pct and wsl_vm_*; without them wsl_vm_* is not_instrumented, and other setups never carry it", () => {
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: 40_000 });
  const records = watchSegment({ t0: T0, durationMs: 20_000, sessions: [{ session: 0, gen, latencyMs: 20 }], primary: "wgc" });
  const base = { environment: "windows-desktop", n: 1, arm: "focus", vTarget: 1, capabilities: operativeCapabilities(), gens: indexGenerators(new Map([[0, gen]])), soak: { records } };
  const vm = Array.from({ length: 41 }, (_, i) => ({ type: "vm", t_ns: String(T0 + BigInt(i) * 500n * MS), cpu_ms: i * 50, private_mb: 4096 }));
  const withWsl = computeRowKpis({ ...base, setupId: "tmux-wsl", wsl: { wslRecords: wslSeries(5), vmRecords: vm } });
  const without = computeRowKpis({ ...base, setupId: "tmux-wsl" });
  assert.equal(withWsl.kpis.wsl_vm_cpu_pct.state, "measured");
  assert.equal(withWsl.kpis.wsl_vm_memory_mb.value, 4096);
  assert.equal(without.kpis.wsl_vm_cpu_pct.reason, "not_instrumented");
  assert.equal(without.kpis.cpu_pct.stats.mean, 26, "the probe's Windows-side host alone");
  assert.equal(withWsl.kpis.cpu_pct.stats.mean, 31, "plus the WSL-side tmux server's 5 %");
  assert.equal(withWsl.kpis.cpu_pct.n, without.kpis.cpu_pct.n);
  const tmux = computeRowKpis({ ...base, setupId: "tmux", wsl: { wslRecords: wslSeries(5), vmRecords: vm } });
  assert.equal(tmux.kpis.wsl_vm_cpu_pct.state, "not_applicable");
});

test("9P: a record gap the generator spent in its own file I/O is not producer stall (review repro gap.mjs)", () => {
  // One session, progress every 500 ms, nothing inside the pane write; one record
  // lands 1500 ms late because a log write blocked.
  const points = (ioInGap) => {
    const pts = [];
    let t = 0n;
    let io = 0;
    for (let i = 0; i < 20; i += 1) {
      pts.push({ t, blocked: 0, io, written: i * 100, scheduled: i * 100 });
      if (i === 9) io += ioInGap;
      t += i === 9 ? 2000n * MS : 500n * MS;
    }
    return pts;
  };
  const run = (pts) => generatorWindow(new Map([[0, { points: pts }]]), { t0: 0n, t1: pts.at(-1).t });
  const legacy = run(points(0).map((p) => ({ ...p, io: null })));
  assert.ok(Math.abs(legacy.stall[0] - 18.1818) < 0.001, "a generator that records no io_ns: the whole 2 s gap is stall, as before");
  const all = run(points(2000 * 1e6));
  assert.equal(all.stall[0], 0, "the whole gap was the generator's own log write");
  assert.match(all.notes.join(" "), /own file I\/O inside record gaps was not counted as stall \(session 0: 2000 ms\)/);
  const part = run(points(1500 * 1e6));
  assert.ok(Math.abs(part.stall[0] - (500 / 11000) * 100) < 0.001, `only the 500 ms the I/O does not explain (${part.stall[0]})`);
});

test("9P end to end: a generator whose every log write takes 1.2 s records it as io_ns, and its stall stays 0, not 100 %", async () => {
  const { profile } = loadProfile("claude-tui");
  const r = await runVirtual({ profile, durationMs: 20000, logDelayNs: 1_200_000_000 });
  assert.ok(r.exit.io_ns > 10e9, `io_ns ${r.exit.io_ns}`);
  assert.equal(r.exit.blocked_ns, 0, "the pane write never blocked");
  assert.ok(r.records.filter((x) => x.type === "progress").every((x) => Number.isFinite(x.io_ns)));
  const g = indexGenerators(new Map([[0, r.records]])).get(0);
  const win = { t0: g.points[1].t, t1: g.points.at(-2).t };
  assert.deepEqual(generatorWindow(new Map([[0, g]]), win).stall, [0]);
  const blind = { ...g, points: g.points.map((p) => ({ ...p, io: null })) };
  assert.deepEqual(generatorWindow(new Map([[0, blind]]), win).stall, [100], "what the same records read before io_ns existed");
  const fast = await runVirtual({ profile, durationMs: 2000 });
  assert.equal(fast.exit.io_ns, 0, "on a virtual clock an instant log costs nothing");
});

// ---------------------------------------------------------------------------
// windows-desktop
// ---------------------------------------------------------------------------

test("windows-desktop: never a GitHub-hosted runner, and a run needs session.interactive and capture.wgc OPERATIVE", async () => {
  assert.equal(desktopVerdict({ GITHUB_ACTIONS: "true", RUNNER_ENVIRONMENT: "github-hosted" }).state, INOP);
  assert.equal(desktopVerdict({}).state, "OPERATIVE");
  const facts = { ...healthyFacts(), environment: "windows-desktop", envPlatform: "win32", platform: "win32", envNotBuilt: null };
  const probe = (interactive) => ({ record: { mechanisms: [{ id: "session.interactive", ...interactive }, { id: "capture.wgc", state: "OPERATIVE", reason: "ok" }], violations: [] }, error: null });
  const ready = mapDoctor({ ...facts, probeDoctor: probe({ state: "OPERATIVE", reason: "unlocked" }) });
  assert.equal(ready["env.desktop_ready"].state, "OPERATIVE");
  assert.ok(!blockingMechanisms(ready).some((b) => b.id === "env.desktop_ready"));
  const locked = mapDoctor({ ...facts, probeDoctor: probe({ state: INOP, reason: "the input desktop is locked" }) });
  assert.equal(locked["env.desktop_ready"].state, INOP);
  assert.match(locked["env.desktop_ready"].reason, /session\.interactive OPERATIVE \(it is INOPERATIVE-ON-THIS-MACHINE: the input desktop is locked\)/);
  assert.ok(blockingMechanisms(locked).some((b) => b.id === "env.desktop_ready"), "the run does not start");
  assert.equal(mapDoctor({ ...facts, environment: "windows-hosted" })["env.desktop_ready"], undefined, "a hosted run is not gated on it");
  await assert.rejects(startWindowsDesktop({ platform: "linux" }), /Windows only/);
  const env = await startWindowsDesktop({ platform: "win32", run: () => ({ status: 0, stdout: '{"w":2560,"h":1440,"dpi":144}' }) });
  assert.deepEqual(env.screen, { width: 2560, height: 1440, scalePct: 150 });
});

test("the clock: 2 ms is the bound (±1 ms of bias at most); the fastest of many round trips is kept; teardown re-measures and a drift past the bound is an incident", async (t) => {
  assert.equal(MAX_CLOCK_RTT_NS, 2_000_000n);
  assert.ok(CLOCK_ROUNDS >= 50);
  const ok = rig(t, { clock: { offsetNs: -7_000_000n, rttNs: 1_900_000n, n: 60 } });
  await ok.adapter.launch(ok.ctx);
  const edge = rig(t, { clock: { offsetNs: 0n, rttNs: 2_000_001n, n: 60 } });
  await assert.rejects(edge.adapter.launch(edge.ctx), (e) => e.reason === "clock_unaligned" && /fastest of 60 clock round trips/.test(e.detail) && /2\.000001 ms/.test(e.detail));
  // Within the bound: no incident.
  const steady = rig(t, { clock: { offsetNs: -7_000_000n, rttNs: 400_000n, n: 60 }, clockEnd: { offsetNs: -7_100_000n, rttNs: 400_000n, n: 60 } });
  await steady.adapter.launch(steady.ctx);
  await steady.adapter.spawnSessions(1, steady.argv);
  const a = await steady.adapter.teardown();
  assert.deepEqual(a.incidents, []);
  assert.equal(steady.adapter.wslSamples().clockEnd.state, "measured");
  assert.equal(steady.adapter.wslSamples().clockEnd.driftNs, "100000");
  assert.equal(steady.adapter.wslSamples().clockEnd.boundNs, "400000");
  // Past it: an incident naming the drift.
  const drifting = rig(t, { clock: { offsetNs: -7_000_000n, rttNs: 400_000n, n: 60 }, clockEnd: { offsetNs: -9_000_000n, rttNs: 400_000n, n: 60 } });
  await drifting.adapter.launch(drifting.ctx);
  await drifting.adapter.spawnSessions(1, drifting.argv);
  const b = await drifting.adapter.teardown();
  assert.equal(b.incidents.length, 1);
  assert.equal(b.incidents[0].kind, "harness_error");
  assert.match(b.incidents[0].detail, /drifted 2 ms during the unit \(bound 0\.4 ms\)/);
  // A teardown round trip past the bound would hide a real drift: UNKNOWN, no incident.
  const slow = rig(t, { clock: { offsetNs: -7_000_000n, rttNs: 400_000n, n: 60 }, clockEnd: { offsetNs: -30_000_000n, rttNs: 5_000_000n, n: 60 } });
  await slow.adapter.launch(slow.ctx);
  await slow.adapter.spawnSessions(1, slow.argv);
  const c = await slow.adapter.teardown();
  assert.deepEqual(c.incidents, []);
  const endSlow = slow.adapter.wslSamples().clockEnd;
  assert.equal(endSlow.state, "unknown");
  assert.match(endSlow.reason, /fastest round trip took 5 ms, more than 2 ms/);
  assert.equal(endSlow.driftNs, undefined, "no drift figure from an unbounded re-measure");
  // A failed re-measure is UNKNOWN too.
  const failing = rig(t, { clock: { offsetNs: -7_000_000n, rttNs: 400_000n, n: 60 }, clockEnd: new Error("node gone") });
  await failing.adapter.launch(failing.ctx);
  await failing.adapter.spawnSessions(1, failing.argv);
  await failing.adapter.teardown();
  assert.deepEqual(failing.adapter.wslSamples().clockEnd, { state: "unknown", reason: "the teardown re-measure failed: node gone" });
});

test("teardown re-measures the clock only after the generators have left", async (t) => {
  let generatorsAlive = 2;
  const order = [];
  const r = rig(t, {
    clockEnd: { offsetNs: -7_000_000n, rttNs: 400_000n, n: 60 },
    scan: ([flag]) => {
      if (flag !== "--termhost-bench-generator") return "";
      order.push(`scan:${generatorsAlive}`);
      return generatorsAlive-- > 0 ? "4300\n" : "";
    },
  });
  await r.adapter.launch(r.ctx);
  await r.adapter.spawnSessions(1, r.argv);
  const inner = r.adapter.clockOffsetFn;
  r.adapter.clockOffsetFn = async (d) => (order.push("clock"), inner(d));
  r.adapter.generatorWaitMs = 5000;
  await r.adapter.teardown();
  const clock = order.indexOf("clock");
  assert.ok(order.includes("scan:0"), `the teardown saw the generators gone: ${order.join(" ")}`);
  assert.ok(clock > order.indexOf("scan:0"), `the re-measure came after the scan that found no generator: ${order.join(" ")}`);
  assert.equal(r.adapter.wslSamples().clockEnd.under_load, undefined, "taken without the generators' load");
});

test("a tmux-wsl row: workload from inside the distro, the clock bound on both latencies, the 9P note on the producer KPIs", () => {
  const measured = (v) => ({ state: "measured", unit: "ms", n: 100, stats: { p50: v, p95: v, p99: v } });
  const kpis = { output_latency_ms: measured(20), input_latency_ms: measured(30), producer_stall_pct: { state: "measured", unit: "%", n: 3, stats: { mean: 1, max: 2 } }, delivered_ratio: { state: "unknown", unit: "ratio", reason: "harness_error" } };
  const records = Array.from({ length: 41 }, (_, i) => ({ type: "sample", t_ns: String(T0 + BigInt(i) * 500n * MS), dt_ms: i === 0 ? null : 500, host: { cpu_pct: 5, pss_mb: 12 }, workload: { cpu_pct: i === 0 ? null : 40, pss_mb: 300, unreadable: 0 } }));
  const adj = wslRowAdjustments(kpis, { wslRecords: records, clockRttNs: "400000", clockEnd: { state: "measured", driftNs: "100000", boundNs: "400000" } }, win);
  assert.equal(adj.workload_cpu_pct.state, "measured");
  assert.equal(adj.workload_cpu_pct.stats.mean, 40);
  assert.equal(adj.workload_memory_mb.value, 300);
  assert.match(adj.output_latency_ms.notes.at(-1), /error bound is ±0\.2 ms .*by teardown it drifted 0\.1 ms \(bound ±0\.4 ms\)/);
  const unknownEnd = wslRowAdjustments(kpis, { wslRecords: records, clockRttNs: "400000", clockEnd: { state: "unknown", reason: "the teardown re-measure failed: x" } }, win);
  assert.match(unknownEnd.output_latency_ms.notes.at(-1), /its drift by teardown is UNKNOWN \(the teardown re-measure failed: x\)/);
  assert.match(wslRowAdjustments(kpis, { wslRecords: records, clockRttNs: "400000", clockEnd: null }, win).input_latency_ms.notes[0], /was not re-measured/);
  assert.ok(adj.input_latency_ms.notes.some((n) => /±0\.2 ms/.test(n)), "the clock bound");
  assert.ok(adj.input_latency_ms.notes.some((n) => /9P/.test(n) && /stdin service/.test(n)), "and the 9P delay to stdin service");
  assert.equal(kpis.output_latency_ms.notes, undefined, "the input values are not mutated");
  assert.match(adj.producer_stall_pct.notes.at(-1), /9P/);
  assert.match(adj.delivered_ratio.notes.at(-1), /9P/);
  const none = wslRowAdjustments(kpis, null, win);
  assert.equal(none.workload_cpu_pct.state, "unknown", "the Windows probe cannot see WSL processes: never its empty group");
  assert.match(none.output_latency_ms.notes.at(-1), /offset into the probe's timebase is unknown/);
  const gap = records.map((r, i) => (i === 7 ? { ...r, workload: { cpu_pct: null, pss_mb: null, unreadable: 1 } } : r));
  assert.equal(wslRowAdjustments(kpis, { wslRecords: gap, clockRttNs: "1" }, win).workload_cpu_pct.state, "unknown");
});

test("the run loop records an adapter's teardown incidents with its setup and unit, and a tmux-wsl document with WSL samples stays schema-valid", async (t) => {
  const tmp = mkdtempSync(join(tmpdir(), "tb-wsl-loop-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  mkdirSync(join(tmp, "primary"), { recursive: true });
  const settings = join(tmp, "primary", "settings.json");
  writeFileSync(settings, "{}");
  const clock = new VirtualClock();
  let current = null;
  class WslFake extends FakeAdapter {
    describe() {
      return { ...super.describe(), id: "tmux-wsl" };
    }
    wslSamples() {
      return { wslRecords: [], vmRecords: [], clockRttNs: "400000", clockEnd: null };
    }
    async teardown() {
      const td = await super.teardown();
      return { ...td, incidents: [incident("harness_error", "the WSL clock offset drifted 3 ms during the unit (bound 0.4 ms)")] };
    }
  }
  const res = await runBench(fakeRunOptions(join(tmp, "work"), { setups: ["tmux-wsl"], ns: [1], smokeSetups: ["tmux-wsl"], out: join(tmp, "results.json") }), {
    log: () => {},
    sleep: clock.sleep,
    startEnvironment: async () => ({ env: { DISPLAY: ":99" }, screen: { width: 1920, height: 1080, scalePct: 100 }, display: ":99", stop: async () => {} }),
    probePath: "fake-probe",
    createAdapter: async (setup, o) => (current = new WslFake({ id: setup, clock, variant: o.variant })),
    probe: fakeProbe({ clock, current: () => current }),
    gatherDoctorFacts: async () => healthyFacts(),
    sampleBackgroundLoad: async () => ({ cpu_pct: 1, memory_used_mb: 100, sampled_ms: 1 }),
    machineBlock: (o) => machineBlock({ ...o, gpu: { state: "unknown", reason: "not_instrumented", detail: "test" } }),
    neutrality: new NeutralityGuard({ paths: [{ label: "settings.json", path: settings }] }),
    reapGenerators: async () => ({ ended: [], survivors: [] }),
  });
  const drift = res.doc.incidents.filter((i) => /drifted 3 ms/.test(i.detail));
  assert.equal(drift.length, 1);
  assert.equal(drift[0].data.setup, "tmux-wsl");
  const v = validateResults(res.doc);
  assert.equal(v.valid, true, JSON.stringify(v.errors.slice(0, 3)));
  const row = res.doc.rows.find((r) => r.setup.id === "tmux-wsl");
  assert.equal(row.kpis.workload_cpu_pct.state, "unknown", "no WSL-side workload samples: unknown, never the probe's empty Windows group");
});

test("a teardown re-measure taken while generators may still run says so, in clockEnd and in the latency note", async (t) => {
  const stuck = rig(t, { clockEnd: { offsetNs: -7_000_000n, rttNs: 400_000n, n: 60 }, scan: ([flag]) => (flag === "--termhost-bench-generator" ? "4300\n" : "") });
  await stuck.adapter.launch(stuck.ctx);
  await stuck.adapter.spawnSessions(1, stuck.argv);
  stuck.adapter.generatorWaitMs = 1;
  await stuck.adapter.teardown();
  const end = stuck.adapter.wslSamples().clockEnd;
  assert.equal(end.state, "measured");
  assert.equal(end.under_load, true);
  assert.match(end.load, /1 generator\(s\) were still running/);
  const note = wslRowAdjustments({ output_latency_ms: { state: "measured", unit: "ms", n: 1, stats: {} } }, { wslRecords: [], clockRttNs: "400000", clockEnd: end }, win).output_latency_ms.notes.join(" ");
  assert.match(note, /re-measured under possible load: 1 generator\(s\) were still running/);
  const blind = rig(t, { clockEnd: { offsetNs: -7_000_000n, rttNs: 400_000n, n: 60 }, scan: ([flag]) => (flag === "--termhost-bench-generator" ? null : "") });
  await blind.adapter.launch(blind.ctx);
  await blind.adapter.spawnSessions(1, blind.argv);
  await blind.adapter.teardown();
  assert.match(blind.adapter.wslSamples().clockEnd.load, /read-back of the generators inside the distro failed/);
});

test("9P: the resize watcher's log writes share the run's I/O timer, so they land in io_ns too", async () => {
  const { createIoTimer, watchResize } = await import("../generator/ai-session.mjs");
  let now = 0n;
  const clock = { nowNs: () => now };
  const io = createIoTimer(clock);
  const slow = io.wrap(() => (now += 700_000_000n));
  const stream = new EventEmitter();
  const off = watchResize(stream, io.wrap(() => (now += 900_000_000n)), clock, () => ({ cols: 80, rows: 24 }));
  stream.emit("resize");
  slow();
  off();
  assert.equal(io.ns(), 1_600_000_000n, "the resize record's 900 ms and another log's 700 ms, one total");
  // main() wires that one timer to both (a real resize needs a TTY, so the wiring is read from the source).
  const src = readFileSync(join(import.meta.dirname, "..", "generator", "ai-session.mjs"), "utf8");
  assert.match(src, /const io = createIoTimer\(clock\);\n\s+const unwatchResize = watchResize\(process\.stdout, io\.wrap\(log\), clock, terminalSize\);/);
  assert.match(src, /runGenerator\(\{[\s\S]{0,400}?\n\s+clock,\n\s+io,\n/, "and hands it to the run");
});

test("the sampler takes a process's identity and its ticks from ONE stat read", () => {
  const stat = (pid, start, ticks) => `${pid} (node) S 1 ${pid} ${pid} 0 -1 0 0 0 0 0 ${ticks} 0 0 0 20 0 1 0 ${start} 1 1`;
  assert.deepEqual(statReading(42, stat(42, 900, 7)), { key: "42:900", ticks: 7 });
  // The PID is reused between the table read and this read: the key is the new process's.
  const reads = new Map([["/p/42/stat", stat(42, 950, 3)], ["/p/42/smaps_rollup", "Pss: 10 kB\n"]]);
  const r = readReadings([42], "/p", { read: (p) => reads.get(p) });
  assert.equal(r.keys.get(42), "42:950");
  assert.equal(r.ticks.get("42:950"), 3);
  const prev = new Map([["42:900", 100]]);
  assert.equal(groupReading([42], prev, r.ticks, r.pss, { dtMs: 500, clkTck: 100, keyOf: (p) => r.keys.get(p) }).cpu_ms, 0, "a new process: nothing, never 3 - 100");
  const gone = readReadings([43], "/p", { read: () => { throw new Error("ENOENT"); } });
  assert.equal(gone.ticks.get(gone.keys.get(43)), null);
});

test("producer_stall_pct at a window edge: only the I/O that cannot fit outside the window is attributed inside it (review repro edge.mjs)", () => {
  // a at 0 (io 0), b at 2500 ms (io 1000 ms: a's log write took 0..1000 ms, then a 1500 ms pane block); the window opens at 1000 ms.
  const pts = [];
  for (let t = -5000n * MS; t < 0n; t += 500n * MS) pts.push({ t, blocked: 0, io: 0, written: 0, scheduled: 0 });
  pts.push({ t: 0n, blocked: 0, io: 0, written: 0, scheduled: 0 });
  pts.push({ t: 2500n * MS, blocked: 0, io: 1000e6, written: 0, scheduled: 0 });
  for (let t = 3000n * MS; t <= 6000n * MS; t += 500n * MS) pts.push({ t, blocked: 0, io: 1000e6, written: 0, scheduled: 0 });
  const r = generatorWindow(new Map([[0, { points: pts }]]), { t0: 1000n * MS, t1: 6000n * MS });
  assert.equal(r.stall[0], 30, "the 1500 ms pane block in a 5000 ms window — not 18 %, from spreading the I/O evenly");
  // The I/O could all fall in the window: a gap wholly inside it still subtracts all of it.
  const whole = generatorWindow(new Map([[0, { points: pts }]]), { t0: -2000n * MS, t1: 6000n * MS });
  assert.equal(whole.stall[0], (1500 / 8000) * 100);
});
