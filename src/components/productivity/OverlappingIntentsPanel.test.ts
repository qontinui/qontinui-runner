/**
 * `OverlappingIntentsPanel` — the view derivation that decides what the
 * panel says (plan `2026-09-12-consolidate-local-orchestration-onto-conductor`
 * Phase 4).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the
 * panel is not rendered; the load-bearing decision — UNKNOWN versus empty
 * versus rows versus isolated — lives in the pure `deriveOverlapView`
 * helper and is pinned here. The JSX only renders the mode this returns.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { deriveOverlapView, type OverlapReadState } from "./OverlappingIntentsPanel";
import type { OverlappingIntentPair } from "./overlappingIntentsApi";
import { COORD_SOURCE_NO_ACCOUNT, deriveCoordGating } from "@/contexts/CoordModeContext";

const connected = deriveCoordGating({ mode: "connected", base: "http://c", source: "profile" });
const isolated = deriveCoordGating({
  mode: "isolated",
  base: null,
  source: COORD_SOURCE_NO_ACCOUNT,
});
const unknownMode = deriveCoordGating(null);

const pair: OverlappingIntentPair = {
  agentA: "agent-a",
  agentB: "agent-b",
  intentA: "touch foo",
  intentB: null,
  overlappingPaths: ["src/lib/foo.ts"],
};

function read(overrides: Partial<OverlapReadState>): OverlapReadState {
  return { rows: [], loaded: false, error: null, ...overrides };
}

describe("deriveOverlapView — a failed read is UNKNOWN, never an empty list", () => {
  it("a failed read renders UNKNOWN with the failure text, not 'no overlap'", () => {
    const v = deriveOverlapView(
      connected,
      read({ error: "coord unreachable: connection refused" }),
    );
    expect(v.mode).toBe("unknown");
    expect(v.error).toBe("coord unreachable: connection refused");
    expect(v.rows).toEqual([]);
    expect(v.countLabel).toBe("unknown");
    expect(v.mode).not.toBe("empty");
  });

  it("a failed read AFTER a successful one does not keep showing the stale rows as current", () => {
    const v = deriveOverlapView(connected, read({ rows: [pair], loaded: true, error: "boom" }));
    expect(v.mode).toBe("unknown");
    expect(v.rows).toEqual([]);
    expect(v.countLabel).toBe("unknown");
  });

  it("before any read has succeeded the panel is UNKNOWN, not empty", () => {
    const v = deriveOverlapView(connected, read({}));
    expect(v.mode).toBe("unknown");
    expect(v.error).toBeNull();
    expect(v.countLabel).toBe("unknown");
  });

  it("only a SUCCESSFUL read that returned nothing is 'empty'", () => {
    const v = deriveOverlapView(connected, read({ loaded: true }));
    expect(v.mode).toBe("empty");
    expect(v.countLabel).toBe("0 active");
  });

  it("a successful read with pairs renders them with a real count", () => {
    const v = deriveOverlapView(connected, read({ rows: [pair], loaded: true }));
    expect(v.mode).toBe("rows");
    expect(v.rows).toEqual([pair]);
    expect(v.countLabel).toBe("1 active");
  });
});

describe("deriveOverlapView — coord mode gating", () => {
  it("isolated wins over everything: no rows, no error, the disabled notice", () => {
    const v = deriveOverlapView(isolated, read({ rows: [pair], loaded: true, error: "x" }));
    expect(v.mode).toBe("isolated");
    expect(v.rows).toEqual([]);
    expect(v.error).toBeNull();
  });

  it("an UNRESOLVED coord mode fails open — the read state decides, not the gate", () => {
    expect(deriveOverlapView(unknownMode, read({ loaded: true })).mode).toBe("empty");
    expect(deriveOverlapView(unknownMode, read({ error: "e" })).mode).toBe("unknown");
  });
});

describe("OverlappingIntentsPanel error handling", () => {
  it("no catch site hand-rolls error stringification — it goes through describeThrown", () => {
    // `invoke()` rejects with a plain STRING; an `instanceof Error ?` ternary
    // would discard the cause on every real failure and render a bare
    // constant. Same guard the deleted CoordinatorDashboard carried.
    const source = readFileSync(join(__dirname, "OverlappingIntentsPanel.tsx"), "utf8");
    const offenders = source.match(/\w+\s+instanceof\s+Error\s*\?[^;]*/g) ?? [];
    expect(offenders).toEqual([]);
    expect(source).toContain("describeThrown(err");
  });
});
