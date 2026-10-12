/**
 * The `windows-hosted` environment (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * D6 and Phase 4): GitHub's hosted `windows-latest` — free for this public
 * repository, software-rendered (no GPU), a desktop nobody uses. Its numbers are
 * plumbing and a Windows regression signal for the runner, never compared with
 * another environment's (D6).
 *
 * Unlike `linux-xvfb` there is no display server to start: the VM's own
 * interactive desktop is the display. The environment measures the primary
 * screen (the adapter's current mode and the applied DPI) so the results file's
 * `machine.display` carries it, and declares a doctor fact: the run refuses to
 * start anywhere but on a GitHub-hosted runner. A workstation is
 * `windows-desktop` (Phase 5), with its own safety rules — a run labelled
 * `windows-hosted` on an operator's desktop would both mislabel its numbers and
 * drive windows and keystrokes on a machine someone is using.
 */

import { spawnSync } from "node:child_process";

export const ID = "windows-hosted";

/**
 * Read-only query of the primary display mode and the applied DPI:
 * `Win32_VideoController` gives the adapter's current physical resolution (no
 * DPI virtualisation, unlike a WinForms `Screen` read from a DPI-unaware host),
 * `AppliedDPI` the scale (96 = 100 %).
 */
export const SCREEN_QUERY =
  "$ErrorActionPreference='Stop'; " +
  "$v = Get-CimInstance -ClassName Win32_VideoController | Where-Object { $_.CurrentHorizontalResolution -gt 0 } | Select-Object -First 1; " +
  "$d = (Get-ItemProperty -Path 'HKCU:\\Control Panel\\Desktop\\WindowMetrics' -Name AppliedDPI -ErrorAction SilentlyContinue).AppliedDPI; " +
  "[pscustomobject]@{ w = [int]$v.CurrentHorizontalResolution; h = [int]$v.CurrentVerticalResolution; dpi = $(if ($d) { [int]$d } else { 0 }) } | ConvertTo-Json -Compress";

/**
 * The doctor's verdict on where a windows-hosted run may start.
 *
 * @param {object} env the process environment
 * @returns {{state: string, reason: string}}
 */
export function hostedRunnerVerdict(env = process.env) {
  const hosted = env.GITHUB_ACTIONS === "true" && env.RUNNER_ENVIRONMENT === "github-hosted";
  if (hosted && (env.RUNNER_OS ?? "Windows") === "Windows") {
    return { state: "OPERATIVE", reason: `a GitHub-hosted Windows runner (RUNNER_ENVIRONMENT=github-hosted${env.ImageOS ? `, ImageOS=${env.ImageOS}` : ""})` };
  }
  return {
    state: "INOPERATIVE-ON-THIS-MACHINE",
    reason:
      "windows-hosted is GitHub's hosted windows-latest (GITHUB_ACTIONS=true, RUNNER_ENVIRONMENT=github-hosted) and this machine is not one; " +
      "a workstation run is windows-desktop (Phase 5), never a hosted run on a desktop someone is using",
  };
}

/**
 * Parse the screen query's output.
 *
 * @returns {{width: number, height: number, scalePct: number} | null}
 */
export function parseScreen(stdout) {
  try {
    const r = JSON.parse(String(stdout).trim());
    if (!Number.isInteger(r?.w) || !Number.isInteger(r?.h) || r.w <= 0 || r.h <= 0) return null;
    const scalePct = Number.isInteger(r.dpi) && r.dpi > 0 ? Math.round((r.dpi / 96) * 100) : 100;
    return { width: r.w, height: r.h, scalePct };
  } catch {
    return null;
  }
}

/** The primary screen, or null when it cannot be read. */
export function readScreen({ run = spawnSync } = {}) {
  const res = run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", SCREEN_QUERY], { encoding: "utf8", windowsHide: true, timeout: 30000 });
  if (res.error || res.status !== 0) return null;
  return parseScreen(res.stdout);
}

/**
 * Start the environment: nothing to launch, the screen to measure.
 *
 * @param {{log?: (s: string) => void, platform?: string, run?: typeof spawnSync}} [o]
 */
export async function startWindowsHosted({ log = () => {}, platform = process.platform, run = spawnSync } = {}) {
  if (platform !== "win32") throw new Error(`the ${ID} environment runs on Windows only (this is ${platform})`);
  const screen = readScreen({ run });
  log(`env: ${ID}, primary screen ${screen ? `${screen.width}x${screen.height} at ${screen.scalePct}%` : "unreadable"}`);
  return {
    id: ID,
    platform: "win32",
    display: screen ? `${screen.width}x${screen.height}` : null,
    screen,
    env: {},
    describe() {
      return { screen: screen ? `${screen.width}x${screen.height}@${screen.scalePct}%` : null };
    },
    async stop() {},
  };
}
