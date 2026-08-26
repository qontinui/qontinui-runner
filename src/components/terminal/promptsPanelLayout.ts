import {
  PROMPTS_PANEL_TOP_HEIGHT_PX,
  PROMPTS_PANEL_RIGHT_WIDTH_PX,
  type PromptsPanelOrientation,
} from "./ZonePromptsPanel";

/**
 * Pure layout rules for the per-zone prompts panel.
 *
 * The panel is an absolutely-positioned overlay, exactly like the zone title
 * bar and the output-filter bar it stacks under. That means its size has to be
 * added to the terminal body's padding by hand — if the two disagree the
 * terminal renders behind chrome, which is the bug class these helpers exist to
 * keep testable without a DOM (the runner's vitest config has no jsdom).
 */

/**
 * Terminal body a zone must keep, in px, no matter what chrome is open.
 *
 * Shrinking the body SIGWINCHes the PTY, and `TerminalInstance`'s resize path
 * has no floor of its own — its mount path does, and says why: a tiny resize
 * "would wipe the grid… the Rust grid is then destructively resized and stays
 * at 10x5 forever". ~6 rows at the default 17px cell height.
 */
export const MIN_TERMINAL_BODY_PX = 100;

/**
 * Below this, a prompts strip shows roughly one clipped card and is not worth
 * the rows it costs — the zone gets the right-hand column instead.
 */
export const MIN_PROMPTS_STRIP_PX = 48;

/**
 * Does this zone offer a prompts panel at all?
 *
 * A tab with no Claude session has no prompts — that is an absence, not an
 * unknown, so no toggle is rendered rather than a toggle that opens an empty
 * panel. A compact card replaces the whole zone body with its own summary UI,
 * so there is nothing for the panel to sit on top of.
 */
export function promptsPanelAvailable(opts: {
  claudeSessionId?: string;
  showCompactCard: boolean;
}): boolean {
  if (opts.showCompactCard) return false;
  return !!opts.claudeSessionId;
}

/**
 * Vertical room a strip could take without pushing the terminal under its
 * floor. Negative results clamp to 0.
 *
 * `zoneHeightPx` of 0 means "not measured yet" — the first render before the
 * ResizeObserver reports. Treated as unconstrained, so the strip renders at
 * its natural height and corrects a frame later rather than flashing empty.
 */
export function availableStripHeight(zoneHeightPx: number, chromeTopPx: number): number {
  if (zoneHeightPx <= 0) return PROMPTS_PANEL_TOP_HEIGHT_PX;
  return Math.max(0, zoneHeightPx - chromeTopPx - MIN_TERMINAL_BODY_PX);
}

/**
 * Where the panel goes.
 *
 * A zone with the whole page has vertical space to spare and horizontal space
 * to give, so prompts become a full-height right-hand column. A tiled zone
 * normally gets a short strip under its title bar — unless it is too SHORT to
 * afford one, in which case it also takes the column: width is the axis it has
 * left, and a resize on that axis changes columns rather than collapsing the
 * row count.
 */
export function promptsPanelOrientation(opts: {
  isSingleView: boolean;
  zoneHeightPx: number;
  chromeTopPx: number;
}): PromptsPanelOrientation {
  if (opts.isSingleView) return "right";
  return availableStripHeight(opts.zoneHeightPx, opts.chromeTopPx) < MIN_PROMPTS_STRIP_PX
    ? "right"
    : "top";
}

/** Rendered height of the top strip: its natural height, clamped to what fits. */
export function promptsStripHeight(zoneHeightPx: number, chromeTopPx: number): number {
  return Math.min(PROMPTS_PANEL_TOP_HEIGHT_PX, availableStripHeight(zoneHeightPx, chromeTopPx));
}

/**
 * Padding the terminal body needs so no overlay covers it.
 *
 * `zoneHeaderPx` is the title bar (0 when the zone renders none) and
 * `filterBarPx` the output-filter bar (0 when closed); both sit above the
 * prompts panel, which is why the panel's own top offset is their sum.
 */
export function zoneBodyPadding(opts: {
  zoneHeaderPx: number;
  filterBarPx: number;
  promptsOpen: boolean;
  isSingleView: boolean;
  zoneHeightPx: number;
}): { top: number; right: number } {
  const chromeTop = opts.zoneHeaderPx + opts.filterBarPx;
  if (!opts.promptsOpen) return { top: chromeTop, right: 0 };
  const orientation = promptsPanelOrientation({
    isSingleView: opts.isSingleView,
    zoneHeightPx: opts.zoneHeightPx,
    chromeTopPx: chromeTop,
  });
  return orientation === "right"
    ? { top: chromeTop, right: PROMPTS_PANEL_RIGHT_WIDTH_PX }
    : { top: chromeTop + promptsStripHeight(opts.zoneHeightPx, chromeTop), right: 0 };
}
