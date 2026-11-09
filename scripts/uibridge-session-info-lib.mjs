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
 *
 * Phase 4 (D5-D7): eval-free fixtures — the PTY-buffer decoder, the launch-gate
 * menu answerer, the `POST /terminals` body, the restore-health session-id
 * picker, the `get-zone-view-state` parser, the account-roster check and the
 * spawn-admission retry classifier. Nothing here, or in the oracle, reaches the
 * webview's script-evaluation door or the Tauri global: the exe-mode CSP
 * (`script-src 'self'`) refuses string eval, and a static test pins that.
 *
 * Phase 5: the per-zone tenant badge — its expectation, the element comparison
 * and the cardinality check behind T0.2b / T2b.
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

/** PowerShell interpreters T10's restart door can use, in preference order. */
export const POWERSHELL_INTERPRETERS = ["powershell", "pwsh"];

/**
 * The first PowerShell interpreter on `PATH` (`powershell`, then `pwsh`), or
 * null. Pure: `exists` is injected. On Windows a bare name also matches
 * `<name>.exe`. A Linux box typically carries only `pwsh` (user-space install),
 * so a door that always spawned `powershell` would fail there AFTER the kill.
 *
 * @param {{pathEnv?:string, platform?:string, exists:(path:string)=>boolean}} input
 * @returns {string|null} the absolute interpreter path
 */
export function resolvePowerShellInterpreter({ pathEnv = "", platform = "linux", exists }) {
  const sep = platform === "win32" ? ";" : ":";
  const slash = platform === "win32" ? "\\" : "/";
  const dirs = String(pathEnv)
    .split(sep)
    .map((d) => d.trim())
    .filter(Boolean);
  for (const name of POWERSHELL_INTERPRETERS) {
    const names = platform === "win32" ? [`${name}.exe`, name] : [name];
    for (const dir of dirs) {
      for (const n of names) {
        const candidate = `${dir.replace(/[\\/]+$/, "")}${slash}${n}`;
        try {
          if (exists(candidate)) return candidate;
        } catch {
          // unreadable entry — keep looking
        }
      }
    }
  }
  return null;
}

/**
 * Decide whether T10 may kill `gatePid`. Pure. Refuses unless ALL hold:
 *
 * - the gate resolved a pid, and the fresh re-resolution (immediately before
 *   the kill) found the same pid holding the port;
 * - that holder's image is not a `NEVER_KILL` name, and is a runner;
 * - T10's restart door (`<interpreter> -File <devStart> -Runner`) can bring
 *   THIS target back: `--dev-start` named a script for it, or the platform is
 *   Windows and `port` is the port `dev-start.ps1` manages (T7's guard);
 * - off Windows, `port` is not the primary's port: the Linux primary is a
 *   systemd user service, which no `-File` door restarts;
 * - the door is RUNNABLE, proved before the kill: an interpreter resolved and
 *   the script exists. Killing first and discovering the door is missing
 *   leaves the target dead.
 *
 * @param {{gatePid:number|null, freshPid:number|null, imageName:string|null, port:number, devStartPort:number, devStartExplicit:boolean, platform:string, doorRunnable:boolean, interpreter?:string|null, devStart?:string|null, resolveError?:string|null}} input
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
  doorRunnable,
  interpreter = null,
  devStart = null,
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
  if (platform !== "win32" && port === devStartPort) {
    return refuse(
      "primary-not-restartable",
      `--port ${port} is the primary's port on '${platform}', where the primary is a systemd ` +
        `service no -File restart door brings back`,
    );
  }
  if (doorRunnable !== true) {
    return refuse(
      "door-unrunnable",
      `the restart door cannot run (interpreter ${interpreter ?? "<none of " + POWERSHELL_INTERPRETERS.join(", ") + " on PATH>"}, ` +
        `script ${devStart ?? "<unset>"}) — killing would leave the target dead`,
    );
  }
  return {
    ok: true,
    pid: gatePid,
    detail:
      `pid ${gatePid} ('${imageName}') still holds port ${port} and the restart door reaches it ` +
      `(${interpreter ?? "?"} -File ${devStart ?? "?"})`,
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
 * @param {{port:number, devStartPort:number, devStartExplicit:boolean, platform?:string, doorRunnable?:boolean, interpreter?:string|null, devStart?:string|null, io?:ResolveIo, resolve?: typeof resolveTargetRunnerPid}} ctx
 *   `doorRunnable` defaults to false: an unproven door refuses the kill.
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
    doorRunnable: ctx.doorRunnable === true,
    interpreter: ctx.interpreter ?? null,
    devStart: ctx.devStart ?? null,
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

// ===========================================================================
// Phase 4 — the PTY buffer, decoded for a human-shaped read
// ===========================================================================

/**
 * Decode a terminal's raw output into the text a person would read, for gate
 * matching. `GET /terminals/{id}/buffer` serves the PTY bytes; Claude Code's
 * TUI paints menus with cursor-column moves (`ESC[<n>G`) instead of spaces and
 * redraws a moved cursor with `ESC[<n>C`, so each of those becomes ONE space
 * BEFORE every other escape sequence is stripped. Without that, the prompt
 * reads `Isthisaproject…` and a redraw glues options together.
 *
 * Also drops OSC sequences (`ESC ] … BEL`, the shell-integration marks and
 * window titles), charset selects, `ESC 7`/`ESC 8`, carriage returns and other
 * C0 controls except `\n` and `\t`.
 *
 * @param {string} raw
 * @returns {string}
 */
export function terminalBufferText(raw) {
  if (typeof raw !== "string") return "";
  return (
    raw
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "")
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\[\d*[GC]/g, " ")
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\[[0-9;?<>=]*[ -/]*[@-~]/g, "")
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b[()][0-9A-Za-z]/g, "")
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b[78=>]/g, "")
      // eslint-disable-next-line no-control-regex
      .replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "")
  );
}

// ===========================================================================
// Phase 4 — launch gates, answered by label and verified before Enter (D7)
// ===========================================================================

/**
 * Build a whitespace-TOLERANT matcher from a readable phrase. A TUI paints
 * with cursor positioning, so even after `terminalBufferText` a phrase may
 * arrive with no spaces or with several; `\s*` between words absorbs both.
 *
 * @param {string} phrase
 * @returns {RegExp}
 */
export function gatePhrase(phrase) {
  const escaped = phrase.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(escaped.trim().split(/\s+/).join("\\s*"), "i");
}

/** The menu's cursor glyph (Claude Code v2.1.319). */
export const MENU_CURSOR = "❯";
/** Down / Up arrow, as the PTY receives them. */
export const KEY_DOWN = "\x1b[B";
export const KEY_UP = "\x1b[A";
export const KEY_ENTER = "\r";

/**
 * The first-run gates a real `claude` opens before its session starts — and so
 * before its SessionStart hook can confirm anything. Each is answered BY LABEL:
 * the options are listed in render order, `target` is the one chosen, and the
 * answerer verifies the cursor sits on `target` before it sends Enter. A key
 * that picks by position could choose `No, exit`.
 *
 * Measured on Claude Code v2.1.319 (plan Phase 1, coord finding 898efce4):
 * no option numbers are shown; trust opens with the cursor on `No, exit`, the
 * MCP menu with it already on `Continue without using this MCP server`.
 */
export const KNOWN_LAUNCH_GATES = [
  {
    id: "folder-trust",
    match: gatePhrase("Is this a project you created or one you trust"),
    options: ["No, exit", "Yes, I trust this folder"],
    target: "Yes, I trust this folder",
    why: "the fixture cwd is a directory this script created, so trusting it is not a judgement about code the operator has not seen",
  },
  {
    id: "mcp-server",
    match: gatePhrase("New MCP server found in this project"),
    options: [
      "Use this MCP server",
      "Use this and all future MCP servers in this project",
      "Continue without using this MCP server",
    ],
    target: "Continue without using this MCP server",
    why: "declining loads no server and judges no code",
  },
];

/** Gates this script REFUSES to answer, with the reason it gives the operator. */
export const REFUSED_LAUNCH_GATES = [
  {
    id: "external-claude-md-imports",
    match: gatePhrase("Allow external CLAUDE.md file imports"),
    why: "answering it would approve loading files from OUTSIDE the cwd, which is a security decision belonging to the operator rather than to a test. Use the default --fixture-cwd (a fresh temp dir), which never opens this gate.",
  },
];

/** Whitespace-free, lowercased — compares a label however the TUI spaced it. */
const squash = (s) =>
  String(s ?? "")
    .replace(/\s+/g, "")
    .toLowerCase();

/** Index just past the LAST match of `re` in `text`, or -1. */
function lastMatchEnd(text, re) {
  const g = new RegExp(re.source, re.flags.includes("g") ? re.flags : `${re.flags}g`);
  let end = -1;
  for (let m = g.exec(text); m !== null; m = g.exec(text)) {
    end = m.index + m[0].length;
    if (m[0].length === 0) g.lastIndex++;
  }
  return end;
}

/**
 * A refused gate open in `tail`, or null. One a caller has already recorded
 * as handled is skipped.
 *
 * @param {string} tail decoded buffer tail (`terminalBufferText`)
 * @param {Set<string>} [answered]
 * @returns {{id:string, why:string}|null}
 */
export function refusedGateIn(tail, answered = new Set()) {
  for (const gate of REFUSED_LAUNCH_GATES) {
    if (answered.has(gate.id)) continue;
    if (gate.match.test(String(tail ?? ""))) return { id: gate.id, why: gate.why };
  }
  return null;
}

/**
 * Decide the keys that answer `gate` from the decoded buffer tail. Pure.
 *
 * Reads the menu's LAST render — the text after the final occurrence of the
 * gate's title — and the LAST cursor glyph in it: a redraw after an arrow key
 * appends a partial line such as `  No, exit ❯ Yes, I trust this folder`, so
 * the last glyph is where the cursor is now.
 *
 * Returns:
 * - `{action:"none", reason}` — the gate is not showing, the caller already
 *   answered it (`answered`), or a later known gate's title follows it (it is
 *   scrollback, and re-answering would type into a live session);
 * - `{action:"confirm", keys:[Enter]}` — the cursor is on `target`;
 * - `{action:"move", keys:[Down|Up …]}` — arrows that bring the cursor to
 *   `target`; the caller sends them, re-reads, and plans again with
 *   `afterMove: true`;
 * - `{action:"refuse", reason}` — the menu is not the one we know (an option
 *   missing, no cursor, the cursor on an unknown line), or — after a move — the
 *   cursor is not on `target`. Nothing is guessed.
 *
 * @param {string} tail
 * @param {{id:string, match:RegExp, options:string[], target:string}} gate
 * @param {{answered?: Set<string>, afterMove?: boolean, laterGates?: {match:RegExp}[]}} [opts]
 */
export function planMenuAnswer(tail, gate, opts = {}) {
  const answered = opts.answered ?? new Set();
  const text = String(tail ?? "");
  if (answered.has(gate.id)) return { action: "none", reason: `${gate.id} already answered` };
  const titleEnd = lastMatchEnd(text, gate.match);
  if (titleEnd < 0) return { action: "none", reason: `${gate.id} is not showing` };

  const others = (opts.laterGates ?? KNOWN_LAUNCH_GATES).filter((g) => g.id !== gate.id);
  const region = text.slice(titleEnd);
  for (const other of others) {
    if (other.match.test(region)) {
      return {
        action: "none",
        reason: `${gate.id} is scrollback: ${other.id ?? "a later gate"} was drawn after it`,
      };
    }
  }

  const sq = squash(region);
  const missing = gate.options.filter((o) => !sq.includes(squash(o)));
  if (missing.length) {
    return {
      action: "refuse",
      reason: `${gate.id}: unrecognised menu — option(s) ${missing.map((o) => `'${o}'`).join(", ")} not rendered after the title`,
    };
  }
  const at = region.lastIndexOf(MENU_CURSOR);
  if (at < 0) {
    return { action: "refuse", reason: `${gate.id}: unrecognised menu — no ${MENU_CURSOR} cursor` };
  }
  const line = region.slice(at + MENU_CURSOR.length).split("\n")[0];
  const lineSq = squash(line).replace(/^\d+\./, "");
  // Longest label first, so a label that prefixes another cannot shadow it.
  const byLength = gate.options
    .map((o, i) => ({ o, i }))
    .sort((a, b) => squash(b.o).length - squash(a.o).length);
  const hit = byLength.find(({ o }) => lineSq.startsWith(squash(o)));
  if (!hit) {
    return {
      action: "refuse",
      reason: `${gate.id}: unrecognised menu — the cursor is on '${line.trim()}', which is none of ${gate.options.map((o) => `'${o}'`).join(", ")}`,
    };
  }
  const targetIdx = gate.options.indexOf(gate.target);
  const delta = targetIdx - hit.i;
  if (delta === 0) return { action: "confirm", keys: [KEY_ENTER], cursor: hit.o };
  if (opts.afterMove) {
    return {
      action: "refuse",
      reason: `${gate.id}: after the arrow keys the cursor is on '${hit.o}', not '${gate.target}' — Enter withheld`,
    };
  }
  return {
    action: "move",
    keys: Array(Math.abs(delta)).fill(delta > 0 ? KEY_DOWN : KEY_UP),
    cursor: hit.o,
  };
}

// ===========================================================================
// Phase 4 — fixture spawn (D5, Phase 1 spelling M1)
// ===========================================================================

/**
 * The `POST /terminals` body for one fixture, in Phase 1's measured spelling
 * M1: a bare `claude`, account-pinned by the runner. The runner validates
 * `account` against its roster (400 not in roster, 409 not logged in), seeds
 * `CLAUDE_CONFIG_DIR` and derives workspace trust for it; its identity seam
 * pins and records the session at spawn, so each terminal yields exactly one
 * authoritative, account-labelled record whose id is read back from
 * restore-health by `terminalId`.
 *
 * M2 (`claude --session-id <uuid>`) is deliberately NOT offered: it leaves a
 * superseded phantom row whose record outlives `DELETE` by ~163 s
 * (coord finding 898efce4).
 *
 * @param {{pageId:string, title:string, fixtureCwd:string, account:string}} input
 */
export function buildFixtureTerminalRequest({ pageId, title, fixtureCwd, account }) {
  return {
    page_id: pageId,
    title,
    working_dir: fixtureCwd,
    account,
    initial_command: "claude",
  };
}

/**
 * Pick a fixture's Claude session id from `/control/sessions/restore-health`
 * rows (the default, open-rows-only view) by its terminal. Pure.
 *
 * - one row → `{ok:true, sessionId, row}`;
 * - none → `{ok:false, pending:true}` — the seam has not recorded it yet;
 * - two or more → `{ok:false, pending:false}` naming them: M1 yields one record
 *   per terminal, so two is a defect to report, never a coin toss.
 *
 * @param {object[]} rows
 * @param {string} terminalId
 */
export function pickFixtureSessionId(rows, terminalId) {
  const mine = (Array.isArray(rows) ? rows : []).filter(
    (r) => r && String(r.terminalId) === String(terminalId) && r.claudeSessionId,
  );
  if (mine.length === 1)
    return { ok: true, sessionId: String(mine[0].claudeSessionId), row: mine[0] };
  if (mine.length === 0) {
    return {
      ok: false,
      pending: true,
      error: `no restore-health record names terminal ${terminalId} yet`,
    };
  }
  return {
    ok: false,
    pending: false,
    error:
      `${mine.length} open records name terminal ${terminalId} ` +
      `(${mine.map((r) => `${r.claudeSessionId} origin=${r.origin ?? "?"} confirmed=${r.confirmed ?? "?"}`).join("; ")}) — ` +
      `expected exactly one`,
  };
}

/**
 * Parse the `terminal-page` `get-zone-view-state` action result. Pure.
 *
 * Fixture creation needs `pageId` (where a `POST /terminals` terminal mounts),
 * `assignments` (the grid zone map the rendered zone is read from) and
 * `workMode`. A build that serves none of them predates plan
 * `2026-11-08-tenant-badge-parity-and-oracle-fixtures` Phase 4, and the old
 * fallbacks (a `default` page guess, an evaluated `localStorage` read) are not
 * taken: an absent field is UNKNOWN, and fixture creation is refused naming the
 * build.
 *
 * Also refuses tmux work mode: one viewport is not a grid, so "distinct
 * rendered zones" cannot be posed there.
 *
 * @param {unknown} data the action's result (unwrapped)
 * @param {string} build the runner build id, for the refusal text
 */
export function parseZoneViewState(data, build) {
  const d = data && typeof data === "object" ? /** @type {any} */ (data) : null;
  if (!d) {
    return { ok: false, error: `get-zone-view-state returned no object (runner build ${build})` };
  }
  const missing = [];
  if (typeof d.pageId !== "string" || d.pageId.trim() === "") missing.push("pageId");
  if (!d.assignments || typeof d.assignments !== "object" || Array.isArray(d.assignments)) {
    missing.push("assignments");
  }
  if (d.workMode !== "grid" && d.workMode !== "tmux") missing.push("workMode");
  if (missing.length) {
    return {
      ok: false,
      error:
        `get-zone-view-state carries no ${missing.join(", ")} — runner build ${build} predates ` +
        `plan 2026-11-08-tenant-badge-parity-and-oracle-fixtures Phase 4, so fixture creation is ` +
        `refused (no page guess, no evaluated localStorage read)`,
    };
  }
  /** @type {Record<number,string>} */
  const assignments = {};
  for (const [k, v] of Object.entries(d.assignments)) {
    if (/^\d+$/.test(k) && typeof v === "string") assignments[Number(k)] = v;
  }
  const view = {
    pageId: d.pageId,
    assignments,
    workMode: d.workMode,
    zoneCount: Number.isInteger(d.zoneCount) ? d.zoneCount : null,
    layoutId: typeof d.layoutId === "string" ? d.layoutId : null,
    maximizedZone: Number.isInteger(d.maximizedZone) ? d.maximizedZone : null,
  };
  if (view.workMode === "tmux") {
    return {
      ok: false,
      tmux: true,
      view,
      error:
        `the terminal page is in tmux work mode — one viewport is not a grid, so fixtures cannot ` +
        `be placed in distinct rendered zones. Switch the page to grid mode first.`,
    };
  }
  return { ok: true, view };
}

/** The zone index the grid gives `terminalId`, or null. */
export function zoneIndexOf(assignments, terminalId) {
  for (const [zone, tid] of Object.entries(assignments ?? {})) {
    if (String(tid) === String(terminalId)) return Number(zone);
  }
  return null;
}

// ===========================================================================
// Phase 4 — the account roster (a supervisor temp runner starts EMPTY)
// ===========================================================================

/** Mirror of the runner's `derive_account_name` (`claude_session/federation.rs`). */
export function deriveAccountName(configDir) {
  const s = String(configDir ?? "");
  const base = s.split(/[/\\]/).pop() ?? s;
  const stripped = base.replace(/^\.+/, "");
  const name = stripped.startsWith("claude-") ? stripped.slice("claude-".length) : "";
  return name || "default";
}

/** Mirror of the runner's `resolve_from` matching (`ai_provider/account_usage.rs`). */
export function accountInRoster(account, roster) {
  const want = String(account ?? "").trim();
  const dirs = Array.isArray(roster) ? roster : [];
  if (!want) return false;
  const lower = want.toLowerCase();
  return dirs.some((d) => {
    const dir = String(d);
    const base = dir.split(/[/\\]/).pop() ?? dir;
    return (
      dir === want || deriveAccountName(dir).toLowerCase() === lower || base.toLowerCase() === lower
    );
  });
}

/**
 * Plan the roster step before any fixture spawn. Pure.
 *
 * A supervisor temp runner starts with an EMPTY roster (measured: `{"dirs":[]}`,
 * and `POST /terminals` with an account answers 400 `not in roster`). This
 * decides what to do about the `--accounts` that do not resolve:
 *
 * - every account resolves → `{action:"none"}`;
 * - some do not and `seedDirs` is empty → `{action:"refuse"}` naming them and
 *   the opt-in flag (the roster is never changed unasked);
 * - `--seed-roster` on the protected port (the primary) → `{action:"refuse"}`:
 *   the operator's live roster is never edited by a test;
 * - otherwise → `{action:"seed", next, prior}`: the union, prior first, so
 *   teardown can restore `prior` exactly.
 *
 * @param {{current:string[], accounts:string[], seedDirs:string[], port:number, protectedPort:number}} input
 */
export function planRosterSeed({ current, accounts, seedDirs, port, protectedPort }) {
  const roster = Array.isArray(current) ? current.map(String) : [];
  const missing = (accounts ?? []).filter((a) => !accountInRoster(a, roster));
  const shown = roster.length ? roster.map(deriveAccountName).join(", ") : "EMPTY";
  if (missing.length === 0) return { action: "none", missing: [], prior: roster };
  const seeds = (seedDirs ?? []).map(String).filter(Boolean);
  if (seeds.length === 0) {
    return {
      action: "refuse",
      missing,
      prior: roster,
      error:
        `account(s) ${missing.join(", ")} are not in this runner's roster (${shown}). A supervisor ` +
        `temp runner starts with an EMPTY roster. Re-run with --seed-roster <configDir>[,<configDir>…] ` +
        `to add them for this run (each dir needs a projects/ subdirectory; the prior roster is restored ` +
        `at teardown).`,
    };
  }
  if (port === protectedPort) {
    return {
      action: "refuse",
      missing,
      prior: roster,
      error:
        `--seed-roster refused on port ${port}: that is the primary runner's port, and its roster is the ` +
        `operator's live setting. Seed only a temp runner.`,
    };
  }
  const next = [...roster];
  for (const d of seeds) if (!next.includes(d)) next.push(d);
  return { action: "seed", missing, prior: roster, next };
}

/**
 * Check the roster the runner ACCEPTED after a seed `PUT`: the PUT silently
 * drops a dir with no `projects/` subdirectory, so the read-back is the truth.
 *
 * @param {{accepted:string[], requested:string[], accounts:string[]}} input
 */
export function verifyRosterSeed({ accepted, requested, accounts }) {
  const got = Array.isArray(accepted) ? accepted.map(String) : [];
  const dropped = (requested ?? []).filter((d) => !got.includes(String(d)));
  const unresolved = (accounts ?? []).filter((a) => !accountInRoster(a, got));
  if (unresolved.length === 0) return { ok: true, dropped, unresolved };
  return {
    ok: false,
    dropped,
    unresolved,
    error:
      `after --seed-roster the runner's roster still lacks ${unresolved.join(", ")}` +
      (dropped.length
        ? `; it dropped ${dropped.join(", ")} (no projects/ subdirectory there)`
        : "") +
      ` — accepted roster: ${got.length ? got.join(", ") : "EMPTY"}`,
  };
}

/** Same members, order ignored. */
export function sameRoster(a, b) {
  const x = [...new Set((a ?? []).map(String))].sort();
  const y = [...new Set((b ?? []).map(String))].sort();
  return x.length === y.length && x.every((v, i) => v === y[i]);
}

// ===========================================================================
// Phase 4 — spawn admission (the runner's resource guard)
// ===========================================================================

/** Bounded backoff for a guard-refused spawn. */
export const SPAWN_RETRY_BUDGET_MS = 120_000;
export const SPAWN_RETRY_BASE_MS = 2_000;
export const SPAWN_RETRY_CAP_MS = 15_000;

/**
 * Classify a `POST /terminals` answer. Pure.
 *
 * - 2xx → `{kind:"ok"}`;
 * - 500 whose error names `resource_guard:` → `{kind:"admission", retry:true}`:
 *   the runner's spawn admission refused for load (e.g.
 *   `resource_guard:critical:thread_count: …`), which passes;
 * - 400 `not in roster` → `{kind:"roster"}`;
 * - 409 → `{kind:"not-logged-in"}`;
 * - anything else → `{kind:"error"}`. Only `admission` is ever retried.
 *
 * @param {{status:number, ok?:boolean, text?:string, json?:any, error?:string|null}} res
 */
export function classifyTerminalSpawn(res) {
  const status = Number(res?.status ?? 0);
  const body =
    typeof res?.json?.error === "string"
      ? res.json.error
      : typeof res?.text === "string"
        ? res.text
        : String(res?.error ?? "");
  if (status >= 200 && status < 300) return { kind: "ok", retry: false, detail: "" };
  if (status === 500 && /resource_guard:/.test(body)) {
    return { kind: "admission", retry: true, detail: body.trim() };
  }
  if (status === 400 && /not in roster/i.test(body)) {
    return { kind: "roster", retry: false, detail: body.trim() };
  }
  if (status === 409) return { kind: "not-logged-in", retry: false, detail: body.trim() };
  return {
    kind: "error",
    retry: false,
    detail: `HTTP ${status || "?"}${body ? `: ${body.trim()}` : ` (${res?.error ?? "no body"})`}`,
  };
}

/**
 * The next step after a refused spawn. Pure. `admissionVerdict` is `/health`
 * `spawnAdmission.threads.verdict` as last read (reported, never trusted to
 * skip a retry — the guard answered, so the guard decides).
 *
 * The budget covers the WHOLE retry, not just the sleeps: `elapsedMs` is read
 * after the refused request and its `/health` read, and `lastAttemptMs` (that
 * attempt's request + `/health` time) stands in for what the NEXT attempt will
 * cost, so a retry is scheduled only when sleep + another such attempt still
 * fits. Worst case: one attempt slower than the one before it can overrun the
 * budget by at most one request timeout plus one `/health` timeout (the
 * oracle's 30 s + 20 s).
 *
 * @param {{classification:{kind:string,retry:boolean,detail:string}, attempt:number, elapsedMs:number, lastAttemptMs?:number, budgetMs?:number, admissionVerdict?:string|null}} input
 * @returns {{retry:true, delayMs:number} | {retry:false, reason:string}}
 */
export function planSpawnRetry({
  classification,
  attempt,
  elapsedMs,
  lastAttemptMs = 0,
  budgetMs = SPAWN_RETRY_BUDGET_MS,
  admissionVerdict = null,
}) {
  if (!classification?.retry) {
    return { retry: false, reason: classification?.detail ?? "not retryable" };
  }
  const delayMs = Math.min(SPAWN_RETRY_CAP_MS, SPAWN_RETRY_BASE_MS * 2 ** Math.max(0, attempt));
  const nextCost = Number.isFinite(lastAttemptMs) && lastAttemptMs > 0 ? lastAttemptMs : 0;
  if (elapsedMs + delayMs + nextCost > budgetMs) {
    return {
      retry: false,
      reason:
        `the runner's spawn admission kept refusing for ${Math.round(elapsedMs / 1000)}s ` +
        `(${attempt + 1} attempt(s), budget ${Math.round(budgetMs / 1000)}s): ${classification.detail}` +
        ` — /health spawnAdmission.threads.verdict=${admissionVerdict ?? "unread"}`,
    };
  }
  return { retry: true, delayMs };
}

/**
 * Which of the owned fixtures are still in the open set. Pure. Residue is
 * NAMED — a record still open after cleanup is reported, never dropped.
 *
 * @param {object[]} openRows restore-health rows (open set)
 * @param {{sessionIds:Set<string>, terminalIds:Set<string>}} owned
 * @returns {string[]} `<session>@<terminal>` for each open owned row
 */
export function openResidue(openRows, owned) {
  return (Array.isArray(openRows) ? openRows : [])
    .filter(
      (r) =>
        owned.sessionIds.has(String(r?.claudeSessionId)) ||
        owned.terminalIds.has(String(r?.terminalId)),
    )
    .map((r) => `${r.claudeSessionId}@${r.terminalId}`);
}

// ===========================================================================
// Phase 5 — the per-zone tenant badge (T0.2b, T2b)
// ===========================================================================

/**
 * The badge's per-zone UI Bridge id. This is a DEPENDENCY on PR-A of this plan
 * (plan D1): PR-A adds `tenantBadgeElementId` to
 * `src/components/terminal/TenantBadge.tsx`, which is NOT yet on this branch —
 * until PR-A lands the runner still stamps the legacy shared id, and this
 * literal is the shape the oracle expects it to produce. Kept literal because
 * the oracle reads a runner it does not import from; the per-zone family
 * (`terminal-zone-header-<n>`, `terminal-session-info-<field>-<n>`) is spelled
 * the same way.
 *
 * @param {number} zoneIndex
 */
export function tenantBadgeElementId(zoneIndex) {
  return `terminal-tenant-badge-${zoneIndex}`;
}

/** The pre-D1 id every zone's badge shared — its presence is the defect. */
export const LEGACY_TENANT_BADGE_ID = "terminal.tenant-badge";

/** The spawn-tenant picker's id: registered only when `showSwitcher`. */
export const SPAWN_TENANT_PICKER_ID = "terminal.spawn-tenant-picker";

/**
 * Should the zone's badge render? Mirrors `tenantBadgeLabel`'s null rule
 * (`TenantBadge.tsx`): hidden unless `showSwitcher`, and then shown only when
 * something names a tenant — the spawn stamp, a divergence, or a RESOLVED
 * acting tenant in `tenancy.display`. Pure.
 *
 * - `pickerPresent` is `terminal.spawn-tenant-picker` being registered, the
 *   observable of `showSwitcher`; a non-boolean (the registry read failed) is
 *   UNKNOWN.
 * - `tenancy` null (no block, or a runner predating it) with the picker
 *   present is UNKNOWN; so is a block with nothing to say whose `display` is
 *   null (a runner predating `tenancy.display` could still have resolved one).
 *
 * @param {{pickerPresent: boolean|null|undefined, tenancy: any}} input
 * @returns {{expect:"present"|"absent"|"unknown", reason:string}}
 */
export function tenantBadgeExpectation({ pickerPresent, tenancy }) {
  if (typeof pickerPresent !== "boolean") {
    return {
      expect: "unknown",
      reason: `whether ${SPAWN_TENANT_PICKER_ID} is registered could not be read, so showSwitcher is UNKNOWN`,
    };
  }
  if (!pickerPresent) {
    return {
      expect: "absent",
      reason: `${SPAWN_TENANT_PICKER_ID} is not registered (showSwitcher false: <= 1 tenant binding)`,
    };
  }
  if (!tenancy || typeof tenancy !== "object") {
    return {
      expect: "unknown",
      reason: "the session carries no tenancy block, so what the badge would name is UNKNOWN",
    };
  }
  const stamped = typeof tenancy.row?.tenantId === "string" && tenancy.row.tenantId.trim() !== "";
  const diverged = tenancy.diverged === true;
  const display = tenancy.display ?? null;
  const resolved = display?.status === "resolved" && Boolean(display.tenantId);
  if (stamped || diverged || resolved) {
    const why = [
      stamped && "a spawn stamp",
      diverged && "a divergence",
      resolved && "a resolved acting tenant",
    ]
      .filter(Boolean)
      .join(" and ");
    return { expect: "present", reason: `showSwitcher and ${why}` };
  }
  if (!display) {
    return {
      expect: "unknown",
      reason:
        "no stamp and no divergence, and tenancy.display is absent (runner predates it) — UNKNOWN",
    };
  }
  return {
    expect: "absent",
    reason: `showSwitcher, but nothing names a tenant (no stamp, no divergence, display.status=${display.status})`,
  };
}

/** A dataset/attribute read that tells "absent" from "present and empty". */
function readAttr(element, kebab) {
  const camel = kebab
    .replace(/^data-/, "")
    .split("-")
    .map((p, i) => (i === 0 ? p : p[0].toUpperCase() + p.slice(1)))
    .join("");
  const ds = element?.state?.dataset;
  if (ds && typeof ds === "object" && camel in ds && ds[camel] !== undefined) {
    return { present: true, value: String(ds[camel]) };
  }
  for (const bag of [element?.attributes, element?.state?.attributes]) {
    if (bag && typeof bag === "object" && kebab in bag && bag[kebab] !== undefined) {
      return { present: true, value: String(bag[kebab]) };
    }
  }
  return { present: false, value: undefined };
}

/** The badge's full label: aria-label / title carry it; the visible text is a stem. */
function badgeLabel(element) {
  for (const v of [
    element?.label,
    element?.accessibleName,
    element?.state?.accessibleName,
    element?.attributes?.["aria-label"],
    element?.state?.attributes?.["aria-label"],
    element?.attributes?.title,
    element?.state?.attributes?.title,
  ]) {
    if (typeof v === "string" && v.trim() !== "") return v;
  }
  return null;
}

/**
 * Compare ONE zone's badge element with its expectation. Pure.
 *
 * Findings (each `{check, status, detail}`, status PASS / FAIL / UNKNOWN):
 * - presence — the element is there exactly when expected;
 * - label — a diverged session's label starts `TENANT MISMATCH`; otherwise it
 *   names the stamped tenant, else the resolved acting tenant;
 * - diverged — `data-tenant-diverged` equals `tenancy.diverged`;
 * - acting — `data-tenant-acting` is ABSENT when `display.source` is
 *   `row_stamp` (a record of the spawn's choice, never an observation), else
 *   `display.tenantId` (open PR #2510's `tenantBadgeDisplayAttrs` rule). A
 *   badge without `data-tenant-display-status` is from a build that predates
 *   #2510: the acting comparison is UNKNOWN naming the build, never PASS.
 *
 * An `unknown` expectation renders the whole arm UNKNOWN with its reason.
 * Overall: any FAIL → FAIL; else any UNKNOWN → UNKNOWN; else PASS.
 *
 * @param {object|null} element the badge element (null when not registered)
 * @param {{expect:string, reason:string}} expectation
 * @param {any} tenancy
 * @param {{build?:string, label?:string}} [opts]
 */
export function compareBadgeElement(element, expectation, tenancy, opts = {}) {
  const build = opts.build ?? "unknown";
  const who = opts.label ?? "badge";
  const findings = [];
  const add = (check, status, detail) => findings.push({ check, status, detail });
  const verdict = () => {
    const status = findings.some((f) => f.status === "FAIL")
      ? "FAIL"
      : findings.some((f) => f.status === "UNKNOWN")
        ? "UNKNOWN"
        : "PASS";
    return { status, findings };
  };

  if (expectation?.expect === "unknown") {
    add("expectation", "UNKNOWN", `${who}: expectation UNKNOWN — ${expectation.reason}`);
    return verdict();
  }
  if (expectation?.expect === "absent") {
    if (element) {
      add("presence", "FAIL", `${who}: rendered, but must not (${expectation.reason})`);
    } else {
      add("presence", "PASS", `${who}: absent, as expected (${expectation.reason})`);
    }
    return verdict();
  }
  if (!element) {
    add("presence", "FAIL", `${who}: NOT rendered, but expected (${expectation?.reason ?? "?"})`);
    return verdict();
  }
  add("presence", "PASS", `${who}: rendered`);

  const diverged = tenancy?.diverged === true;
  const display = tenancy?.display ?? null;
  const label = badgeLabel(element);
  if (label === null) {
    add(
      "label",
      "UNKNOWN",
      `${who}: the element carries no label/accessibleName/aria-label to read`,
    );
  } else if (diverged) {
    if (label.trimStart().startsWith("TENANT MISMATCH"))
      add("label", "PASS", `${who}: TENANT MISMATCH`);
    else
      add(
        "label",
        "FAIL",
        `${who}: the session is diverged but the label reads '${label.slice(0, 80)}'`,
      );
  } else {
    const stamp = typeof tenancy?.row?.tenantId === "string" ? tenancy.row.tenantId.trim() : "";
    const acting = display?.status === "resolved" ? display.tenantId : null;
    const want = stamp || acting || null;
    if (!want)
      add("label", "UNKNOWN", `${who}: no stamped or resolved tenant to look for in the label`);
    else if (label.includes(want)) add("label", "PASS", `${who}: names ${want}`);
    else add("label", "FAIL", `${who}: the label does not name ${want}: '${label.slice(0, 120)}'`);
  }

  const dv = readAttr(element, "data-tenant-diverged");
  const wantDiverged = diverged ? "true" : "false";
  if (!dv.present)
    add("diverged", "FAIL", `${who}: data-tenant-diverged is absent (expected '${wantDiverged}')`);
  else if (dv.value === wantDiverged)
    add("diverged", "PASS", `${who}: data-tenant-diverged='${dv.value}'`);
  else
    add(
      "diverged",
      "FAIL",
      `${who}: data-tenant-diverged='${dv.value}', backend diverged=${diverged}`,
    );

  const status = readAttr(element, "data-tenant-display-status");
  if (!status.present) {
    add(
      "acting",
      "UNKNOWN",
      `${who}: no data-tenant-display-status — runner build ${build} predates PR #2510's display attributes, so data-tenant-acting cannot be compared`,
    );
  } else {
    const actingAttr = readAttr(element, "data-tenant-acting");
    const wantActing = display?.source === "row_stamp" ? null : (display?.tenantId ?? null);
    if (wantActing === null) {
      if (actingAttr.present) {
        add(
          "acting",
          "FAIL",
          `${who}: data-tenant-acting='${actingAttr.value}' but must be ABSENT (display.source=${display?.source ?? "null"})`,
        );
      } else {
        add(
          "acting",
          "PASS",
          `${who}: data-tenant-acting absent (display.source=${display?.source ?? "null"})`,
        );
      }
    } else if (!actingAttr.present) {
      add("acting", "FAIL", `${who}: data-tenant-acting is absent, expected '${wantActing}'`);
    } else if (actingAttr.value === wantActing) {
      add("acting", "PASS", `${who}: data-tenant-acting='${wantActing}'`);
    } else {
      add(
        "acting",
        "FAIL",
        `${who}: data-tenant-acting='${actingAttr.value}', expected '${wantActing}'`,
      );
    }
  }
  return verdict();
}

/**
 * T2b's cardinality: the registered badge ids must be distinct, per-zone, and
 * exactly as many as the zones expecting a badge. Pure. The defect it pins
 * (measured on a two-zone page): one `terminal.tenant-badge` for two zones.
 *
 * A per-zone badge on a zone in `unboundZones` (its header carries no
 * `data-claude-session-id` yet) is NOT counted: whether that zone owes a badge
 * is UNKNOWN until it binds (or it is a stamped plain shell, plan D2), so it
 * is returned in `unboundBadges` for the caller to report as its own UNKNOWN
 * finding rather than turned into a count FAIL.
 *
 * @param {{registryIds:string[], zones:{zoneIndex:number, expect:string}[], unboundZones?:number[]}} input
 * @returns {{ok:boolean|null, detail:string, unboundBadges:string[]}} `ok: null` is UNKNOWN
 */
export function badgeCardinality({ registryIds, zones, unboundZones = [] }) {
  const ids = (registryIds ?? []).map(String);
  const unboundIds = new Set((unboundZones ?? []).map((z) => tenantBadgeElementId(z)));
  const unboundBadges = [...new Set(ids.filter((id) => unboundIds.has(id)))];
  const badgeIds = ids.filter(
    (id) =>
      !unboundIds.has(id) &&
      (id === LEGACY_TENANT_BADGE_ID || /^terminal-tenant-badge-\d+$/.test(id)),
  );
  const unknown = (zones ?? []).filter((z) => z.expect === "unknown");
  if (unknown.length) {
    return {
      ok: null,
      detail: `zone(s) ${unknown.map((z) => z.zoneIndex).join(", ")} have an UNKNOWN expectation, so how many badges are owed is UNKNOWN`,
      unboundBadges,
    };
  }
  const expecting = (zones ?? []).filter((z) => z.expect === "present");
  const problems = [];
  const dup = badgeIds.filter((id, i) => badgeIds.indexOf(id) !== i);
  if (dup.length) problems.push(`duplicated badge id(s): ${[...new Set(dup)].join(", ")}`);
  if (badgeIds.includes(LEGACY_TENANT_BADGE_ID)) {
    problems.push(`the shared pre-per-zone id '${LEGACY_TENANT_BADGE_ID}' is registered`);
  }
  const distinct = new Set(badgeIds);
  if (distinct.size !== expecting.length) {
    problems.push(
      `${distinct.size} distinct badge id(s) [${[...distinct].join(", ")}] for ${expecting.length} zone(s) expecting a badge [${expecting.map((z) => z.zoneIndex).join(", ")}]`,
    );
  }
  for (const z of expecting) {
    if (!distinct.has(tenantBadgeElementId(z.zoneIndex))) {
      problems.push(
        `zone ${z.zoneIndex} expects ${tenantBadgeElementId(z.zoneIndex)}, which is not registered`,
      );
    }
  }
  return problems.length
    ? { ok: false, detail: problems.join("; "), unboundBadges }
    : {
        ok: true,
        detail: `${distinct.size} distinct per-zone badge id(s) for ${expecting.length} zone(s)`,
        unboundBadges,
      };
}
