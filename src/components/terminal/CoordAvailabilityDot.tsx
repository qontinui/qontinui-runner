import type { TerminalCoordStatus } from "./useCoordTerminalStatus";

/** Colors match the existing terminal-page palette (`STATE_BORDER_COLORS` in
 *  `zone-grid/constants.ts`): tokyo-night green/yellow/red, not a generic
 *  traffic-light red/amber/green. */
const LIGHT_COLORS: Record<TerminalCoordStatus["light"], string> = {
  green: "#9ece6a",
  yellow: "#e0af68",
  red: "#f7768e",
};

const LIGHT_LABEL: Record<TerminalCoordStatus["light"], string> = {
  green: "Coord fully available",
  yellow: "Coord partially available",
  red: "Coord unavailable",
};

/**
 * The Terminal page's per-terminal coord-mcp light: a small colored circle
 * in the zone title bar, green/yellow/red per {@link TerminalCoordStatus}.
 *
 * Renders nothing when `status` is `undefined` — the caller (`ZoneLabel`)
 * only has a status for Claude-bound tabs, and a plain shell tab should
 * show no dot at all rather than a misleading red one.
 */
export function CoordAvailabilityDot({ status }: { status: TerminalCoordStatus | undefined }) {
  if (!status) return null;
  return (
    <span
      data-page-element="zone-coord-availability-dot"
      data-coord-light={status.light}
      data-coord-verdict={status.verdict}
      title={`${LIGHT_LABEL[status.light]} — ${status.detail}`}
      aria-label={`${LIGHT_LABEL[status.light]}: ${status.detail}`}
      className="w-1.5 h-1.5 rounded-full shrink-0"
      style={{ backgroundColor: LIGHT_COLORS[status.light] }}
    />
  );
}
