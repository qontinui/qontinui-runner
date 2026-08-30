/**
 * Addressability contract for the session-info dropdown trigger.
 *
 * `environment: "node"` (no jsdom, no `@testing-library/react`), so — following
 * `SessionManagerToggle.test.ts` / `UnzonedChip.test.ts` — the exported id
 * helper is asserted directly and the JSX glue is asserted by reading the
 * source.
 *
 * What this pins is the fix for the MULTI-zone `origin: "auto"` regression
 * (2026-08-20 manual-test loop, item B): the trigger is a `<button>`, so
 * `useAutoRegister`'s DOM walker matches it, and because the registry is
 * last-write-wins per id, whichever of the walker and the component's
 * `useUIElement` effect ran last decided the entry. In multi-zone layouts the
 * walker won, re-registering the trigger with `origin: "auto"` and the
 * DOM-derived accessible label — losing the `Session info (zone N)` label. The
 * `data-no-register` opt-out (App.tsx wires `excludeSelectors:
 * ["[data-no-register]"]`) removes the walker from the race entirely, and the
 * explicit `data-ui-bridge-id` stamp pins the same id the hook registers under
 * so no other scanner can mint a competing entry for the node.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

import { sessionInfoElementId } from "./useSessionInfo";

const SOURCE = readFileSync(
  fileURLToPath(new URL("./SessionInfoDropdown.tsx", import.meta.url)),
  "utf8",
);

const APP_SOURCE = readFileSync(fileURLToPath(new URL("../../App.tsx", import.meta.url)), "utf8");

describe("SessionInfoDropdown addressability", () => {
  it("gives every zone its own trigger + panel id", () => {
    expect(sessionInfoElementId("trigger", 0)).toBe("terminal-session-info-trigger-0");
    expect(sessionInfoElementId("trigger", 1)).toBe("terminal-session-info-trigger-1");
    expect(sessionInfoElementId("trigger", 0)).not.toBe(sessionInfoElementId("trigger", 1));
  });

  it("opts the trigger out of the auto-register DOM walker", () => {
    expect(SOURCE).toContain('data-no-register="true"');
  });

  it("stamps the hook's own id on the trigger and the panel", () => {
    expect(SOURCE).toContain('data-ui-bridge-id={sessionInfoElementId("trigger", zoneIndex)}');
    expect(SOURCE).toContain('data-ui-bridge-id={sessionInfoElementId("panel", zoneIndex)}');
  });

  /**
   * `data-ui-bridge-id` alone was NOT enough, and the gap was observable: a
   * live trigger surfaced as `button-<slug>-<n>` with `registered: false`,
   * where `n` shuffled between snapshots, forcing callers to match on
   * accessible name instead of id.
   *
   * Two facts combine. (1) `useUIElement`'s unmount cleanup unregisters by id
   * unconditionally, and these ids are keyed on the ZONE SLOT rather than the
   * session — so a session moving between zones can delete the registry entry
   * of the session that inherited its slot, leaving a live node unregistered
   * that `data-no-register` then prevents anything from re-registering.
   * (2) ui-bridge's core discovery namer (`getElementId`) resolves an
   * unregistered node as `data-testid` → HTML `id` → slugified accessible name
   * + a first-free-integer collision counter. It NEVER reads
   * `data-ui-bridge-id`, so the stamp above could not pin the fallback name.
   *
   * `data-testid` is that namer's FIRST choice, so with it set the discovered
   * id is byte-identical to the registered one and cannot shuffle.
   */
  it("pins the discovery fallback id with data-testid, not just data-ui-bridge-id", () => {
    expect(SOURCE).toContain('data-testid={sessionInfoElementId("trigger", zoneIndex)}');
    expect(SOURCE).toContain('data-testid={sessionInfoElementId("panel", zoneIndex)}');
    expect(SOURCE).toContain("data-testid={sessionInfoElementId(row.field, zoneIndex)}");
  });

  it("uses the SAME id for the registration and the discovery fallback", () => {
    // The point is not that both attributes exist but that they agree — a
    // trigger addressed as `terminal-session-info-trigger-1` must resolve to
    // the same node whether or not its registry entry survived.
    for (const field of ["trigger", "panel"] as const) {
      const expr = `sessionInfoElementId("${field}", zoneIndex)`;
      expect(SOURCE).toContain(`data-ui-bridge-id={${expr}}`);
      expect(SOURCE).toContain(`data-testid={${expr}}`);
    }
  });

  /**
   * Regression guard on the SDK contract this fix depends on. If ui-bridge
   * ever stops preferring `data-testid` in `getElementId`, the stamps above go
   * back to being inert and the shuffling positional id returns — silently.
   */
  it("depends on ui-bridge discovery preferring data-testid", () => {
    const sdk = readFileSync(
      fileURLToPath(
        new URL("../../../node_modules/@qontinui/ui-bridge/dist/index.mjs", import.meta.url),
      ),
      "utf8",
    );
    const getElementId = sdk.slice(sdk.indexOf("getElementId(element) {"));
    expect(getElementId).not.toBe("");
    // `data-testid` must be consulted BEFORE the slug + collision-counter
    // fallback that produced the shuffling `button-<slug>-<n>` ids.
    const testIdAt = getElementId.indexOf('getAttribute("data-testid")');
    const counterAt = getElementId.indexOf("discoveryCache");
    expect(testIdAt).toBeGreaterThanOrEqual(0);
    expect(counterAt).toBeGreaterThan(testIdAt);
  });

  it("keeps the `data-no-register` opt-out wired in App.tsx", () => {
    // The opt-out above is inert unless the provider still excludes it.
    expect(APP_SOURCE).toContain('excludeSelectors={["[data-no-register]"]}');
  });
});
