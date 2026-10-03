/**
 * Loud-failure helpers for source-reading ("wiring") tests.
 *
 * A wiring test that slices source text between two markers is only as strong
 * as its markers. `String.prototype.indexOf` answers `-1` for an absent marker
 * and `slice(start, -1)` then quietly runs to the end of the file, so the
 * guard degrades into a whole-file presence check and stays green. That was
 * qontinui-runner#1831's second vacuous guard. These helpers make the absence a
 * thrown, separately-worded error instead, in the same spirit as
 * `emptyReadArgs()` in `FleetSessionPicker.wiring.test.ts`.
 */

import { fileURLToPath } from "node:url";

/**
 * Index of `marker` in `src` at or after `from`. Throws, naming the marker and
 * the search start, when it is absent — never returns -1.
 *
 * `from` lets an END marker be required to occur AFTER its start marker, which
 * also rules out the second quiet failure: an end marker that matches earlier
 * in the file than the start, whose `slice` is the empty string.
 */
export function assertAnchored(src: string, marker: string, from = 0): number {
  const at = src.indexOf(marker, from);
  if (at < 0) {
    throw new Error(
      `assertAnchored: marker not found${from > 0 ? ` at or after offset ${from}` : ""}: ` +
        JSON.stringify(marker) +
        ` — the source was reworded or reformatted; a slice guard cannot speak for code it can no longer locate`,
    );
  }
  return at;
}

/**
 * Directory a wiring test reads its subject sources from.
 *
 * Normally the test's own directory. The mutation probe
 * (`qontinui-claude-config/scripts/mutation-probe.py`) stages a mutated copy of
 * that directory and exports it as `MP_STAGE`; honouring it is what lets the
 * UNCHANGED test file be rerun against the mutant.
 */
export function subjectDir(testFileUrl: string): string {
  const staged = process.env.MP_STAGE;
  if (staged) return staged.endsWith("/") ? staged : `${staged}/`;
  return fileURLToPath(new URL(".", testFileUrl));
}
