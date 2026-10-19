/**
 * termhost-bench machine facts — the results file's `machine` block (Phase 1
 * schema): OS build, CPU, RAM, GPU, display size / refresh / scale, environment
 * id and the background load sampled just before the run. A fact that cannot be
 * established is the honest absence `{state, reason, detail}`, never a guess.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3.
 */

import { spawnSync } from "node:child_process";
import os from "node:os";

const MIB = 1024 * 1024;

/** Machine-wide CPU busy share (% of all logical cores) between two `os.cpus()` reads. */
export function cpuBusyPct(before, after) {
  let busy = 0;
  let total = 0;
  for (let i = 0; i < Math.min(before.length, after.length); i += 1) {
    const b = before[i].times;
    const a = after[i].times;
    const idle = a.idle - b.idle;
    const all = a.user - b.user + (a.nice - b.nice) + (a.sys - b.sys) + (a.irq - b.irq) + idle;
    busy += all - idle;
    total += all;
  }
  return total > 0 ? Math.max(0, (busy / total) * 100) : null;
}

/**
 * Sample the machine's background load over `ms` (before anything is launched).
 *
 * @returns {Promise<object>} the schema's background_load value or an absence
 */
export async function sampleBackgroundLoad(ms = 1000, { cpus = os.cpus, totalmem = os.totalmem, freemem = os.freemem } = {}) {
  const before = cpus();
  await new Promise((r) => setTimeout(r, ms));
  const after = cpus();
  const pct = cpuBusyPct(before, after);
  if (pct === null) return { state: "unknown", reason: "harness_error", detail: "os.cpus() reported no CPU time" };
  return {
    cpu_pct: Math.round(pct * 10) / 10,
    memory_used_mb: Math.round((totalmem() - freemem()) / MIB),
    sampled_ms: Math.max(1, Math.round(ms)),
  };
}

/** GPUs on Windows (Win32_VideoController), or an absence. */
export function detectGpu({ platform = process.platform, run = spawnSync } = {}) {
  if (platform !== "win32") {
    return {
      state: "unknown",
      reason: "not_instrumented",
      detail: "GPU inventory is read on Windows only; a linux-xvfb run renders on the CPU (llvmpipe) whatever is installed",
    };
  }
  const res = run(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Get-CimInstance Win32_VideoController | ForEach-Object { [pscustomobject]@{ n=[string]$_.Name; d=[string]$_.DriverVersion } } | ConvertTo-Json -Compress",
    ],
    { encoding: "utf8", windowsHide: true, timeout: 30000 },
  );
  if (res.status !== 0 || !res.stdout) {
    return { state: "unknown", reason: "harness_error", detail: `Win32_VideoController query failed (exit ${res.status})` };
  }
  try {
    const parsed = JSON.parse(res.stdout);
    const rows = (Array.isArray(parsed) ? parsed : [parsed]).filter((r) => r?.n);
    if (rows.length === 0) return { state: "unknown", reason: "harness_error", detail: "no video controller listed" };
    return rows.map((r) => ({
      name: r.n,
      driver: r.d ? r.d : { state: "unknown", reason: "harness_error", detail: "no driver version" },
    }));
  } catch (e) {
    return { state: "unknown", reason: "harness_error", detail: `unparseable controller list: ${e.message}` };
  }
}

/**
 * The display block: the environment's screen plus the refresh rate the probe
 * measured. The schema requires a refresh rate inside the block, so an unknown
 * rate makes the whole block an honest absence that still names the geometry.
 *
 * @param {{width: number, height: number, scalePct?: number} | null} screen
 * @param {{hz: number | null, known: boolean, source?: string | null}} refresh
 */
export function displayBlock(screen, refresh) {
  if (!screen) return { state: "unknown", reason: "not_instrumented", detail: "the environment did not report its screen" };
  if (!refresh?.known || !(refresh.hz > 0)) {
    return {
      state: "unknown",
      reason: "refresh_rate_unknown",
      detail: `${screen.width}x${screen.height} at ${screen.scalePct ?? 100}% scale; refresh ${refresh?.hz ?? "?"} Hz from "${refresh?.source ?? "nothing"}" is not a measured rate`,
    };
  }
  return {
    width_px: screen.width,
    height_px: screen.height,
    refresh_hz: refresh.hz,
    scale_pct: screen.scalePct ?? 100,
  };
}

/**
 * The machine block.
 *
 * @param {{environment: string, screen?: object | null, refresh?: object, backgroundLoad: object,
 *   gpu?: object, name?: string}} o
 */
export function machineBlock({ environment, screen = null, refresh = { hz: null, known: false }, backgroundLoad, gpu, name }) {
  const cpus = os.cpus();
  const platform = process.platform === "win32" ? "win32" : "linux";
  const release = os.release() || "unknown";
  let build;
  if (process.platform === "win32") {
    const parts = release.split(".");
    build = parts.length >= 3 ? parts[2] : { state: "unknown", reason: "harness_error", detail: `os.release() ${release}` };
  } else {
    const v = typeof os.version === "function" ? os.version() : "";
    build = v ? v : { state: "unknown", reason: "harness_error", detail: "os.version() unavailable" };
  }
  return {
    name: name ?? os.hostname() ?? "unknown",
    environment,
    os: { platform, release, build },
    cpu: { model: cpus[0]?.model?.trim() || "unknown", logical_cores: Math.max(1, cpus.length) },
    ram_mb: Math.max(1, Math.round(os.totalmem() / MIB)),
    gpu: gpu ?? detectGpu(),
    display: displayBlock(screen, refresh),
    background_load: backgroundLoad,
  };
}
