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
 * The size contract of a zone overlay panel. The prompts panel and the review
 * panel (`SessionReviewPanel`) share the orientation rules below and differ
 * only in these numbers — the review panel carries a diff and a send bar, so
 * its strip is taller, its column wider, and a strip too short to hold it
 * sends it to the column sooner.
 */
export interface PanelGeometry {
  /** Natural height of the `"top"` strip. */
  topHeightPx: number;
  /** Width of the `"right"` column. */
  rightWidthPx: number;
  /** Below this, the strip is not worth its rows — the zone takes the column. */
  minStripPx: number;
}

export const PROMPTS_PANEL_GEOMETRY: PanelGeometry = {
  topHeightPx: PROMPTS_PANEL_TOP_HEIGHT_PX,
  rightWidthPx: PROMPTS_PANEL_RIGHT_WIDTH_PX,
  minStripPx: MIN_PROMPTS_STRIP_PX,
};

export const REVIEW_PANEL_GEOMETRY: PanelGeometry = {
  topHeightPx: 260,
  rightWidthPx: 420,
  minStripPx: 160,
};

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
export function availableStripHeight(
  zoneHeightPx: number,
  chromeTopPx: number,
  geometry: PanelGeometry = PROMPTS_PANEL_GEOMETRY,
): number {
  if (zoneHeightPx <= 0) return geometry.topHeightPx;
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
  geometry?: PanelGeometry;
}): PromptsPanelOrientation {
  if (opts.isSingleView) return "right";
  const geometry = opts.geometry ?? PROMPTS_PANEL_GEOMETRY;
  return availableStripHeight(opts.zoneHeightPx, opts.chromeTopPx, geometry) < geometry.minStripPx
    ? "right"
    : "top";
}

/** Rendered height of the top strip: its natural height, clamped to what fits. */
export function promptsStripHeight(
  zoneHeightPx: number,
  chromeTopPx: number,
  geometry: PanelGeometry = PROMPTS_PANEL_GEOMETRY,
): number {
  return Math.min(geometry.topHeightPx, availableStripHeight(zoneHeightPx, chromeTopPx, geometry));
}

/**
 * Padding the terminal body needs so no overlay covers it.
 *
 * `zoneHeaderPx` is the title bar (0 when the zone renders none) and
 * `filterBarPx` the output-filter bar (0 when closed); both sit above the
 * open panel, which is why the panel's own top offset is their sum.
 * `promptsOpen` means "an overlay panel is open"; `geometry` says which one
 * (the prompts panel's when omitted). At most one is open per zone.
 */
export function zoneBodyPadding(opts: {
  zoneHeaderPx: number;
  filterBarPx: number;
  promptsOpen: boolean;
  isSingleView: boolean;
  zoneHeightPx: number;
  geometry?: PanelGeometry;
}): { top: number; right: number } {
  const chromeTop = opts.zoneHeaderPx + opts.filterBarPx;
  if (!opts.promptsOpen) return { top: chromeTop, right: 0 };
  const geometry = opts.geometry ?? PROMPTS_PANEL_GEOMETRY;
  const orientation = promptsPanelOrientation({
    isSingleView: opts.isSingleView,
    zoneHeightPx: opts.zoneHeightPx,
    chromeTopPx: chromeTop,
    geometry,
  });
  return orientation === "right"
    ? { top: chromeTop, right: geometry.rightWidthPx }
    : { top: chromeTop + promptsStripHeight(opts.zoneHeightPx, chromeTop, geometry), right: 0 };
}

/**
 * Which overlay a zone shows. The prompts and review panels occupy the same
 * slot, so opening one closes the other for that tab: two overlays at the same
 * offset would stack, and the body padding can only reserve room for one.
 */
export type ZonePanelKind = "prompts" | "review";

/**
 * Toggle `kind` for `tabId` across the two per-page sets. Pure: returns new
 * sets (or the same set when it is unchanged), so React state can hold them.
 *
 * `review` is the set of tabs showing the review panel. `prompts` is the set
 * of tabs whose prompts panel DIFFERS from `promptsDefault` (the global
 * prompts-view switch, `promptsViewGlobal.ts`): with the default off it is
 * simply the open set; with it on it is the set of tabs closed by hand.
 *
 * Opening and closing go by what is SHOWING, not what is stored: a global
 * "show prompts" flip can leave a tab nominally prompts-open while its review
 * panel holds the slot. `reviewAvailable` is false where the zone cannot show
 * review at all, so a stale review entry never hides prompts there.
 */
export function toggleZonePanel(
  sets: { prompts: ReadonlySet<string>; review: ReadonlySet<string> },
  tabId: string,
  kind: ZonePanelKind,
  promptsDefault = false,
  reviewAvailable = true,
): { prompts: ReadonlySet<string>; review: ReadonlySet<string> } {
  const flip = (set: ReadonlySet<string>): ReadonlySet<string> => {
    const next = new Set(set);
    if (next.has(tabId)) next.delete(tabId);
    else next.add(tabId);
    return next;
  };
  const promptsOpen = promptsDefault !== sets.prompts.has(tabId);
  const reviewShowing = reviewAvailable && sets.review.has(tabId);
  if (kind === "review") {
    const opening = !sets.review.has(tabId);
    return {
      prompts: opening && promptsOpen ? flip(sets.prompts) : sets.prompts,
      review: flip(sets.review),
    };
  }
  if (promptsOpen && reviewShowing) {
    // Nominally open but hidden behind review: showing it means closing review.
    return { prompts: sets.prompts, review: flip(sets.review) };
  }
  const opening = !promptsOpen;
  return {
    prompts: flip(sets.prompts),
    review: opening && sets.review.has(tabId) ? flip(sets.review) : sets.review,
  };
}
