/**
 * Room the zone header keeps clear at its right end while the cell is hovered,
 * and the zone width below which the hover cluster collapses.
 *
 * `ZoneHoverActions` reveals its button cluster over the cell's top-right
 * corner on hover, which is the same row where `ZoneLabel` right-aligns the cwd
 * label (`ml-auto`). Padding the header by the cluster's width while the cell is
 * hovered pushes the label left of the cluster instead of under it.
 *
 * 148 px = the six-button cluster's span (`hoverActionsSpanPx(6)` = 144) plus a
 * 4 px gap; a remote tab's cluster carries a seventh button ("End on remote
 * session"), so its header reserves `hoverActionsSpanPx(7)` + 4 = 170. It takes
 * effect on the cluster's own 150 ms reveal delay, so a pointer passing over a
 * grid does not reflow every header it crosses.
 *
 * Narrow zones. The header items that cannot shrink below a floor need
 * `headerControlsMinPx()` = 280 px, or `headerControlsMinPx(true)` = 346 px
 * with a remote tab's controls (the per-item tables are in
 * `ZoneHoverActions.tsx`), and the row cannot clip them (its dropdowns open
 * below it). The hover padding REPLACES the row's right `px-2` padding, so the
 * row needs 8 (left padding) + those + the reserve: 436 px beside the 148 px
 * reserve, 524 for a remote tab beside its 170. Below that the toggles would sit under
 * the z-30 cluster and could not be clicked. So the cell is a CSS container
 * (`@container/zone`), and below those widths the cluster collapses: maximize,
 * restart, label, export and send-to-window move into ONE overflow menu, while
 * Close (and a remote tab's End button) stay visible, because Close carries the
 * `terminal.close-session-<id>` UI Bridge id automation clicks. The reserve
 * shrinks to the collapsed cluster: `hoverActionsSpanPx(2)` + 4 = 60, or
 * `hoverActionsSpanPx(3)` + 4 = 82 for a remote tab.
 *
 * Too narrow even for that. Beside the collapsed cluster the full header needs
 * 8 + 280 + 60 = 348 px (8 + 346 + 82 = 436 for a remote tab). Below those
 * widths the row's pin, filter, prompts and scroll-to-bottom toggles hide too,
 * freeing 4 x (14 + 6) = 80 px, and the overflow menu carries them (a set pin
 * or filter stays, as the only sign of it, and costs its 20 px back). The cluster
 * stays on the row: the band just under it holds the filter bar's Clear and the
 * prompts / review panels' Hide and Reload, so a Close moved there would take
 * the clicks meant for them, and it has no confirmation.
 *
 * Known limit: that row still needs 8 + 200 + 60 = 268 px with EVERY item present
 * and no pin or filter set (8 + 266 + 82 = 356 for a remote tab), and 20 px more
 * for each one set, so a narrower zone holding all of them still reaches under
 * the cluster. The review toggle goes first, and the menu carries it too. A
 * typical local Claude zone (no tmux position, no ephemeral marker, no tenant
 * badge, no review badge, label not being edited) needs 8 + 124 + 60 = 192 px, or
 * 216 with a tenant badge. A remote tab's own controls add to that: measured in
 * Chromium, a typical one with nothing set still has its review toggle under the
 * cluster at 250 px and is clear at 300.
 *
 * All of it is CSS — no ResizeObserver — and the class strings are literals so
 * Tailwind's scanner emits them; `ZoneHoverActions.inline.test.tsx` ties every
 * number here to the arithmetic it comes from.
 */
export const ZONE_HEADER_HOVER_RESERVE_PX = 148;
export const ZONE_HEADER_HOVER_RESERVE_REMOTE_PX = 170;
/** The collapsed cluster's reserve: `hoverActionsSpanPx(2)` (overflow + Close) + 4. */
export const ZONE_HEADER_COLLAPSED_RESERVE_PX = 60;
/** The same with a remote tab's End button: `hoverActionsSpanPx(3)` + 4. */
export const ZONE_HEADER_COLLAPSED_RESERVE_REMOTE_PX = 82;
/** Zone width (CSS px) below which the cluster collapses: 8 + 280 + 148. */
export const ZONE_HOVER_COLLAPSE_BELOW_PX = 436;
/** The same for a remote tab: 8 + 346 + 170. */
export const ZONE_HOVER_COLLAPSE_BELOW_REMOTE_PX = 524;
/** Zone width below which the row's own toggles move to the menu: 8 + 280 + 60. */
export const ZONE_HEADER_TOGGLES_HIDE_BELOW_PX = 348;
/** The same for a remote tab: 8 + 346 + 82. */
export const ZONE_HEADER_TOGGLES_HIDE_BELOW_REMOTE_PX = 436;

/** Makes the zone cell the named query container the variants below read. */
export const ZONE_CELL_CONTAINER_CLASS = "@container/zone";

export const ZONE_HEADER_HOVER_RESERVE_CLASS =
  "group-hover:pr-[148px] @max-[436px]/zone:group-hover:pr-[60px] transition-[padding] duration-200 group-hover:delay-150";
export const ZONE_HEADER_HOVER_RESERVE_REMOTE_CLASS =
  "group-hover:pr-[170px] @max-[524px]/zone:group-hover:pr-[82px] transition-[padding] duration-200 group-hover:delay-150";

/** On each collapsible action: gone below the collapse width. */
export const ZONE_HOVER_FULL_ONLY_CLASS = "@max-[436px]/zone:hidden";
export const ZONE_HOVER_FULL_ONLY_REMOTE_CLASS = "@max-[524px]/zone:hidden";
/** On the overflow button's wrapper: shown only below the collapse width. */
export const ZONE_HOVER_COLLAPSED_ONLY_CLASS = "hidden @max-[436px]/zone:flex";
export const ZONE_HOVER_COLLAPSED_ONLY_REMOTE_CLASS = "hidden @max-[524px]/zone:flex";

/** On each row toggle the overflow menu also carries: gone below the width above. */
export const ZONE_HEADER_TOGGLE_CLASS = "@max-[348px]/zone:hidden";
export const ZONE_HEADER_TOGGLE_REMOTE_CLASS = "@max-[436px]/zone:hidden";

/** The header's reserve class for a tab (a remote tab's cluster is one button wider). */
export function zoneHeaderReserveClass(remote: boolean): string {
  return remote ? ZONE_HEADER_HOVER_RESERVE_REMOTE_CLASS : ZONE_HEADER_HOVER_RESERVE_CLASS;
}

/** The class for a row toggle the overflow menu also carries, for a tab. */
export function zoneHeaderToggleClass(remote: boolean): string {
  return remote ? ZONE_HEADER_TOGGLE_REMOTE_CLASS : ZONE_HEADER_TOGGLE_CLASS;
}
