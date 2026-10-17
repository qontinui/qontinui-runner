/**
 * termhost-bench environments (plan D6): numbers compare only within one.
 *
 * Phase 3 builds `linux-xvfb`. `windows-hosted` arrives with Phase 4 (the VS Code
 * adapter and the windows-latest job) and `windows-desktop` with Phase 5; until
 * then `run` refuses them with that reason, while `doctor` already reports what a
 * Windows machine offers through the probe's own doctor.
 */

import { ENVIRONMENTS } from "../kpi-catalog.mjs";
import { SCREEN as XVFB_SCREEN, startLinuxXvfb } from "./linux-xvfb.mjs";

/**
 * @typedef {{id: string, platform: string, start: ((o: object) => Promise<object>) | null,
 *   notBuilt: string | null, setups: string[], softwareRendered: boolean, screen: object | null}} EnvironmentSpec
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
  }),
  "windows-hosted": Object.freeze({
    id: "windows-hosted",
    platform: "win32",
    start: null,
    notBuilt: "the windows-hosted environment arrives with Phase 4 (the VS Code adapter and the windows-latest job)",
    setups: Object.freeze(["runner", "vscode", "windows-terminal"]),
    softwareRendered: true,
    screen: null,
  }),
  "windows-desktop": Object.freeze({
    id: "windows-desktop",
    platform: "win32",
    start: null,
    notBuilt: "the windows-desktop environment arrives with Phase 5 (Windows Terminal, tmux-wsl and the desktop run)",
    setups: Object.freeze(["runner", "vscode", "windows-terminal", "tmux-wsl"]),
    softwareRendered: false,
    screen: null,
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
