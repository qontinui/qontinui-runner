/**
 * termhost-bench environments (plan D6): numbers compare only within one.
 *
 * Phase 3 built `linux-xvfb`; Phase 4 `windows-hosted` (GitHub's hosted
 * windows-latest, for the runner's A/B and the VS Code smoke); Phase 5
 * `windows-desktop` (a workstation's interactive desktop with a GPU — the
 * operator's comparison). A spec's `notBuilt` names why `run` refuses an
 * environment that is not built; every environment is built today.
 *
 * Optional spec fields:
 *   doctorFacts(env)   extra doctor verdicts the environment itself decides, merged
 *                      into the doctor's mechanisms (`doctor.mjs`); a fact named
 *                      in `blockingMechanisms` stops a run that cannot start here
 *   crosscheckProfile  (required) the capture cross-check's profile the probe is
 *                      told for every watch and every row records (D2):
 *                      `software-compositor` on windows-hosted, `strict` elsewhere
 *   setupLabels        row labels the environment puts on a setup's rows (D6: on
 *                      linux-xvfb, VS Code's terminal falls back from WebGL to the
 *                      DOM renderer, so its rows say so and stay out of any verdict)
 */

import { CROSSCHECK_PROFILES, ENVIRONMENTS } from "../kpi-catalog.mjs";
import { SCREEN as XVFB_SCREEN, startLinuxXvfb } from "./linux-xvfb.mjs";
import { desktopVerdict, startWindowsDesktop } from "./windows-desktop.mjs";
import { hostedRunnerVerdict, startWindowsHosted } from "./windows-hosted.mjs";

/**
 * @typedef {{id: string, platform: string, start: ((o: object) => Promise<object>) | null,
 *   notBuilt: string | null, setups: string[], softwareRendered: boolean, screen: object | null,
 *   crosscheckProfile: string,
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
    // No secondary capture source on X11, so no cross-check to relax.
    crosscheckProfile: "strict",
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
    // D6: software rendering, no GPU — DDA coalesces 22-63% of its frames there
    // (CI run 38362046020), so the coalescing fallback is off. Its numbers are a
    // same-VM regression signal, never compared across environments.
    crosscheckProfile: "software-compositor",
    doctorFacts: (env) => ({ "env.hosted_runner": hostedRunnerVerdict(env) }),
    setupLabels: Object.freeze({}),
  }),
  "windows-desktop": Object.freeze({
    id: "windows-desktop",
    platform: "win32",
    start: startWindowsDesktop,
    notBuilt: null,
    setups: Object.freeze(["runner", "vscode", "windows-terminal", "tmux-wsl"]),
    softwareRendered: false,
    screen: null,
    crosscheckProfile: "strict",
    doctorFacts: (env) => ({ "env.desktop": desktopVerdict(env) }),
    setupLabels: Object.freeze({}),
  }),
});

for (const id of ENVIRONMENTS) {
  if (!ENVIRONMENT_SPECS[id]) throw new Error(`environment ${id} from the catalog has no spec`);
  if (!CROSSCHECK_PROFILES.includes(ENVIRONMENT_SPECS[id].crosscheckProfile)) throw new Error(`environment ${id} declares no valid crosscheckProfile`);
}

/** The spec for `id`, or a RangeError naming the known ones. */
export function environmentSpec(id) {
  const spec = ENVIRONMENT_SPECS[id];
  if (!spec) throw new RangeError(`unknown environment "${id}" (one of ${Object.keys(ENVIRONMENT_SPECS).join(", ")})`);
  return spec;
}
