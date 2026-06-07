/**
 * Phase 2 — UnzonedChip label/visibility contract.
 *
 * vitest runs `environment: "node"` (no jsdom), so per repo precedent
 * (`CommitTrafficLight.test.tsx`, `useZoneLayout.restore.test.ts`) we test the
 * exported pure helper `unzonedChipLabel` rather than rendering JSX. The chip's
 * visual shell is verified by UI Bridge / manual tests once the spec updates.
 */

import { describe, it, expect } from "vitest";
import { unzonedChipLabel } from "./UnzonedChip";

describe("unzonedChipLabel — chip renders only when sessions are hidden", () => {
  it("returns null when nothing is unassigned (chip must not render)", () => {
    expect(unzonedChipLabel(0)).toBeNull();
  });

  it("returns null for negative counts (defensive)", () => {
    expect(unzonedChipLabel(-1)).toBeNull();
  });

  it("singular phrasing for exactly one hidden session", () => {
    const label = unzonedChipLabel(1);
    expect(label).not.toBeNull();
    expect(label!.text).toBe("1 more");
    expect(label!.title).toContain("1 session is not visible");
  });

  it("plural phrasing for multiple hidden sessions", () => {
    const label = unzonedChipLabel(4);
    expect(label).not.toBeNull();
    expect(label!.text).toBe("4 more");
    expect(label!.title).toContain("4 sessions are not visible");
  });
});
