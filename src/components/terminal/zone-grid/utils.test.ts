/**
 * Tests for the zone-cell mount predicate behind the session-info strip.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom, no
 * `@testing-library/react` — see `LaunchMenu.test.tsx` for the precedent), so
 * the mount DECISION is a pure function and is asserted here; the JSX shell it
 * gates is verified through the UI Bridge.
 *
 * D1 of `plans/2026-08-19-session-info-dropdown-mount-gaps-remediation.md`:
 * with exactly one terminal open, `showLabels` is false (it tracks
 * `layout.zones.length > 1`), so no `ZoneLabel` — and therefore no
 * `SessionInfoDropdown` — mounted at all. The regression this file guards is
 * "the single-zone layout has no session-info mount site".
 */

import { describe, it, expect } from "vitest";
import { showSoloSessionInfo } from "./utils";

const SESSION = "8553fb2f-dee1-49e2-9438-c29932317500";

describe("showSoloSessionInfo", () => {
  it("mounts on a single-zone layout, where ZoneLabel does not (D1)", () => {
    expect(
      showSoloSessionInfo({
        showLabels: false,
        showCompactCard: false,
        claudeSessionId: SESSION,
      }),
    ).toBe(true);
  });

  it("does NOT double-mount on a multi-zone layout — ZoneLabel already hosts it", () => {
    expect(
      showSoloSessionInfo({ showLabels: true, showCompactCard: false, claudeSessionId: SESSION }),
    ).toBe(false);
  });

  it("stays out of the compact card, which owns the whole zone body", () => {
    expect(
      showSoloSessionInfo({ showLabels: false, showCompactCard: true, claudeSessionId: SESSION }),
    ).toBe(false);
  });

  it("renders nothing for a tab with no Claude session — no session to describe", () => {
    expect(showSoloSessionInfo({ showLabels: false, showCompactCard: false })).toBe(false);
    expect(
      showSoloSessionInfo({ showLabels: false, showCompactCard: false, claudeSessionId: "" }),
    ).toBe(false);
  });
});
