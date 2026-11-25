/**
 * Phase 2 — UnzonedChip label/visibility contract.
 *
 * vitest runs `environment: "node"` (no jsdom), so per repo precedent
 * (`CommitTrafficLight.test.tsx`, `useZoneLayout.restore.test.ts`) we test the
 * exported pure helper `unzonedChipLabel` rather than rendering JSX. The chip's
 * visual shell is verified by UI Bridge / manual tests once the spec updates.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
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

  it("renders nothing in tmux work mode, where off screen is the normal state", () => {
    // Every session but the shown one is off screen in tmux mode; the session
    // manager reaches them. A "2 more" anomaly chip there was the live check's
    // finding.
    expect(unzonedChipLabel(2, "tmux")).toBeNull();
    expect(unzonedChipLabel(2, "grid")!.text).toBe("2 more");
  });

  it("the page hands the chip its work mode", () => {
    const page = readFileSync(resolve(__dirname, "./TerminalPage.tsx"), "utf8");
    expect(page).toMatch(
      /<UnzonedChip\s+unassignedCount=\{zoneLayout\.unassignedTabIds\.length\}\s+workMode=\{zoneLayout\.workMode\}/,
    );
  });
});
