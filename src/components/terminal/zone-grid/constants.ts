import type { SessionState } from "../useZoneLayout";

export const STATE_BORDER_COLORS: Record<SessionState, string> = {
  idle: "#2a2d3d",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

export const STATE_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

export const STATE_GLOW: Record<SessionState, string> = {
  idle: "none",
  working: "0 0 4px rgba(122, 162, 247, 0.3)",
  "needs-input": "0 0 8px rgba(224, 175, 104, 0.4)",
  completed: "none",
  error: "0 0 4px rgba(247, 118, 142, 0.3)",
};

export const STATE_LABELS: Record<SessionState, string> = {
  idle: "Idle",
  working: "Working",
  "needs-input": "Needs Input",
  completed: "Completed",
  error: "Error",
};

export const STATE_BG_COLORS: Record<SessionState, string> = {
  idle: "bg-[#565f89]/10",
  working: "bg-[#7aa2f7]/10",
  "needs-input": "bg-[#e0af68]/15",
  completed: "bg-[#9ece6a]/10",
  error: "bg-[#f7768e]/10",
};

export const TREND_ICONS: Record<string, { symbol: string; color: string }> = {
  up: { symbol: "\u25B2", color: "#9ece6a" },
  down: { symbol: "\u25BC", color: "#f7768e" },
  stable: { symbol: "\u2015", color: "#565f89" },
};

/**
 * Height of a zone's title bar — `ZoneLabel`, and the single-zone
 * session-info strip that stands in for it.
 *
 * Both the bar and the terminal body's top padding read this, so it is a
 * CONTRACT rather than a description: `ZoneLabel` sets it explicitly, and
 * anything taller is clipped instead of silently overlapping the first line of
 * output. It drifted to 28px once — a `relative` dropdown wrapper is a block
 * container, so its inline-block button picked up the inherited line-height as
 * leading — and nothing caught it, because the padding was a magic number
 * agreeing with the bar only by hand.
 */
export const ZONE_HEADER_HEIGHT_PX = 20;

/** Height of a zone's output-filter bar, when open. */
export const ZONE_FILTER_BAR_HEIGHT_PX = 26;
