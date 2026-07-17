import type { ZoneAssignments } from "./useZoneLayout";

/**
 * Per-tab mount classification for a single render pass (Layer 1 of
 * `plans/terminal-grid-bootstrap-redesign.md`, extended by the flow-grid
 * virtualization plan Phase 3).
 *
 * Preset (non-flow) layouts use the two-way vocabulary — exactly one owner
 * per tab:
 *  - `"assigned"` — the tab owns a zone → mount the inline `TerminalInstance`.
 *  - `"hidden"`   — transient unassigned tab → `HiddenTerminal` keeps the PTY
 *                   session alive off-grid.
 *
 * Flow mode (`FLOW_GRID_ID`, the synthesized scrolling grid) splits the
 * assigned case in two so offscreen zones don't each keep a live xterm parser
 * resident (incident #535 — 30 parsers OOM'd the WebView2 renderer at ~8.6GB):
 *  - `"assigned-live"`    — assigned AND near the viewport → mount a real
 *                           `TerminalInstance`.
 *  - `"assigned-virtual"` — assigned but far offscreen → render the lightweight
 *                           `CompactZoneCard` with NO `TerminalInstance`. The
 *                           card's state comes from the Phase-2 global output
 *                           tap, which is instance-independent, so unmounting
 *                           the parser no longer blinds state tracking. When the
 *                           zone scrolls back near the viewport the classifier
 *                           flips it to `assigned-live`, the instance cold-mounts
 *                           and the grid-snapshot + PTY-ring scrollback replay
 *                           repaints full history for free.
 */
export type TabClassification = "assigned" | "assigned-live" | "assigned-virtual" | "hidden";

/**
 * Number of `TerminalInstance` mount owners a classification implies. The
 * dual-mount race (two subtrees registering the same `terminal-input-<id>` and
 * both listening on `terminal-output`) is impossible iff no tab ever exceeds 1.
 * Virtual zones legitimately mount 0 (the CompactZoneCard carries no parser), so
 * the invariant is "exactly one OR zero", never two.
 */
export function mountOwnerCount(classification: TabClassification): 0 | 1 {
  return classification === "assigned-virtual" ? 0 : 1;
}

/**
 * Pure, centralized classification of every tab for one render pass. Factored
 * out of `ZoneGrid`'s memo so the exactly-one-or-zero-owner invariant is unit
 * testable without an `IntersectionObserver` (jsdom lacks it).
 *
 * @param tabs           all tracked tabs (assigned + transient).
 * @param assignments    zone-index → tab-id map (the source of "owns a zone").
 * @param isFlowMode     whether the active layout is the synthesized flow grid.
 *                       Preset layouts keep today's all-mounted two-way result;
 *                       `nearViewport` is ignored for them.
 * @param nearViewport   tab-ids whose zone cell is at/near the viewport (within
 *                       the observer overscan). Only consulted in flow mode.
 */
export function classifyTabs(
  tabs: readonly { id: string }[],
  assignments: ZoneAssignments,
  isFlowMode: boolean,
  nearViewport: ReadonlySet<string>,
): Map<string, TabClassification> {
  const m = new Map<string, TabClassification>();
  const assignedIds = new Set(Object.values(assignments).filter((id): id is string => id != null));
  for (const t of tabs) {
    if (!assignedIds.has(t.id)) {
      m.set(t.id, "hidden");
      continue;
    }
    if (!isFlowMode) {
      m.set(t.id, "assigned");
      continue;
    }
    m.set(t.id, nearViewport.has(t.id) ? "assigned-live" : "assigned-virtual");
  }
  return m;
}
