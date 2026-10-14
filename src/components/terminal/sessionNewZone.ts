/**
 * Where "open in a new zone" puts a session — the session search palette's
 * modifier action (Shift+Enter / the row's "New zone" button; plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4).
 *
 * In grid mode the session gets a zone of its own: the first empty zone, or —
 * when every zone is taken — the zone a one-step grow of the grid adds (the
 * same grow-only decision a quick launch makes, `computeQuickLaunchLayoutId`).
 * When the grid cannot grow, the plan says so (`full`) and the caller reports
 * it rather than silently doing a plain open. tmux mode has one viewport, so
 * a new zone there IS showing the session.
 *
 * Pure: no React.
 */

import type { TerminalWorkMode } from "./terminalWorkMode";
import { computeQuickLaunchLayoutId, type ZoneAssignments } from "./useZoneLayout";

export interface NewZoneInput {
  workMode: TerminalWorkMode;
  layoutId: string;
  zoneCount: number;
  assignments: ZoneAssignments;
}

export type NewZonePlan =
  /** tmux: show the session in the one viewport. */
  | { kind: "tmux" }
  /** An empty zone exists; the session goes there. */
  | { kind: "empty"; zone: number }
  /** Grow the grid to `layoutId`; its new zone `zone` is the session's. */
  | { kind: "grow"; layoutId: string; zone: number }
  /** Every zone is taken and the grid cannot grow. */
  | { kind: "full" };

/** The first zone holding no tab, or -1. */
export function firstEmptyZone(zoneCount: number, assignments: ZoneAssignments): number {
  for (let z = 0; z < zoneCount; z++) if (!assignments[z]) return z;
  return -1;
}

export function planNewZone(input: NewZoneInput): NewZonePlan {
  if (input.workMode === "tmux") return { kind: "tmux" };
  const empty = firstEmptyZone(input.zoneCount, input.assignments);
  if (empty >= 0) return { kind: "empty", zone: empty };
  const target = computeQuickLaunchLayoutId(
    input.layoutId,
    input.zoneCount,
    false,
    input.zoneCount + 1,
  );
  // Every existing zone is taken, so the grown grid's first new zone is the
  // one right after them.
  return target === null
    ? { kind: "full" }
    : { kind: "grow", layoutId: target, zone: input.zoneCount };
}
