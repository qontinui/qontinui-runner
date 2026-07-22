/**
 * Structural + addressability invariants for `SessionManagerPanel`.
 *
 * Two things this file pins, neither of which any other test covers:
 *
 *  1. **The Live/Previous arm invariant.** The panel body is
 *     `{view === "previous" ? <PastSessionsView/> : <>…live panels…</>}`. Every
 *     panel that belongs to the Live view must sit INSIDE the `<>…</>` arm.
 *     `main` keeps adding panels here, so this file conflicts on almost every
 *     rebase — and both ways of resolving it wrong (hoisting a panel above the
 *     conditional ⇒ it renders in Previous too; dropping it into the `previous`
 *     arm ⇒ it vanishes from Live) typecheck and pass CI. This test is the
 *     cheap guard, done by reading the source since the runner's vitest config
 *     is `environment: "node"` (no jsdom, no `@testing-library/react` — see
 *     `StewardControl.test.tsx`).
 *
 *  2. **The view toggle's stable control ids.** Without them the Previous
 *     surface is unreachable through the UI Bridge: the panel always mounts on
 *     "live", and the SDK's auto-derived ids for the two bare buttons slugify
 *     from their visible text.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

const SOURCE = readFileSync(
  fileURLToPath(new URL("./SessionManagerPanel.tsx", import.meta.url)),
  "utf8",
);

/** Byte offsets of the `view === "previous" ? … : <>…</>` conditional. */
function armBounds() {
  const conditionStart = SOURCE.indexOf('{view === "previous" ? (');
  expect(conditionStart, 'the `view === "previous"` conditional').toBeGreaterThan(-1);

  const elseArm = SOURCE.indexOf(") : (", conditionStart);
  expect(elseArm, "the conditional's else arm").toBeGreaterThan(-1);

  const liveArmStart = SOURCE.indexOf("<>", elseArm);
  expect(liveArmStart, "the Live arm's fragment open").toBeGreaterThan(-1);

  const liveArmEnd = SOURCE.indexOf("</>", liveArmStart);
  expect(liveArmEnd, "the Live arm's fragment close").toBeGreaterThan(-1);

  return { conditionStart, previousArmEnd: elseArm, liveArmStart, liveArmEnd };
}

describe("SessionManagerPanel view arms", () => {
  it("renders PastSessionsView only in the `previous` arm", () => {
    const { conditionStart, previousArmEnd } = armBounds();
    const occurrences = [...SOURCE.matchAll(/<PastSessionsView\b/g)].map((m) => m.index ?? -1);

    expect(occurrences).toHaveLength(1);
    expect(occurrences[0]).toBeGreaterThan(conditionStart);
    expect(occurrences[0]).toBeLessThan(previousArmEnd);
  });

  // Every panel that belongs to the Live view. Adding a panel to this list is
  // the intended way to protect it from the rebase hazard described above.
  it.each(["<StewardControl", "<SessionManagerHeader", "<SessionCard", "<WorktreesPanel"])(
    "keeps %s inside the Live arm",
    (marker) => {
      const { liveArmStart, liveArmEnd } = armBounds();
      const at = SOURCE.indexOf(marker);

      expect(at, `${marker} is rendered`).toBeGreaterThan(-1);
      expect(at, `${marker} must not render above the view conditional`).toBeGreaterThan(
        liveArmStart,
      );
      expect(at, `${marker} must not fall outside the Live fragment`).toBeLessThan(liveArmEnd);
    },
  );

  it("keeps the Live|Previous toggle ABOVE the conditional so both views show it", () => {
    const { conditionStart } = armBounds();
    for (const id of [
      "terminal.session-manager-view-live",
      "terminal.session-manager-view-previous",
    ]) {
      const at = SOURCE.indexOf(id);
      expect(at, `${id} is stamped`).toBeGreaterThan(-1);
      expect(at, `${id} must render outside the view conditional`).toBeLessThan(conditionStart);
    }
  });
});

describe("SessionManagerPanel view toggle addressability", () => {
  it("stamps both toggle buttons with a namespaced control id", () => {
    expect(SOURCE).toContain('data-ui-bridge-id="terminal.session-manager-view-live"');
    expect(SOURCE).toContain('data-ui-bridge-id="terminal.session-manager-view-previous"');
  });

  it("gives each toggle a title naming its view (what ai/find resolves on)", () => {
    const titles = [...SOURCE.matchAll(/title="([^"]*)"/g)].map((m) => m[1]);
    expect(titles.some((t) => /\bLive\b/.test(t))).toBe(true);
    expect(titles.some((t) => /\bPrevious\b/.test(t))).toBe(true);
  });
});
