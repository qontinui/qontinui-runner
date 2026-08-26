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
 * Where the panel goes.
 *
 * A zone with the whole page has vertical space to spare and horizontal space
 * to give, so prompts become a full-height right-hand column. A tiled zone has
 * neither, so they become a short strip directly under its title bar — the
 * least it can cost while staying readable.
 */
export function promptsPanelOrientation(isSingleView: boolean): PromptsPanelOrientation {
  return isSingleView ? "right" : "top";
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
}): { top: number; right: number } {
  const chromeTop = opts.zoneHeaderPx + opts.filterBarPx;
  if (!opts.promptsOpen) return { top: chromeTop, right: 0 };
  return promptsPanelOrientation(opts.isSingleView) === "right"
    ? { top: chromeTop, right: PROMPTS_PANEL_RIGHT_WIDTH_PX }
    : { top: chromeTop + PROMPTS_PANEL_TOP_HEIGHT_PX, right: 0 };
}
