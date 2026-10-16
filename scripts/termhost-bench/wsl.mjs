/**
 * WSL plumbing for the `tmux-wsl` setup (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 5): the
 * distro and its tools (the doctor's checks), Windows -> WSL paths, the clock
 * offset between the Linux VM's CLOCK_MONOTONIC and the Windows QPC timebase the
 * probe reads, the two samplers — the WSL-side `/proc` sampler (`wsl-sampler.mjs`)
 * and the Windows-side sampler of the shared WSL VM process — and the KPIs they
 * feed.
 *
 * Attribution (KPI catalog): the host of a tmux-wsl session is the portable
 * Windows Terminal's process tree on Windows (the probe samples it) PLUS the
 * private tmux server and its clients inside the distro (this sampler); the
 * shared WSL VM process (`vmmemWSL`, or `vmmem` on older builds) is recorded
 * separately as `wsl_vm_*` and attributed to no setup — everything running in
 * WSL is inside it, including the host's own tmux and the workload.
 *
 * Every invocation is `wsl.exe -d <distro> --exec <argv>` — no shell inside the
 * distro, so an argument reaches the Linux process as it was given.
 */

import { spawn, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, win32 } from "node:path";
import { fileURLToPath } from "node:url";

import { parseJsonl, toNs } from "./kpis.mjs";
import { measureKpi, unknownKpi } from "./stats.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));

/** The sampler script, run inside the distro. */
export const WSL_SAMPLER_SCRIPT = join(HERE, "wsl-sampler.mjs");

/** `%SystemRoot%\System32\wsl.exe`. */
export function wslExePath(env = process.env) {
  return win32.join(env.SystemRoot || env.windir || "C:\\Windows", "System32", "wsl.exe");
}

/** `[wsl.exe, ["-d", distro, "--exec", ...argv]]`. */
export function wslInvocation(distro, argv, { wslExe = "wsl.exe" } = {}) {
  if (!distro) throw new TypeError("a WSL invocation needs a distro");
  return [wslExe, ["-d", distro, "--exec", ...argv]];
}

/**
 * `wsl.exe -l -v` writes UTF-16LE; decode it (with or without a BOM) and parse
 * `[*] NAME STATE VERSION` rows.
 *
 * @param {Buffer | string} out
 * @returns {{name: string, isDefault: boolean, state: string, version: number}[]}
 */
export function parseWslList(out) {
  let text = out;
  if (Buffer.isBuffer(out)) {
    const looksUtf16 = out.length >= 2 && (out[1] === 0 || (out[0] === 0xff && out[1] === 0xfe));
    text = looksUtf16 ? out.toString("utf16le") : out.toString("utf8");
  }
  text = String(text).replace(/^\uFEFF/, "").replace(/\0/g, "");
  const rows = [];
  for (const line of text.split(/\r?\n/)) {
    const m = /^\s*(\*)?\s*(\S+)\s+(\S+)\s+(\d+)\s*$/.exec(line);
    if (!m || m[2] === "NAME") continue;
    rows.push({ name: m[2], isDefault: m[1] === "*", state: m[3], version: Number(m[4]) });
  }
  return rows;
}

/** The distro to use: the named one, or WSL's default. */
export function chooseDistro(list, wanted = null) {
  if (wanted) return list.find((d) => d.name === wanted) ?? null;
  return list.find((d) => d.isDefault) ?? null;
}

/**
 * A Windows path as WSL's default automount sees it (`C:\a\b` -> `/mnt/c/a/b`).
 * The adapter asks `wslpath -a` instead whenever it can (a distro may mount
 * elsewhere); this is the fallback and the tests' reference.
 */
export function toWslPath(winPath) {
  const m = /^([A-Za-z]):[\\/](.*)$/.exec(String(winPath));
  if (!m) throw new RangeError(`not an absolute Windows path: ${winPath}`);
  return `/mnt/${m[1].toLowerCase()}/${m[2].replace(/\\/g, "/")}`.replace(/\/+$/, "") || "/";
}

/** True for an absolute Windows path (the argv elements a WSL generator needs translated). */
export function isWindowsPath(s) {
  return /^[A-Za-z]:[\\/]/.test(String(s));
}

/**
 * The generator argv for a session inside WSL: the distro's `node`, and every
 * Windows path translated. Only the paths change: the flags and their order are
 * the one command line every setup runs (D13).
 *
 * @param {string[]} argv the harness's argv (`rundir.mjs` generatorArgv)
 * @param {{node: string, translate: (p: string) => string}} o
 */
export function wslGeneratorArgv(argv, { node, translate }) {
  return argv.map((a, i) => (i === 0 ? node : isWindowsPath(a) ? translate(a) : a));
}

// ---------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------

/**
 * The doctor's verdicts for the distro and the tools inside it (pure: takes
 * what each read returned).
 *
 * @param {{list: object[] | null, listError?: string | null, wanted?: string | null,
 *   tmux?: string | null, node?: string | null, clkTck?: string | null, bash?: boolean | null}} f
 * @returns {Record<string, {state: string, reason: string}>}
 */
export function mapWslFacts(f) {
  const m = {};
  const INOP = "INOPERATIVE-ON-THIS-MACHINE";
  if (!f.list) {
    m["setup.wsl"] = { state: INOP, reason: `wsl.exe -l -v did not answer${f.listError ? ` (${f.listError})` : ""}: WSL is not installed or not enabled` };
    for (const id of ["setup.wsl_tmux", "setup.wsl_node"]) m[id] = { state: "UNKNOWN", reason: "not checked: no WSL distro" };
    return m;
  }
  const d = chooseDistro(f.list, f.wanted ?? null);
  if (!d) {
    const names = f.list.map((x) => x.name).join(", ") || "none";
    m["setup.wsl"] = { state: INOP, reason: f.wanted ? `no WSL distro named ${f.wanted} (installed: ${names})` : `no default WSL distro (installed: ${names}); name one with --wsl-distro` };
    for (const id of ["setup.wsl_tmux", "setup.wsl_node"]) m[id] = { state: "UNKNOWN", reason: "not checked: no WSL distro" };
    return m;
  }
  m["setup.wsl"] =
    d.version === 2
      ? { state: "OPERATIVE", reason: `WSL 2 distro ${d.name}${d.isDefault ? " (default)" : ""}${f.bash === false ? "; no /bin/bash in it" : ""}` }
      : { state: INOP, reason: `distro ${d.name} is WSL ${d.version}; the tmux-wsl setup measures WSL 2 (one VM process to record)` };
  if (f.bash === false && m["setup.wsl"].state === "OPERATIVE") m["setup.wsl"] = { state: INOP, reason: `distro ${d.name} has no /bin/bash, the session shell` };
  m["setup.wsl_tmux"] = f.tmux ? { state: "OPERATIVE", reason: `${f.tmux} in ${d.name}` } : { state: INOP, reason: `tmux -V did not answer in ${d.name} (apt-get install tmux)` };
  const major = Number(/^v(\d+)/.exec(f.node ?? "")?.[1] ?? 0);
  m["setup.wsl_node"] = !f.node
    ? { state: INOP, reason: `node --version did not answer in ${d.name}: the generators run on the distro's node` }
    : major >= 20
      ? { state: "OPERATIVE", reason: `node ${f.node} in ${d.name}${f.clkTck ? `; CLK_TCK ${f.clkTck}` : ""}` }
      : { state: INOP, reason: `node ${f.node} in ${d.name} is older than v20` };
  return m;
}

/** Gather the doctor's WSL facts (impure). */
export function gatherWslFacts({ run = spawnSync, wslExe = "wsl.exe", wanted = null } = {}) {
  const r = run(wslExe, ["-l", "-v"], { timeout: 30000, windowsHide: true });
  if (r.error || r.status !== 0) return { list: null, listError: r.error?.message ?? `exit ${r.status}`, wanted };
  const list = parseWslList(r.stdout ?? Buffer.alloc(0));
  const d = chooseDistro(list, wanted);
  const ask = (argv) => {
    if (!d) return null;
    const [cmd, args] = wslInvocation(d.name, argv, { wslExe });
    const x = run(cmd, args, { encoding: "utf8", timeout: 60000, windowsHide: true });
    return x.error || x.status !== 0 ? null : String(x.stdout ?? "").trim().split("\n")[0] || null;
  };
  const bashOk = d ? run(...wslInvocation(d.name, ["test", "-x", "/bin/bash"], { wslExe }), { timeout: 30000, windowsHide: true }).status === 0 : null;
  return { list, wanted, distro: d?.name ?? null, tmux: ask(["tmux", "-V"]), node: ask(["node", "--version"]), clkTck: ask(["getconf", "CLK_TCK"]), bash: bashOk };
}

// ---------------------------------------------------------------------------
// Clock offset
// ---------------------------------------------------------------------------

/**
 * The WSL clock's offset from the host's, from round trips: the sample with the
 * smallest round trip bounds the error by half of it. `offsetNs` is
 * remote - host, so a WSL reading minus it is the host time.
 *
 * @param {{t0: bigint, t1: bigint, remote: bigint}[]} samples
 * @returns {{offsetNs: bigint, rttNs: bigint, n: number} | null}
 */
export function estimateClockOffset(samples) {
  const ok = samples.filter((s) => typeof s.t0 === "bigint" && typeof s.t1 === "bigint" && typeof s.remote === "bigint" && s.t1 >= s.t0);
  if (ok.length === 0) return null;
  const best = ok.reduce((a, b) => (b.t1 - b.t0 < a.t1 - a.t0 ? b : a));
  return { offsetNs: best.remote - (best.t0 + best.t1) / 2n, rttNs: best.t1 - best.t0, n: ok.length };
}

/** The echo script the distro's node runs: each newline answered with its hrtime. */
export const CLOCK_ECHO_SCRIPT =
  "process.stdin.on('data',d=>{for(const c of d.toString())if(c==='\\n')process.stdout.write(process.hrtime.bigint()+'\\n')});process.stdin.on('end',()=>process.exit(0))";

/**
 * Measure the offset over `rounds` round trips through `wsl.exe --exec node -e`.
 *
 * @returns {Promise<{offsetNs: bigint, rttNs: bigint, n: number}>}
 */
export function measureClockOffset({ distro, node = "node", wslExe = "wsl.exe", rounds = 15, spawnFn = spawn, now = () => process.hrtime.bigint(), timeoutMs = 30000 }) {
  return new Promise((resolve, reject) => {
    const [cmd, args] = wslInvocation(distro, [node, "-e", CLOCK_ECHO_SCRIPT], { wslExe });
    const child = spawnFn(cmd, args, { stdio: ["pipe", "pipe", "ignore"], windowsHide: true });
    const samples = [];
    let buf = "";
    let t0 = null;
    // A round trip that never answers leaves wsl.exe (our own child) running:
    // ended by its handle, then the measurement fails.
    const timer = setTimeout(() => {
      try {
        child.kill();
      } catch {
        // already gone
      }
      finish(new Error(`the clock round trip through ${distro} did not finish within ${timeoutMs / 1000} s`));
    }, timeoutMs);
    const finish = (err) => {
      clearTimeout(timer);
      try {
        child.stdin.end();
      } catch {
        // already closed
      }
      if (err) return reject(err);
      const est = estimateClockOffset(samples);
      return est ? resolve(est) : reject(new Error("no clock round trip completed"));
    };
    const ping = () => {
      t0 = now();
      child.stdin.write("\n");
    };
    child.on("error", (e) => finish(e));
    child.stdout.on("data", (d) => {
      buf += d.toString();
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const lineText = buf.slice(0, i).trim();
        buf = buf.slice(i + 1);
        const t1 = now();
        if (/^\d+$/.test(lineText) && t0 !== null) samples.push({ t0, t1, remote: BigInt(lineText) });
        if (samples.length >= rounds) return finish(null);
        ping();
      }
    });
    // The first answer includes node's start-up; it still bounds itself by its own round trip.
    ping();
  });
}

// ---------------------------------------------------------------------------
// The shared WSL VM process
// ---------------------------------------------------------------------------

/** The WSL 2 VM's process(es), read-only: `vmmemWSL` (newer Windows) or `vmmem`. */
export const VM_PROCESS_QUERY =
  "$ErrorActionPreference='Stop'; " +
  "@(Get-CimInstance -ClassName Win32_Process -Filter \"Name='vmmemWSL' OR Name='vmmem'\" | ForEach-Object { " +
  "[pscustomobject]@{ p=[int]$_.ProcessId; n=[string]$_.Name; t=$(if ($_.CreationDate) { [string]$_.CreationDate.ToFileTimeUtc() } else { '0' }) } }) | ConvertTo-Json -Compress -Depth 2";

/** The VM process to record: `vmmemWSL` first (WSL's own), else the one `vmmem`. */
export function pickVmProcess(text) {
  const t = String(text ?? "").trim();
  if (!t) return null;
  const parsed = JSON.parse(t);
  const rows = (Array.isArray(parsed) ? parsed : [parsed]).filter((r) => Number.isInteger(r?.p));
  const wsl = rows.find((r) => /^vmmemWSL$/i.test(r.n));
  const one = wsl ?? (rows.length === 1 ? rows[0] : null);
  return one ? { pid: one.p, name: one.n, startKey: String(one.t ?? "0") } : null;
}

/**
 * The Windows-side VM sampler: Windows PowerShell, one JSON line per interval,
 * timestamped with the QPC clock as Node's `process.hrtime` scales it on Windows
 * (`ticks * 1e9 / Frequency`), until `$env:TB_STOP` exists. Reads only
 * (`Get-Process` CPU time and private bytes); a reading the VM's protection
 * refuses is written as an error line, never as 0.
 */
export const VM_SAMPLER_SCRIPT =
  "$ErrorActionPreference='Stop'\n" +
  "$scale = 1e9 / [double][Diagnostics.Stopwatch]::Frequency\n" +
  "$out = [IO.StreamWriter]::new($env:TB_OUT, $true)\n" +
  "$out.AutoFlush = $true\n" +
  "$id = [int]$env:TB_VM_PID\n" +
  "while (-not (Test-Path -LiteralPath $env:TB_STOP)) {\n" +
  "  $t = [string][long]([double][Diagnostics.Stopwatch]::GetTimestamp() * $scale)\n" +
  "  try {\n" +
  "    $p = Get-Process -Id $id\n" +
  "    $out.WriteLine((@{ type='vm'; t_ns=$t; cpu_ms=[double]$p.TotalProcessorTime.TotalMilliseconds; private_mb=[double]$p.PrivateMemorySize64 / 1MB } | ConvertTo-Json -Compress))\n" +
  "  } catch {\n" +
  "    $out.WriteLine((@{ type='error'; t_ns=$t; detail=[string]$_.Exception.Message } | ConvertTo-Json -Compress))\n" +
  "  }\n" +
  "  Start-Sleep -Milliseconds ([int]$env:TB_INTERVAL_MS)\n" +
  "}\n" +
  "$out.Close()\n";

// ---------------------------------------------------------------------------
// KPIs
// ---------------------------------------------------------------------------

/** Records of a sampler file, or [] when there is none. */
export function readSamplerFile(path) {
  try {
    return parseJsonl(readFileSync(path, "utf8")).records;
  } catch {
    return [];
  }
}

const inWin = (t, win) => t !== null && t >= win.t0 && t <= win.t1;

/**
 * `wsl_vm_cpu_pct` and `wsl_vm_memory_mb` over the soak window, from the VM
 * sampler's cumulative CPU time and private bytes.
 *
 * @param {object[]} vmRecords
 * @param {{t0: bigint, t1: bigint} | null} win
 */
export function vmKpis(vmRecords, win) {
  if (!win) {
    return {
      wsl_vm_cpu_pct: unknownKpi("wsl_vm_cpu_pct", "harness_error", { detail: "no soak window" }),
      wsl_vm_memory_mb: unknownKpi("wsl_vm_memory_mb", "harness_error", { detail: "no soak window" }),
    };
  }
  const inside = vmRecords.filter((r) => inWin(toNs(r.t_ns), win));
  const errors = inside.filter((r) => r.type === "error");
  const vm = inside.filter((r) => r.type === "vm" && Number.isFinite(r.cpu_ms));
  if (vm.length === 0) {
    const detail = errors.length > 0 ? `the VM process could not be read: ${String(errors[0].detail).slice(0, 200)}` : vmRecords.length === 0 ? "the WSL VM sampler wrote nothing" : "no VM sample fell inside the soak window";
    return {
      wsl_vm_cpu_pct: unknownKpi("wsl_vm_cpu_pct", errors.length > 0 ? "harness_error" : "insufficient_samples", { n: 0, detail }),
      wsl_vm_memory_mb: unknownKpi("wsl_vm_memory_mb", errors.length > 0 ? "harness_error" : "insufficient_samples", { n: 0, detail }),
    };
  }
  const pct = [];
  for (let i = 1; i < vm.length; i += 1) {
    const dtMs = Number(toNs(vm[i].t_ns) - toNs(vm[i - 1].t_ns)) / 1e6;
    if (dtMs > 0) pct.push(Math.max(0, ((vm[i].cpu_ms - vm[i - 1].cpu_ms) / dtMs) * 100));
  }
  const last = vm[vm.length - 1];
  const notes = errors.length > 0 ? [`${errors.length} VM sample(s) in the window could not be read`] : [];
  return {
    wsl_vm_cpu_pct: errors.length > 0 ? unknownKpi("wsl_vm_cpu_pct", "harness_error", { n: pct.length, detail: notes[0] }) : measureKpi("wsl_vm_cpu_pct", pct),
    wsl_vm_memory_mb: Number.isFinite(last.private_mb) ? measureKpi("wsl_vm_memory_mb", { value: last.private_mb, n: vm.length }) : unknownKpi("wsl_vm_memory_mb", "harness_error", { n: vm.length, detail: "the last VM sample carried no private bytes" }),
  };
}

/**
 * The tmux-wsl host's `cpu_pct` and `memory_mb`: the probe's Windows-side host
 * group (the portable Windows Terminal tree) PLUS the WSL-side host group (the
 * private tmux server and clients) at the same moments. Each probe sample in the
 * window is paired with the WSL sample nearest in time (within `maxGapMs`); a
 * window where pairs are missing for more than a tenth of the probe's samples is
 * unknown, never a partial host.
 *
 * @param {{probeCpu: {t: bigint, v: number}[], probeMemEnd: number | null, wslRecords: object[], win: {t0: bigint, t1: bigint} | null, maxGapMs?: number}} o
 */
export function wslHostKpis({ probeCpu, probeMemEnd, wslRecords, win, maxGapMs = 600 }) {
  if (!win) {
    return {
      cpu_pct: unknownKpi("cpu_pct", "harness_error", { detail: "no soak window" }),
      memory_mb: unknownKpi("memory_mb", "harness_error", { detail: "no soak window" }),
    };
  }
  const wsl = wslRecords.filter((r) => r.type === "sample" && inWin(toNs(r.t_ns), { t0: win.t0 - BigInt(maxGapMs) * 1_000_000n, t1: win.t1 + BigInt(maxGapMs) * 1_000_000n }));
  const probe = probeCpu.filter((p) => inWin(p.t, win));
  if (wsl.length === 0) {
    const detail = wslRecords.length === 0 ? "the WSL-side sampler wrote nothing: the tmux server's share of the host is unmeasured" : "no WSL-side sample fell inside the soak window";
    return { cpu_pct: unknownKpi("cpu_pct", "harness_error", { detail }), memory_mb: unknownKpi("memory_mb", "harness_error", { detail }) };
  }
  const sums = [];
  let unpaired = 0;
  for (const p of probe) {
    let best = null;
    for (const w of wsl) {
      const gap = Math.abs(Number(toNs(w.t_ns) - p.t)) / 1e6;
      if (gap <= maxGapMs && (best === null || gap < best.gap)) best = { gap, w };
    }
    const v = best?.w.host?.cpu_pct;
    if (typeof v === "number" && Number.isFinite(v)) sums.push(p.v + v);
    else unpaired += 1;
  }
  const notes = [`includes the WSL-side tmux server and clients, sampled through /proc inside the distro`];
  const cpu =
    probe.length > 0 && unpaired > probe.length / 10
      ? unknownKpi("cpu_pct", "harness_error", { n: sums.length, detail: `${unpaired} of ${probe.length} probe samples had no readable WSL-side sample within ${maxGapMs} ms` })
      : measureKpi("cpu_pct", sums, { notes: unpaired > 0 ? [...notes, `${unpaired} probe sample(s) without a WSL-side partner left out`] : notes });
  const lastWsl = [...wsl].reverse().find((r) => inWin(toNs(r.t_ns), win) && typeof r.host?.pss_mb === "number");
  const memory =
    typeof probeMemEnd === "number" && Number.isFinite(probeMemEnd) && lastWsl
      ? measureKpi("memory_mb", { value: probeMemEnd + lastWsl.host.pss_mb, n: 1 }, { notes: ["Windows-side private bytes plus the WSL-side tmux server's PSS"] })
      : unknownKpi("memory_mb", "harness_error", { detail: lastWsl ? "the probe's host group has no end-of-window memory" : "no readable WSL-side memory sample in the soak window" });
  return { cpu_pct: cpu, memory_mb: memory };
}

/** A note appended to a KPI value (its own copy). */
function withNote(kpi, note) {
  return kpi ? { ...kpi, notes: [...(kpi.notes ?? []), note] } : kpi;
}

/**
 * The rest of a tmux-wsl row's WSL-specific readings, given the row's KPIs so far:
 *
 *   workload_*      the generators and their session shells run INSIDE the distro,
 *                   where the probe (on Windows) cannot see them: taken from the
 *                   WSL-side sampler's workload group, or unknown without it;
 *   latencies       carry the clock offset's error bound (half the fastest round
 *                   trip) and, when the teardown re-measure saw one, the drift;
 *   producer side   the generators' logs, slot files and stop-file poll live on the
 *                   Windows run directory, reached over WSL's 9P (/mnt/c): the run
 *                   loop and the probe read them there while the unit runs. The
 *                   time inside that I/O is recorded by the generator (`io_ns`) and
 *                   taken out of producer_stall_pct's record gaps (kpis.mjs
 *                   generatorWindow); between records it was never in it (only the
 *                   fd-1 pane write is timed as blocked). It still delays the
 *                   generator's stdin service and its schedule, so input_latency_ms
 *                   and delivered_ratio can carry it; all three say so.
 *
 * @param {Record<string, object>} kpis
 * @param {{wslRecords?: object[], clockRttNs?: string | null, clockEnd?: object | null} | null} wsl
 * @param {{t0: bigint, t1: bigint} | null} win
 * @returns {Record<string, object>} the values to set
 */
export function wslRowAdjustments(kpis, wsl, win) {
  const out = {};
  if (!wsl) {
    const detail = "the generators run inside WSL, where the Windows probe cannot see them, and the run passed no WSL-side samples";
    out.workload_cpu_pct = unknownKpi("workload_cpu_pct", "harness_error", { detail });
    out.workload_memory_mb = unknownKpi("workload_memory_mb", "harness_error", { detail });
  } else if (!win) {
    out.workload_cpu_pct = unknownKpi("workload_cpu_pct", "harness_error", { detail: "no soak window" });
    out.workload_memory_mb = unknownKpi("workload_memory_mb", "harness_error", { detail: "no soak window" });
  } else {
    const inside = (wsl.wslRecords ?? []).filter((r) => r.type === "sample" && inWin(toNs(r.t_ns), win) && r.workload);
    const note = "from the WSL-side sampler: the generators and their session shells run inside the distro";
    const withheld = inside.filter((r) => r.dt_ms !== null && r.workload.cpu_pct === null).length;
    const cpu = inside.map((r) => r.workload.cpu_pct).filter((v) => typeof v === "number" && Number.isFinite(v));
    out.workload_cpu_pct =
      withheld > 0
        ? unknownKpi("workload_cpu_pct", "harness_error", { n: cpu.length, detail: `${withheld} WSL-side workload sample(s) in the window had an unreadable member` })
        : measureKpi("workload_cpu_pct", cpu, { notes: [note] });
    const last = [...inside].reverse().find((r) => r.workload.pss_mb !== undefined);
    out.workload_memory_mb =
      last && typeof last.workload.pss_mb === "number"
        ? measureKpi("workload_memory_mb", { value: last.workload.pss_mb, n: inside.length }, { notes: [`${note} (PSS)`] })
        : unknownKpi("workload_memory_mb", inside.length === 0 ? "insufficient_samples" : "harness_error", { n: inside.length, detail: "no readable WSL-side workload memory at the end of the window" });
  }
  const rtt = wsl?.clockRttNs ? BigInt(wsl.clockRttNs) : null;
  const end = wsl?.clockEnd ?? null;
  const driftText =
    end?.state === "measured"
      ? `; by teardown it drifted ${Number(BigInt(end.driftNs)) / 1e6} ms (bound ±${Number(BigInt(end.boundNs)) / 1e6} ms)`
      : end?.state === "unknown"
        ? `; its drift by teardown is UNKNOWN (${end.reason})`
        : "; its drift by teardown was not re-measured";
  const clockNote =
    rtt === null
      ? "generator times were taken on the WSL VM's clock; the offset into the probe's timebase is unknown"
      : `generator times were shifted from the WSL VM's clock into the probe's; the offset's error bound is ±${Number(rtt / 2n) / 1e6} ms (half the fastest round trip)${driftText}`;
  for (const id of ["output_latency_ms", "input_latency_ms"]) if (kpis[id]?.state === "measured") out[id] = withNote(kpis[id], clockNote);
  const ioNote =
    "the tmux-wsl generators' logs, slot files and stop-file checks go over WSL's 9P to the Windows run directory: " +
    "that I/O time (io_ns) is taken out of producer_stall_pct's record gaps, but it also delays the generator's stdin service and schedule, " +
    "so input_latency_ms and delivered_ratio can include it";
  for (const id of ["producer_stall_pct", "delivered_ratio", "input_latency_ms"]) if (kpis[id]) out[id] = withNote(out[id] ?? kpis[id], ioNote);
  return out;
}
