/**
 * Phase 2 — Unzoned-count chip (honesty backstop past the 9-zone ceiling).
 *
 * The default zone layout is `single` (1 zone), and auto-grow (Phase 1) caps at
 * `full-grid` (9 zones). Any live tab beyond the current layout's zone capacity
 * renders as an offscreen `HiddenTerminal` (`<div className="hidden">`) with ZERO
 * affordance — this is exactly how 4 live gate-continuation sessions went
 * invisible to the operator. This chip is the visible backstop: whenever ANY
 * tab is unassigned it surfaces a compact "N more" pill near the StatusStrip;
 * clicking it opens the `ZoneControlPanel` whose Unassigned (N) list lets the
 * operator see and assign the hidden sessions.
 *
 * Renders NOTHING when nothing is hidden (`unassignedCount === 0`), matching the
 * StatusStrip's self-hide posture so it never adds chrome when the layout
 * already shows every session.
 *
 * Styling mirrors StatusStrip's `Pill` button idiom (10px, rounded, tokyo-night
 * palette) so the chip reads as part of the same status row.
 */

import { EyeOff } from "lucide-react";

/**
 * Pure label/visibility helper — exported so the unit test can lock the
 * count→label contract without rendering (the runner's vitest config is
 * `environment: "node"`, no jsdom; component JSX is verified by UI Bridge /
 * manual tests).
 *
 * Returns `null` when there is nothing hidden (chip must not render), otherwise
 * the chip's short label + its `title` tooltip.
 */
export function unzonedChipLabel(unassignedCount: number): { text: string; title: string } | null {
  if (unassignedCount <= 0) return null;
  return {
    text: `${unassignedCount} more`,
    title: `${unassignedCount} session${
      unassignedCount === 1 ? " is" : "s are"
    } not visible in any zone — click to open the zone control panel and assign them`,
  };
}

interface UnzonedChipProps {
  /** Count of live tabs not assigned to any visible zone. */
  unassignedCount: number;
  /** Opens the ZoneControlPanel (where the Unassigned (N) list lives). */
  onOpen: () => void;
}

export function UnzonedChip({ unassignedCount, onOpen }: UnzonedChipProps) {
  const label = unzonedChipLabel(unassignedCount);
  if (!label) return null;

  return (
    <div
      data-page-element="unzoned-chip"
      className="flex items-center px-2 py-1 bg-[#13141f]/40 backdrop-blur-sm border-b border-[#2a2d3d]/20 shrink-0"
    >
      <button
        type="button"
        onClick={onOpen}
        className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium leading-none whitespace-nowrap hover:bg-white/5 transition-colors"
        style={{ color: "#e0af68" }}
        title={label.title}
      >
        <EyeOff className="w-2.5 h-2.5" aria-hidden />
        <span>{label.text}</span>
      </button>
    </div>
  );
}
