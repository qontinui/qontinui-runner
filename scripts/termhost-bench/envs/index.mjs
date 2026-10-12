/**
 * termhost-bench environments (plan D6): numbers compare only within one.
 *
 * Phase 3 built `linux-xvfb`; Phase 4 builds `windows-hosted` (GitHub's hosted
 * windows-latest, for the runner's A/B and the VS Code smoke). `windows-desktop`
 * arrives with Phase 5; until then `run` refuses it with that reason, while
 * `doctor` already reports what a Windows machine offers through the probe's own
 * doctor.
 *
 * Optional spec fields:
 *   doctorFacts(env)   extra doctor verdicts the environment itself decides, merged
 *                      into the doctor's mechanisms (`doctor.mjs`); a fact named
 *                      in `blockingMechanisms` stops a run that cannot start here
 *   setupLabels        row labels the environment puts on a setup's rows (D6: on
 *                      linux-xvfb, VS Code's terminal falls back from WebGL to the
 *                      DOM renderer, so its rows say so and stay out of any verdict)
 */

import { ENVIRONMENTS } from "../kpi-catalog.mjs";
import { SCREEN as XVFB_SCREEN, startLinuxXvfb } from "./linux-xvfb.mjs";
import { hostedRunnerVerdict, startWindowsHosted } from "./windows-hosted.mjs";

/**
 * @typedef {{id: string, platform: string, start: ((o: object) => Promise<object>) | null,
 *   notBuilt: string | null, setups: string[], softwareRendered: boolean, screen: object | null,
 *   doctorFacts?: (env: object) => Record<string, {state: string, reason: string}>,
 *   setupLabels?: Record<string, string[]>}} EnvironmentSpec
 */

/** @type {Record<string, EnvironmentSpec>} */
export const ENVIRONMENT_SPECS = Object.freeze({
  "linux-xvfb": Object.freeze({
    id: "linux-xvfb",
    platform: "linux",
    start: startLinuxXvfb,
    notBuilt: null,
    setups: Object.freeze(["runner", "tmux"]),
    softwareRendered: true,
    screen: XVFB_SCREEN,
    setupLabels: Object.freeze({ vscode: Object.freeze(["dom_renderer_fallback"]) }),
  }),
  "windows-hosted": Object.freeze({
    id: "windows-hosted",
    platform: "win32",
    start: startWindowsHosted,
    notBuilt: null,
    setups: Object.freeze(["runner", "vscode", "windows-terminal"]),
    softwareRendered: true,
    screen: null,
    doctorFacts: (env) => ({ "env.hosted_runner": hostedRunnerVerdict(env) }),
    setupLabels: Object.freeze({}),
  }),
  "windows-desktop": Object.freeze({
    id: "windows-desktop",
    platform: "win32",
    start: null,
    notBuilt: "the windows-desktop environment arrives with Phase 5 (Windows Terminal, tmux-wsl and the desktop run)",
    setups: Object.freeze(["runner", "vscode", "windows-terminal", "tmux-wsl"]),
    softwareRendered: false,
    screen: null,
    setupLabels: Object.freeze({}),
  }),
});

for (const id of ENVIRONMENTS) {
  if (!ENVIRONMENT_SPECS[id]) throw new Error(`environment ${id} from the catalog has no spec`);
}

/** The spec for `id`, or a RangeError naming the known ones. */
export function environmentSpec(id) {
  const spec = ENVIRONMENT_SPECS[id];
  if (!spec) throw new RangeError(`unknown environment "${id}" (one of ${Object.keys(ENVIRONMENT_SPECS).join(", ")})`);
  return spec;
}
