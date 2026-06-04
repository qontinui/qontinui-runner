/**
 * Regression tests for the registry-driven restore zone binding.
 *
 * The bug: restore used to rebuild the zone↔session binding by CREATION ORDER.
 * Plain shells created first grabbed the low zones and real Claude sessions
 * spilled to `zoneIndex:-1` (unassigned/hidden). The fix makes a durable
 * backend registry the source of truth: each Claude session record is bound to
 * its RECORDED zone via `assignTabToZone` (which RESERVES the zone), and the
 * creation-order auto-fill (`reconcileAssignments`) must NOT steal a reserved
 * zone.
 *
 * vitest runs in a `node` environment (no React Testing Library — see the
 * sibling `useTerminalManager.test.ts`), so we exercise the pure reducer
 * `reconcileAssignments` directly. That is the exact function behind the
 * `useZoneLayout` auto-assign effect.
 */

import { describe, it, expect } from "vitest";
import { reconcileAssignments } from "./useZoneLayout";

describe("reconcileAssignments — registry zones win over creation-order shells", () => {
  it("binds records {0: claudeA, 3: claudeB} to zones 0 and 3 even though shells were created first", () => {
    // Restore order: two plain shells reconnect FIRST, then records bind their
    // zones. We model the record binding as the reserved set + pre-seeded
    // assignments (what `assignTabToZone` produces), then auto-fill the shells.
    const reserved = new Set<number>([0, 3]);
    // Records already assigned to their recorded zones:
    const afterRecordBind = { 0: "claudeA", 3: "claudeB" };
    // Now the two plain shells (created first, ids sort earlier) are the
    // unassigned tabs. The full live tab list:
    const tabIds = ["shell-1", "shell-2", "claudeA", "claudeB"];

    const next = reconcileAssignments(afterRecordBind, tabIds, 6, reserved);

    // Claude sessions keep their recorded zones …
    expect(next[0]).toBe("claudeA");
    expect(next[3]).toBe("claudeB");
    // … and the shells land in OTHER zones — never 0 or 3.
    expect(next[0]).not.toBe("shell-1");
    expect(next[0]).not.toBe("shell-2");
    expect(next[3]).not.toBe("shell-1");
    expect(next[3]).not.toBe("shell-2");
    const shellZones = Object.entries(next)
      .filter(([, id]) => id === "shell-1" || id === "shell-2")
      .map(([z]) => Number(z));
    expect(shellZones).not.toContain(0);
    expect(shellZones).not.toContain(3);
    expect(shellZones).toHaveLength(2);
  });

  it("WITHOUT reservation a shell created first would steal zone 0 (documents the original bug)", () => {
    // No reserved set: classic creation-order fill. shell-1 grabs zone 0.
    const next = reconcileAssignments({}, ["shell-1", "claudeA"], 6, new Set());
    expect(next[0]).toBe("shell-1");
  });

  it("auto-fills a reserved zone once it is genuinely occupied (reservation cleared)", () => {
    // After the record's tab lands, the zone is occupied and the reservation
    // would have been dropped — a later vacancy auto-fills normally.
    const next = reconcileAssignments({ 0: "claudeA" }, ["claudeA", "shell-1"], 6, new Set());
    expect(next[0]).toBe("claudeA");
    expect(next[1]).toBe("shell-1");
  });
});
