/**
 * Which ConPTY host a Windows setup actually runs — `pty_host_impl` in the
 * results file's `setup` block — MEASURED from the process table rather than
 * assumed (plan `2026-10-11-terminal-page-performance-kpis-across-setups`, KPI
 * catalog diagnostics note: "The harness measures which host each setup
 * actually runs instead of assuming it").
 *
 * On Windows a pseudoconsole is served by a host process the CALLER of
 * `CreatePseudoConsole` starts as its own child: the system's `conhost.exe`
 * (kernelbase's ConPTY, which the runner's `portable-pty` uses), or a bundled
 * `OpenConsole.exe` when the app ships its own `conpty.dll` (VS Code with
 * `terminal.integrated.windowsUseConptyDll`, Windows Terminal). So the census
 * counts the pty-host processes whose PARENT is one of the host's own processes
 * — a root, or a process with a root's image name (VS Code's pty host is a
 * `Code.exe` utility process). A `conhost.exe` under some console tool the
 * host happened to run (a `git.exe` the editor spawned) has a foreign parent
 * and is not counted.
 *
 * Read-only: it takes a process table (`proc.mjs` `listProcesses`) and signals
 * nothing.
 */

import { descendantsOf } from "./proc.mjs";

const OPENCONSOLE = /^openconsole(\.exe)?$/i;
const CONHOST = /^conhost(\.exe)?$/i;

/**
 * @param {Map<number, {pid: number, ppid: number, name: string}>} table
 * @param {number[]} rootPids the host's root processes
 * @returns {{openconsole: number, conhost: number, roots: number}}
 */
export function ptyHostCensus(table, rootPids) {
  const roots = rootPids.filter((p) => Number.isInteger(p) && table.has(p));
  const hostNames = new Set(roots.map((p) => String(table.get(p).name).toLowerCase()));
  const tree = new Map();
  for (const r of roots) {
    tree.set(r, table.get(r));
    for (const d of descendantsOf(table, r)) tree.set(d.pid, d);
  }
  let openconsole = 0;
  let conhost = 0;
  for (const p of tree.values()) {
    const isOpen = OPENCONSOLE.test(p.name ?? "");
    const isCon = CONHOST.test(p.name ?? "");
    if (!isOpen && !isCon) continue;
    const parent = tree.get(p.ppid);
    if (!parent) continue;
    const hostParent = roots.includes(parent.pid) || hostNames.has(String(parent.name).toLowerCase());
    if (!hostParent) continue;
    if (isOpen) openconsole += 1;
    else conhost += 1;
  }
  return { openconsole, conhost, roots: roots.length };
}

/** The census's verdict: `bundled_openconsole`, `system_conhost`, or null when it saw no pty host. */
export function ptyHostFromCensus(census) {
  if (!census) return null;
  if (census.openconsole > 0) return "bundled_openconsole";
  if (census.conhost > 0) return "system_conhost";
  return null;
}

/**
 * `pty_host_impl` for a results row: the measured census when it saw a pty
 * host; otherwise the setting the app resolved (`settingImpl`, already mapped to
 * a catalog value) when there is one; otherwise an honest absence. Unix setups
 * are `unix_pty`.
 *
 * @param {{platform: string, census?: object | null, settingImpl?: string | null, what?: string}} o
 * @returns {string | {state: string, reason: string, detail: string}}
 */
export function ptyHostImpl({ platform, census = null, settingImpl = null, what = "the setup" }) {
  if (platform !== "win32") return "unix_pty";
  const measured = ptyHostFromCensus(census);
  if (measured) return measured;
  if (settingImpl) return settingImpl;
  return {
    state: "unknown",
    reason: "harness_error",
    detail: census
      ? `no conhost.exe / OpenConsole.exe under ${what}'s processes (census over ${census.roots} root(s))`
      : `the process table could not be read for ${what}'s pty-host census`,
  };
}
