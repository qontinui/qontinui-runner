/**
 * Testable half of the session-info / restore-census oracle
 * (`scripts/uibridge-session-info.test.mjs`).
 *
 * Node built-ins only, and NO top-level side effects: importing this module
 * reads nothing, spawns nothing and kills nothing. Every OS read goes through an
 * injected `io` object so `scripts/__tests__/uibridge-session-info-lib.test.mjs`
 * runs without `/proc`, PowerShell, or a runner. Follows the
 * `scripts/perf-harness-lib.mjs` precedent: the oracle's `main()` stays in the
 * script and feeds these functions.
 *
 * Plan: `qontinui-dev-notes/plans/2026-11-08-tenant-badge-parity-and-oracle-fixtures.md`
 * Phase 3 (D3: the self-hosting gate resolves the TARGET runner's own pid from
 * the OS and fails closed; D4: T10 kills the pid the gate resolved and nothing
 * else). Later phases add their exports below the sections here.
 */

// ===========================================================================
// Process-image identity
// ===========================================================================

/**
 * Image names this oracle will NEVER kill — they host live Claude Code
 * sessions, including the caller's own. Compared against the normalised
 * basename (`imageBasename`), so `node.exe` and `/usr/bin/node` both match.
 */
export const NEVER_KILL = ["node", "powershell", "pwsh"];

/**
 * The runner's executable basenames, `.exe` stripped and lowercased. A
 * supervisor-started Windows primary runs as `qontinui-runner-primary.exe`;
 * every other runner as `qontinui-runner`.
 */
export const RUNNER_IMAGE_BASENAMES = ["qontinui-runner", "qontinui-runner-primary"];

/**
 * Reduce an image path or process name to a comparable basename: the last path
 * component, a trailing ` (deleted)` dropped (Linux `readlink /proc/<pid>/exe`
 * appends it once the binary on disk was replaced by a rebuild), a trailing
 * `.exe` dropped, lowercased. Returns "" for anything unusable.
 *
 * @param {unknown} name
 * @returns {string}
 */
export function imageBasename(name) {
  if (typeof name !== "string") return "";
  let s = name.trim().replace(/ \(deleted\)$/, "");
  const cut = Math.max(s.lastIndexOf("/"), s.lastIndexOf("\\"));
  if (cut >= 0) s = s.slice(cut + 1);
  return s.replace(/\.exe$/i, "").toLowerCase();
}

/**
 * THE image predicate: is this a qontinui runner executable? One predicate on
 * the exe BASENAME, used by the self-hosting gate, the context note and the
 * kill planner alike, so they cannot disagree about what a runner is.
 *
 * Linux `comm` truncates at 15 characters, so `qontinui-runner-primary` reads
 * as `qontinui-runner` there — still accepted. Resolution therefore prefers the
 * basename of `readlink /proc/<pid>/exe` and falls back to `comm`.
 *
 * @param {unknown} name an image path, a basename, or a process name
 * @returns {boolean}
 */
export function isRunnerImage(name) {
  return RUNNER_IMAGE_BASENAMES.includes(imageBasename(name));
}

/** @param {unknown} name @returns {boolean} */
export function isNeverKillImage(name) {
  return NEVER_KILL.includes(imageBasename(name));
}

// ===========================================================================
// Listener resolution — which pid serves 127.0.0.1:<port>?
// ===========================================================================

/** `/proc/net/tcp` state code for LISTEN. */
const TCP_LISTEN = "0A";

/**
 * Socket inodes of the LISTEN rows on `port` in a `/proc/net/tcp` or
 * `/proc/net/tcp6` body. The local address column is `<hex addr>:<hex port>`
 * for both families (IPv6 addresses are 32 hex digits), the state column is
 * `0A` for LISTEN, and the inode is column 10. ESTABLISHED (`01`) and every
 * other state are ignored: a client connection that happens to use the port
 * must never be mistaken for the server.
 *
 * Returns every matching inode (de-duplicated, in row order) — the caller
 * decides what more than one means. Malformed lines are skipped.
 *
 * @param {string} procNetTcpText
 * @param {number} port
 * @returns {string[]} inode numbers as decimal strings; [] when none
 */
export function listenerInodeForPort(procNetTcpText, port) {
  if (typeof procNetTcpText !== "string" || !Number.isInteger(port) || port <= 0) return [];
  const out = [];
  for (const line of procNetTcpText.split(/\r?\n/)) {
    const cols = line.trim().split(/\s+/);
    // sl local_address rem_address st tx:rx tr:tm retrnsmt uid timeout inode ...
    if (cols.length < 10 || !/^\d+:$/.test(cols[0])) continue;
    const local = cols[1];
    const colon = local.lastIndexOf(":");
    if (colon < 0) continue;
    const localPort = Number.parseInt(local.slice(colon + 1), 16);
    if (localPort !== port) continue;
    if (cols[3].toUpperCase() !== TCP_LISTEN) continue;
    const inode = cols[9];
    if (!/^\d+$/.test(inode) || inode === "0") continue;
    if (!out.includes(inode)) out.push(inode);
  }
  return out;
}

/**
 * The default OS door for `resolveTargetRunnerPid`. Built on call, never at
 * import, so importing this module stays side-effect free.
 *
 * @returns {Promise<ResolveIo>}
 */
export async function defaultResolveIo() {
  const fs = await import("node:fs/promises");
  const { execFile } = await import("node:child_process");
  const { promisify } = await import("node:util");
  const run = promisify(execFile);
  return {
    platform: process.platform,
    readFile: (p) => fs.readFile(p, "utf8"),
    readdir: (p) => fs.readdir(p),
    readlink: (p) => fs.readlink(p),
    runPowerShell: async (script) => {
      try {
        const { stdout } = await run("powershell.exe", ["-NoProfile", "-Command", script], {
          timeout: 30_000,
          windowsHide: true,
        });
        return { ok: true, stdout: String(stdout) };
      } catch (e) {
        return { ok: false, stdout: String(e?.stdout ?? ""), error: e?.message ?? String(e) };
      }
    },
  };
}

/**
 * @typedef {object} ResolveIo
 * @property {string} platform `process.platform` of the box being read
 * @property {(path: string) => string|Promise<string>} readFile
 * @property {(path: string) => string[]|Promise<string[]>} readdir
 * @property {(path: string) => string|Promise<string>} readlink
 * @property {(script: string) => {ok:boolean,stdout:string,error?:string}|Promise<{ok:boolean,stdout:string,error?:string}>} runPowerShell
 */

/**
 * @typedef {object} TargetResolution
 * @property {number|null} pid the runner pid listening on the port, ONLY when
 *   exactly one process holds the listener AND its image is a runner; else null
 * @property {string} source how it was read (`proc-net-tcp`, `Get-NetTCPConnection`, …)
 * @property {string|null} error why `pid` is null; null when it is not
 * @property {number|null} holderPid the single listener holder, whatever its
 *   image — so the kill planner can refuse a wrong image BY NAME
 * @property {string|null} imageName the holder's image as read
 */

/**
 * Which pid serves `127.0.0.1:<port>`, read from the OS — not from the runner,
 * so it reaches every build under test, including ones whose `/health` serves
 * no pid (plan D3, option A).
 *
 * - Linux: `/proc/net/tcp{,6}` LISTEN row → socket inode → the `/proc/<pid>/fd`
 *   entry linking to `socket:[<inode>]` → image from `readlink /proc/<pid>/exe`
 *   (basename), falling back to `/proc/<pid>/comm`.
 * - Windows: `Get-NetTCPConnection -LocalPort <port> -State Listen` →
 *   `OwningProcess` → `Get-Process -Id` (`Path`, falling back to `ProcessName`).
 * - Anything else: UNKNOWN, as the ancestry walk already is.
 *
 * Never throws. Every failure (no listener, more than one holder, a holder that
 * is not a runner, an OS read that cannot run) yields `pid: null` with the
 * reason in `error`; a GATING consumer must treat that as UNKNOWN and withhold.
 *
 * @param {number} port
 * @param {ResolveIo} [io] injected for tests; defaults to the real OS
 * @returns {Promise<TargetResolution>}
 */
export async function resolveTargetRunnerPid(port, io) {
  const base = { pid: null, source: "none", error: null, holderPid: null, imageName: null };
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    return { ...base, error: `invalid port ${JSON.stringify(port)}` };
  }
  let door = io;
  try {
    door = door ?? (await defaultResolveIo());
  } catch (e) {
    return { ...base, error: `cannot build the OS door: ${e?.message ?? e}` };
  }
  try {
    if (door.platform === "linux") return await resolveLinux(port, door);
    if (door.platform === "win32") return await resolveWindows(port, door);
    return {
      ...base,
      source: door.platform ?? "unknown-platform",
      error: `listener resolution is not implemented on platform '${door.platform}' — UNKNOWN`,
    };
  } catch (e) {
    return { ...base, error: `OS read failed: ${e?.message ?? String(e)}` };
  }
}

/** Shared tail: one holder pid + its image → a resolution. */
function verdictForHolder(source, holders, imageName, port) {
  const base = { pid: null, source, error: null, holderPid: null, imageName: null };
  if (holders.length === 0) {
    return { ...base, error: `no process holds a LISTEN socket on port ${port}` };
  }
  if (holders.length > 1) {
    return {
      ...base,
      error:
        `${holders.length} processes hold the LISTEN socket on port ${port} ` +
        `(pids ${holders.join(", ")}) — the target is ambiguous`,
    };
  }
  const holderPid = holders[0];
  if (!isRunnerImage(imageName)) {
    return {
      ...base,
      holderPid,
      imageName: imageName ?? null,
      error:
        `port ${port} is held by pid ${holderPid} whose image is '${imageName ?? "<unreadable>"}', ` +
        `not ${RUNNER_IMAGE_BASENAMES.join(" / ")}`,
    };
  }
  return { ...base, pid: holderPid, holderPid, imageName };
}

async function resolveLinux(port, io) {
  const source = "proc-net-tcp";
  const inodes = [];
  const readErrors = [];
  for (const file of ["/proc/net/tcp", "/proc/net/tcp6"]) {
    try {
      for (const ino of listenerInodeForPort(await io.readFile(file), port)) {
        if (!inodes.includes(ino)) inodes.push(ino);
      }
    } catch (e) {
      readErrors.push(`${file}: ${e?.message ?? e}`);
    }
  }
  if (readErrors.length === 2) {
    return {
      pid: null,
      source,
      error: `cannot read /proc/net/tcp{,6} (${readErrors.join("; ")})`,
      holderPid: null,
      imageName: null,
    };
  }
  if (inodes.length === 0) {
    return verdictForHolder(source, [], null, port);
  }

  const wanted = new Set(inodes.map((i) => `socket:[${i}]`));
  const holders = [];
  for (const entry of await io.readdir("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    let fds;
    try {
      fds = await io.readdir(`/proc/${entry}/fd`);
    } catch {
      continue; // another user's process, or it exited mid-scan
    }
    for (const fd of fds) {
      let target;
      try {
        target = await io.readlink(`/proc/${entry}/fd/${fd}`);
      } catch {
        continue;
      }
      if (wanted.has(target)) {
        const pid = Number(entry);
        if (!holders.includes(pid)) holders.push(pid);
        break;
      }
    }
  }
  if (holders.length === 0) {
    return {
      pid: null,
      source,
      error:
        `port ${port} has LISTEN socket inode(s) ${inodes.join(", ")} but no readable ` +
        `/proc/<pid>/fd links to them (a process this user cannot inspect?)`,
      holderPid: null,
      imageName: null,
    };
  }
  let imageName = null;
  if (holders.length === 1) {
    imageName = await linuxImageName(holders[0], io);
  }
  return verdictForHolder(source, holders, imageName, port);
}

/** Basename of `readlink /proc/<pid>/exe`, falling back to `comm` (15-char truncated). */
async function linuxImageName(pid, io) {
  try {
    const exe = imageBasename(await io.readlink(`/proc/${pid}/exe`));
    if (exe) return exe;
  } catch {
    // fall through to comm
  }
  try {
    const comm = String(await io.readFile(`/proc/${pid}/comm`)).trim();
    return comm || null;
  } catch {
    return null;
  }
}

async function resolveWindows(port, io) {
  const source = "Get-NetTCPConnection";
  // `port` is a validated integer, so interpolation cannot inject.
  const script =
    `$ids=@(Get-NetTCPConnection -LocalPort ${port} -State Listen -ErrorAction SilentlyContinue | ` +
    `Select-Object -ExpandProperty OwningProcess -Unique);` +
    `foreach($i in $ids){$p=Get-Process -Id $i -ErrorAction SilentlyContinue;` +
    `"$i|$($p.Path)|$($p.ProcessName)"}`;
  const r = await io.runPowerShell(script);
  if (!r.ok) {
    return {
      pid: null,
      source,
      error: `Get-NetTCPConnection could not run: ${r.error ?? "unknown error"}`,
      holderPid: null,
      imageName: null,
    };
  }
  const rows = [];
  for (const line of String(r.stdout).split(/\r?\n/)) {
    const [pidStr, path, name] = line.trim().split("|");
    if (!/^\d+$/.test(pidStr ?? "")) continue;
    const pid = Number(pidStr);
    if (pid === 0 || rows.some((x) => x.pid === pid)) continue;
    rows.push({ pid, image: imageBasename(path) || (name ? String(name).trim() : null) });
  }
  return verdictForHolder(
    source,
    rows.map((x) => x.pid),
    rows.length === 1 ? rows[0].image : null,
    port,
  );
}

// ===========================================================================
// The self-hosting verdict (plan D3)
// ===========================================================================

/**
 * Would restarting the `--port` target kill THIS process? A question about the
 * TARGET's own pid, answered by whether that pid is in this process's ancestry.
 *
 * The predecessor refused when ANY `qontinui-runner` was an ancestor — a proxy.
 * Measured 2026-11-08: target temp runner pid 149760 was not an ancestor, yet
 * the gate refused on ancestor pid 1158459, the primary. Another runner in the
 * chain is reported as CONTEXT, never as the verdict.
 *
 * Fails closed: an unwalkable ancestry or an unresolved target is UNKNOWN —
 * `evaluable: false, safe: false` — because the consumer is a gate.
 *
 * @param {{chain: {name:string,pid:number}[], target: {pid:number|null,error?:string|null,source?:string,imageName?:string|null}, registryNote?: string}} input
 * @returns {{safe:boolean, evaluable:boolean, selfHosted:boolean, targetPid:number|null, detail:string}}
 */
export function selfHostingVerdict({ chain, target, registryNote }) {
  const note = registryNote ? ` [${registryNote}]` : "";
  const links = Array.isArray(chain) ? chain : [];
  if (links.length === 0) {
    return {
      safe: false,
      evaluable: false,
      selfHosted: false,
      targetPid: target?.pid ?? null,
      detail:
        `process ancestry is empty — cannot prove this process is outside the target ` +
        `runner's process tree. Failing closed.${note}`,
    };
  }
  const targetPid = target?.pid ?? null;
  if (targetPid === null || targetPid === undefined) {
    return {
      safe: false,
      evaluable: false,
      selfHosted: false,
      targetPid: null,
      detail:
        `target runner pid unresolved (${target?.error ?? "no reason given"}) — UNKNOWN, so ` +
        `restarting it cannot be proven safe for this process. Failing closed.${note}`,
    };
  }
  const rendered = links.map((p) => `${p.name}(${p.pid})`).join(" <- ");
  const hit = links.find((p) => p.pid === targetPid);
  if (hit) {
    return {
      safe: false,
      evaluable: true,
      selfHosted: true,
      targetPid,
      detail:
        `SELF-HOSTED: the target runner (pid ${targetPid}, ${hit.name}) is an ancestor of this ` +
        `process (${rendered}). Restarting it would kill the test mid-run.${note}`,
    };
  }
  const others = links.filter((p) => p.pid !== targetPid && isRunnerImage(p.name));
  const context = others.length
    ? `; context: another runner is an ancestor — ${others.map((p) => `${p.name} pid ${p.pid}`).join(", ")} — which restarting the target does not touch`
    : "";
  return {
    safe: true,
    evaluable: true,
    selfHosted: false,
    targetPid,
    detail:
      `target runner pid ${targetPid} (${target?.imageName ?? "runner"}, via ${target?.source ?? "?"}) ` +
      `is not in this process's ancestry (${rendered})${context}${note}`,
  };
}

// ===========================================================================
// T10's kill (plan D4)
// ===========================================================================

/**
 * Decide whether T10 may kill `gatePid`. Pure. Refuses unless ALL hold:
 *
 * - the gate resolved a pid, and the fresh re-resolution (immediately before
 *   the kill) found the same pid holding the port;
 * - that holder's image is not a `NEVER_KILL` name, and is a runner;
 * - T10's restart door (`dev-start.ps1 -Runner` via PowerShell) can bring THIS
 *   target back: `--dev-start` named a script for it, or the platform is
 *   Windows and `port` is the port `dev-start.ps1` manages (T7's guard).
 *
 * @param {{gatePid:number|null, freshPid:number|null, imageName:string|null, port:number, devStartPort:number, devStartExplicit:boolean, platform:string, resolveError?:string|null}} input
 * @returns {{ok:true, pid:number, detail:string} | {ok:false, reason:string, detail:string}}
 */
export function planTargetKill({
  gatePid,
  freshPid,
  imageName,
  port,
  devStartPort,
  devStartExplicit,
  platform,
  resolveError = null,
}) {
  const refuse = (reason, detail) => ({
    ok: false,
    reason,
    detail: `REFUSED (${reason}): ${detail}`,
  });
  if (!Number.isInteger(gatePid) || gatePid <= 0) {
    return refuse("no-gate-pid", `the self-hosting gate resolved no target pid (${gatePid})`);
  }
  if (!Number.isInteger(freshPid) || freshPid <= 0) {
    return refuse(
      "target-unresolved",
      `re-resolving port ${port} immediately before the kill found no single holder` +
        (resolveError ? ` (${resolveError})` : ""),
    );
  }
  if (freshPid !== gatePid) {
    return refuse(
      "pid-changed",
      `port ${port} is now held by pid ${freshPid}, not the gate's pid ${gatePid}`,
    );
  }
  if (isNeverKillImage(imageName)) {
    return refuse(
      "never-kill",
      `pid ${freshPid} is '${imageName}', which hosts live sessions and is never killed`,
    );
  }
  if (!isRunnerImage(imageName)) {
    return refuse(
      "wrong-image",
      `pid ${freshPid} is '${imageName ?? "<unreadable>"}', not ${RUNNER_IMAGE_BASENAMES.join(" / ")}`,
    );
  }
  if (!devStartExplicit) {
    if (platform !== "win32") {
      return refuse(
        "no-restart-door",
        `T10's restart door is dev-start.ps1, which is Windows-by-substance; on '${platform}' ` +
          `pass --dev-start naming a script that restarts THIS instance`,
      );
    }
    if (port !== devStartPort) {
      return refuse(
        "port-not-managed",
        `--port ${port} is not the port dev-start.ps1 manages (${devStartPort}), so its ` +
          `-Runner would start a different runner than the one killed`,
      );
    }
  }
  return {
    ok: true,
    pid: gatePid,
    detail: `pid ${gatePid} ('${imageName}') still holds port ${port} and the restart door reaches it`,
  };
}

/**
 * T10's kill: re-resolve the target, plan, and — only on the ok arm — call
 * `killFn(gatePid, "SIGKILL")` exactly once. One pid, no tree semantics:
 * `SIGKILL` on POSIX, `TerminateProcess` on Windows (Node maps it). A bare
 * `process.kill(pid)` would send SIGTERM, contradicting T10's hard-crash
 * premise. Nothing else on the box is enumerated, let alone stopped.
 *
 * @param {number|null} gatePid the pid `gatesAllowMutation` resolved
 * @param {(pid:number, signal:string) => unknown} [killFn]
 * @param {{port:number, devStartPort:number, devStartExplicit:boolean, platform?:string, io?:ResolveIo, resolve?: typeof resolveTargetRunnerPid}} ctx
 * @returns {Promise<{ok:boolean, detail:string, plan:object}>}
 */
export async function killTargetRunner(
  gatePid,
  killFn = (pid, signal) => process.kill(pid, signal),
  ctx = /** @type {any} */ ({}),
) {
  const resolve = ctx.resolve ?? resolveTargetRunnerPid;
  const fresh = await resolve(ctx.port, ctx.io);
  const plan = planTargetKill({
    gatePid,
    freshPid: fresh.holderPid ?? fresh.pid ?? null,
    imageName: fresh.imageName ?? null,
    port: ctx.port,
    devStartPort: ctx.devStartPort,
    devStartExplicit: Boolean(ctx.devStartExplicit),
    platform: ctx.platform ?? ctx.io?.platform ?? process.platform,
    resolveError: fresh.error ?? null,
  });
  if (!plan.ok) return { ok: false, detail: plan.detail, plan };
  try {
    killFn(plan.pid, "SIGKILL");
  } catch (e) {
    return { ok: false, detail: `kill(${plan.pid}, SIGKILL) failed: ${e?.message ?? e}`, plan };
  }
  return {
    ok: true,
    detail: `killed target runner pid ${plan.pid} with SIGKILL (${plan.detail}; no tree flag, nothing else touched)`,
    plan,
  };
}
