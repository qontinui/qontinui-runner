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

/** Border colour of the focused zone when its state colour is the idle grey. */
export const ZONE_FOCUS_COLOR = "#7aa2f7";

/**
 * The FINISHED treatment — a chequered "finish flag" band around a zone whose
 * session is marked finished (the WORK axis; see `useFinishedSessions`).
 *
 * Chosen to be unmistakable at a glance across a full grid, and to not collide
 * with anything the border already says:
 *
 * - **Pattern, not hue, carries the meaning.** Every other zone border is a
 *   solid or dashed line whose colour is the activity state; a chequered band
 *   is a different shape entirely, so it reads without colour vision and can
 *   never be mistaken for `completed` (a solid green line — one finished
 *   *turn*, which is not a finished *session*).
 * - **The chequered flag is the universal "finished" sign.** Two rows of
 *   squares, so it reads as a chequerboard rather than a dashed line.
 * - **Green, because nobody has to act.** It is the `completed` state's green,
 *   the runner's "done" colour; red and amber stay reserved for states that
 *   oblige someone to do something.
 * - **The off-squares are transparent**, so they take the surface behind the
 *   zone instead of minting a second colour.
 */
export const FINISHED_BAND_PX = 6;
export const FINISHED_CHECK_COLOR = STATE_COLORS.completed;

/**
 * `border-image` value painting the chequered band in `color`.
 *
 * The source is an 18 px chequerboard of 3 px squares, sliced at the band
 * width: each corner and each edge tile is exactly two squares by two, and an
 * edge tile spans an even number of squares, so `round` repeats it without
 * breaking the alternation where tiles meet.
 */
export function finishedBorderImage(color: string = FINISHED_CHECK_COLOR): string {
  const square = FINISHED_BAND_PX / 2;
  const size = FINISHED_BAND_PX * 3;
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}">` +
    `<defs><pattern id="c" width="${FINISHED_BAND_PX}" height="${FINISHED_BAND_PX}" patternUnits="userSpaceOnUse">` +
    `<rect width="${square}" height="${square}" fill="${color}"/>` +
    `<rect x="${square}" y="${square}" width="${square}" height="${square}" fill="${color}"/>` +
    `</pattern></defs><rect width="${size}" height="${size}" fill="url(#c)"/></svg>`;
  return `url("data:image/svg+xml,${encodeURIComponent(svg)}") ${FINISHED_BAND_PX} / ${FINISHED_BAND_PX}px round`;
}

/**
 * Focus ring for a finished zone. The band replaces the border colour that
 * normally shows focus, so focus moves to a ring just outside it (the grid's
 * 2 px gap holds it).
 */
export const FINISHED_FOCUS_SHADOW = `0 0 0 2px ${ZONE_FOCUS_COLOR}`;

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
