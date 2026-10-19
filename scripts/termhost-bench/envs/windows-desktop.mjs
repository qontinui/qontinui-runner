/**
 * The `windows-desktop` environment (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, D6 and Phase 5): a
 * fleet workstation's interactive, unlocked desktop with a real GPU — the
 * environment that answers the operator's question (runner, VS Code, Windows
 * Terminal and tmux-wsl at N = 1, 5, 15, 40, both arms). Never a GitHub-hosted
 * VM: that is `windows-hosted`, whose numbers are software-rendered plumbing.
 *
 * Like `windows-hosted` there is no display server to start; the environment
 * measures the primary screen read-only. Its doctor facts (`doctor.mjs`) block a
 * run unless the probe finds the session interactive and per-window capture
 * operative: `doctor --env windows-desktop` is how an agent learns whether the
 * desktop run can happen on this machine at all (Phase 5's deferral trigger).
 * The input arm additionally needs `input.safe` (D9); without it that arm reads
 * unknown(input_unsafe) and nothing is typed.
 */

import { spawnSync } from "node:child_process";

import { readScreen } from "./windows-hosted.mjs";

export const ID = "windows-desktop";

/**
 * The doctor's verdict on where a windows-desktop run may start: anywhere but a
 * GitHub-hosted runner.
 *
 * @param {object} env the process environment
 * @returns {{state: string, reason: string}}
 */
export function desktopVerdict(env = process.env) {
  if (env.GITHUB_ACTIONS === "true" && env.RUNNER_ENVIRONMENT === "github-hosted") {
    return {
      state: "INOPERATIVE-ON-THIS-MACHINE",
      reason: "this is a GitHub-hosted runner, which is the windows-hosted environment (software-rendered, no GPU); windows-desktop is a workstation desktop",
    };
  }
  return { state: "OPERATIVE", reason: "not a GitHub-hosted runner; the probe's session.interactive and capture.wgc decide whether this desktop can be measured" };
}

/**
 * Start the environment: nothing to launch, the screen to measure.
 *
 * @param {{log?: (s: string) => void, platform?: string, run?: typeof spawnSync}} [o]
 */
export async function startWindowsDesktop({ log = () => {}, platform = process.platform, run = spawnSync } = {}) {
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
