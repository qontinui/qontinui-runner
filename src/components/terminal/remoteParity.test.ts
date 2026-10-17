/**
 * The local-vs-remote parity matrix (plan
 * `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
 * Phase C).
 *
 * Pins three things:
 *  1. Every action id the tab chrome renders (`ZONE_HOVER_ACTIONS`,
 *     `REMOTE_TAB_CONTROL_ACTIONS`) has a matrix row, and every row's roster
 *     ids are real.
 *  2. Each component renders its buttons FROM its roster. Every top-level
 *     button carries `data-zone-action` / `data-remote-action`, so a button
 *     added without a roster entry (and therefore without a row) fails here.
 *     The check reads the source, because the runner's vitest has no DOM
 *     (same approach as `FleetSessionPicker.wiring.test.ts`).
 *  3. The matrix rules: no `unknown` row, a `broken` row names its plan, and a
 *     `different-by-design` row carries the copy the operator sees.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { REMOTE_PARITY_MATRIX, REMOTE_RESTART_REFUSAL } from "./remoteParity";
import { REMOTE_TAB_CONTROL_ACTIONS } from "./RemoteTabControls";
import { ZONE_HOVER_ACTIONS } from "./ZoneHoverActions";

const source = (name: string) => readFileSync(fileURLToPath(new URL(name, import.meta.url)), "utf8");

const ZONE_IDS = Object.values(ZONE_HOVER_ACTIONS) as string[];
const REMOTE_IDS = Object.values(REMOTE_TAB_CONTROL_ACTIONS) as string[];
const ALL_ROSTER_IDS = [...ZONE_IDS, ...REMOTE_IDS];

describe("remote parity matrix: roster coverage", () => {
  it("has a row for every action the tab chrome renders", () => {
    const covered = new Set(REMOTE_PARITY_MATRIX.flatMap((r) => r.rosterIds));
    const missing = ALL_ROSTER_IDS.filter((id) => !covered.has(id));
    expect(missing).toEqual([]);
  });

  it("names only roster ids that exist", () => {
    const known = new Set(ALL_ROSTER_IDS);
    const stray = REMOTE_PARITY_MATRIX.flatMap((r) => r.rosterIds).filter((id) => !known.has(id));
    expect(stray).toEqual([]);
  });

  it("maps each roster id to exactly one row", () => {
    for (const id of ALL_ROSTER_IDS) {
      expect(REMOTE_PARITY_MATRIX.filter((r) => r.rosterIds.includes(id)).map((r) => r.id)).toHaveLength(1);
    }
  });

  it("has unique row ids", () => {
    const ids = REMOTE_PARITY_MATRIX.map((r) => r.id);
    expect(new Set(ids).size).toBe(ids.length);
  });
});

describe("remote parity matrix: components render from their rosters", () => {
  it("ZoneHoverActions stamps every button with a roster id or a sub-menu marker", () => {
    const src = source("./ZoneHoverActions.tsx");
    // Every <button> is either a top-level action (stamped from the roster) or
    // an explicitly marked sub-menu entry (export formats, window targets). A
    // new button carrying neither makes the counts disagree.
    const buttons = (src.match(/<button\b/g) ?? []).length;
    const stamped = [...src.matchAll(/data-zone-action=\{ZONE_HOVER_ACTIONS\.(\w+)\}/g)].map((m) => m[1]);
    const submenu = (src.match(/data-zone-submenu=/g) ?? []).length;
    expect(stamped.length + submenu).toBe(buttons);
    expect([...stamped].sort()).toEqual(Object.keys(ZONE_HOVER_ACTIONS).sort());
  });

  it("RemoteTabControls stamps every button with a roster id, and only those", () => {
    const src = source("./RemoteTabControls.tsx");
    const buttons = (src.match(/<button\b/g) ?? []).length;
    const stamped = [...src.matchAll(/data-remote-action=\{REMOTE_TAB_CONTROL_ACTIONS\.(\w+)\}/g)].map(
      (m) => m[1],
    );
    expect(stamped).toHaveLength(buttons);
    expect([...stamped].sort()).toEqual(Object.keys(REMOTE_TAB_CONTROL_ACTIONS).sort());
  });
});

describe("remote parity matrix: verdict rules", () => {
  it("ships no unknown row", () => {
    expect(REMOTE_PARITY_MATRIX.filter((r) => r.verdict === "unknown").map((r) => r.id)).toEqual([]);
  });

  it("gives every broken row a plan stem", () => {
    for (const r of REMOTE_PARITY_MATRIX.filter((x) => x.verdict === "broken")) {
      expect(r.planStem, r.id).toMatch(/^\d{4}-\d{2}-\d{2}-/);
    }
  });

  it("gives every different-by-design row the copy the operator sees", () => {
    for (const r of REMOTE_PARITY_MATRIX.filter((x) => x.verdict === "different-by-design")) {
      expect(r.operatorCopy?.trim(), r.id).toBeTruthy();
    }
  });

  it("cites evidence for every row", () => {
    for (const r of REMOTE_PARITY_MATRIX) {
      expect(r.evidence.trim().length, r.id).toBeGreaterThan(0);
      expect(r.localEntry.trim().length, r.id).toBeGreaterThan(0);
      expect(r.remotePath.trim().length, r.id).toBeGreaterThan(0);
    }
  });
});

describe("restart is refused for a remote tab on every path", () => {
  it("the matrix copy is the one the hover button and /restart show", () => {
    const restart = REMOTE_PARITY_MATRIX.find((r) => r.id === "restart");
    expect(restart?.operatorCopy).toBe(REMOTE_RESTART_REFUSAL);
    expect(source("./ZoneHoverActions.tsx")).toContain("REMOTE_RESTART_REFUSAL");
    expect(source("./commands/useTerminalCommands.ts")).toContain(
      'fail("not-restartable", REMOTE_RESTART_REFUSAL)',
    );
  });

  it("handleRestartInZone refuses a remote tab before spawning anything", () => {
    const src = source("./contexts/TransitionEffectsContext.tsx");
    const guard = src.indexOf('reason: "remote-session"');
    const spawn = src.indexOf("await createTerminal(");
    expect(guard).toBeGreaterThan(-1);
    expect(spawn).toBeGreaterThan(guard);
  });
});
