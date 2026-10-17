/**
 * The `linux-xvfb` environment (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * D6 and Phase 3): a private Xvfb display at 1920x1080x24 on a free display
 * number, passed to every launch — including a supervisor spawn's
 * `extra_env.DISPLAY`. llvmpipe renders on the CPU the load also uses, so a
 * `linux-xvfb` number is regression context for the runner only and is never
 * compared with a Windows one (D6).
 *
 * D2 asks for every measured window to be visible, unoccluded, foreground and
 * maximised. A bare X server has no window manager: nothing maximises a window,
 * and the input focus stays at PointerRoot, which the probe (rightly) does not
 * count as "foreground". So when `matchbox-window-manager` is installed the
 * environment runs it — it shows exactly one application window at a time,
 * full-screen and focused, which is D2's shape — and the harness additionally
 * activates the measured window itself before each measurement. Without it the
 * environment still runs and the doctor reports `env.window_manager` DEGRADED.
 *
 * Both processes are this run's own children and are ended by their handles.
 */

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { delimiter, join } from "node:path";

export const ID = "linux-xvfb";

/** The plan's screen: 1920x1080x24 at 100 % scale. */
export const SCREEN = Object.freeze({ width: 1920, height: 1080, depth: 24, scalePct: 100 });

/** The window manager the environment prefers, and its argv. */
export const WINDOW_MANAGER = Object.freeze({
  name: "matchbox-window-manager",
  args: Object.freeze(["-use_titlebar", "no", "-use_cursor", "no"]),
});

/**
 * An executable on PATH, or null.
 *
 * @param {string} name
 * @param {{env?: object, exists?: (p: string) => boolean, platform?: string}} [o]
 */
export function findOnPath(name, { env = process.env, exists = existsSync, platform = process.platform } = {}) {
  const exts = platform === "win32" ? (env.PATHEXT ?? ".EXE;.CMD;.BAT").split(";") : [""];
  for (const dir of String(env.PATH ?? "").split(delimiter)) {
    if (!dir) continue;
    for (const ext of exts) {
      const p = join(dir, `${name}${ext}`);
      if (exists(p)) return p;
    }
  }
  return null;
}

/** The first display number with neither a socket nor a lock file. */
export function freeDisplay({ exists = existsSync, from = 90, to = 190 } = {}) {
  for (let n = from; n < to; n += 1) {
    if (!exists(`/tmp/.X11-unix/X${n}`) && !exists(`/tmp/.X${n}-lock`)) return n;
  }
  throw new Error(`no free X display number in ${from}..${to - 1}`);
}

/**
 * Xvfb's argv. `-noreset` keeps the server (and its window manager) alive when
 * the last client of one unit disconnects before the next unit connects.
 */
export function xvfbArgv(display, screen = SCREEN) {
  return [`:${display}`, "-screen", "0", `${screen.width}x${screen.height}x${screen.depth}`, "-nolisten", "tcp", "-noreset"];
}

/**
 * Start the environment.
 *
 * @param {{log?: (s: string) => void, spawnFn?: typeof spawn, env?: object, platform?: string,
 *   which?: (name: string) => string | null, exists?: (p: string) => boolean,
 *   sleepFn?: (ms: number) => Promise<void>, screen?: object}} [o]
 */
export async function startLinuxXvfb({
  log = () => {},
  spawnFn = spawn,
  env = process.env,
  platform = process.platform,
  which = (name) => findOnPath(name, { env }),
  exists = existsSync,
  sleepFn = (ms) => new Promise((r) => setTimeout(r, ms)),
  screen = SCREEN,
} = {}) {
  if (platform !== "linux") throw new Error(`the ${ID} environment runs on Linux only (this is ${platform})`);
  const xvfb = which("Xvfb");
  if (!xvfb) throw new Error("Xvfb is not on PATH (apt-get install xvfb)");
  const n = freeDisplay({ exists });
  const display = `:${n}`;
  const xvfbChild = spawnFn(xvfb, xvfbArgv(n, screen), { stdio: ["ignore", "ignore", "pipe"] });
  let xvfbErr = "";
  xvfbChild.stderr?.on("data", (d) => {
    xvfbErr = (xvfbErr + d.toString("utf8")).slice(-4096);
  });
  let xvfbExit = null;
  xvfbChild.on("exit", (code, signal) => {
    xvfbExit = { code, signal };
  });
  const deadline = Date.now() + 15000;
  while (!exists(`/tmp/.X11-unix/X${n}`)) {
    if (xvfbExit) throw new Error(`Xvfb ${display} exited (${JSON.stringify(xvfbExit)}): ${xvfbErr.slice(-400)}`);
    if (Date.now() > deadline) {
      xvfbChild.kill("SIGTERM");
      throw new Error(`Xvfb ${display} did not create its socket within 15 s`);
    }
    await sleepFn(100);
  }
  log(`env: Xvfb ${display} (pid ${xvfbChild.pid}) ${screen.width}x${screen.height}x${screen.depth}`);

  let wmChild = null;
  let wmNote = null;
  const wm = which(WINDOW_MANAGER.name);
  if (wm) {
    wmChild = spawnFn(wm, [...WINDOW_MANAGER.args], { env: { ...env, DISPLAY: display }, stdio: "ignore" });
    await sleepFn(500);
    if (wmChild.exitCode !== null && wmChild.exitCode !== undefined) {
      wmNote = `${WINDOW_MANAGER.name} exited at once (code ${wmChild.exitCode})`;
      wmChild = null;
    } else {
      log(`env: ${WINDOW_MANAGER.name} (pid ${wmChild.pid}) on ${display}`);
    }
  } else {
    wmNote = `${WINDOW_MANAGER.name} is not on PATH: windows are neither maximised nor focused by policy`;
  }

  const stopChild = async (child) => {
    if (!child || child.exitCode !== null) return;
    const exited = new Promise((r) => child.once("exit", r));
    child.kill("SIGTERM");
    await Promise.race([exited, sleepFn(5000)]);
    if (child.exitCode === null) {
      child.kill("SIGKILL");
      await Promise.race([exited, sleepFn(2000)]);
    }
  };

  return {
    id: ID,
    platform: "linux",
    display,
    screen,
    env: { DISPLAY: display },
    windowManager: wmChild ? WINDOW_MANAGER.name : null,
    windowManagerNote: wmNote,
    pids: { xvfb: xvfbChild.pid, windowManager: wmChild?.pid ?? null },
    describe() {
      return {
        display,
        screen: `${screen.width}x${screen.height}x${screen.depth}`,
        xvfb_pid: xvfbChild.pid,
        window_manager: wmChild ? WINDOW_MANAGER.name : null,
        window_manager_note: wmNote,
      };
    },
    async stop() {
      await stopChild(wmChild);
      await stopChild(xvfbChild);
    },
  };
}
