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

  it("keeps the `data-no-register` opt-out wired in App.tsx", () => {
    // The opt-out above is inert unless the provider still excludes it.
    expect(APP_SOURCE).toContain('excludeSelectors={["[data-no-register]"]}');
  });
});
