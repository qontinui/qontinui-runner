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
 * `headerControlsMinPx()` = 256 px, or `headerControlsMinPx(true)` = 312 px
 * with a remote tab's controls (the per-item tables are in
 * `ZoneHoverActions.tsx`), and the row cannot clip them (its dropdowns open
 * below it). The hover padding REPLACES the row's right `px-2` padding, so the
 * row needs 8 (left padding) + those + the reserve: 412 px beside the 148 px
 * reserve, 490 for a remote tab beside its 170. Below that the toggles would sit under
 * the z-30 cluster and could not be clicked. So the cell is a CSS container
 * (`@container/zone`), and below those widths the cluster collapses: maximize,
 * restart, label, export and send-to-window move into ONE overflow menu, while
 * Close (and a remote tab's End button) stay visible, because Close carries the
 * `terminal.close-session-<id>` UI Bridge id automation clicks. The reserve
 * shrinks to the collapsed cluster: `hoverActionsSpanPx(2)` + 4 = 60, or
 * `hoverActionsSpanPx(3)` + 4 = 82 for a remote tab.
 *
 * Known limit: beside the collapsed cluster the full header still needs
 * 8 + 256 + 60 = 324 px (8 + 312 + 82 = 402 for a remote tab), so in a zone
 * narrower than that (with every item present) the rightmost toggles still
 * reach under the cluster. A typical local Claude zone (no tmux position, no
 * ephemeral marker, no review badge, label not being edited) needs about
 * 8 + 204 + 60 = 272 px.
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
/** Zone width (CSS px) below which the cluster collapses: 8 + 256 + 148. */
export const ZONE_HOVER_COLLAPSE_BELOW_PX = 412;
/** The same for a remote tab: 8 + 312 + 170. */
export const ZONE_HOVER_COLLAPSE_BELOW_REMOTE_PX = 490;

/** Makes the zone cell the named query container the variants below read. */
export const ZONE_CELL_CONTAINER_CLASS = "@container/zone";

export const ZONE_HEADER_HOVER_RESERVE_CLASS =
  "group-hover:pr-[148px] @max-[412px]/zone:group-hover:pr-[60px] transition-[padding] duration-200 group-hover:delay-150";
export const ZONE_HEADER_HOVER_RESERVE_REMOTE_CLASS =
  "group-hover:pr-[170px] @max-[490px]/zone:group-hover:pr-[82px] transition-[padding] duration-200 group-hover:delay-150";

/** On each collapsible action: gone below the collapse width. */
export const ZONE_HOVER_FULL_ONLY_CLASS = "@max-[412px]/zone:hidden";
export const ZONE_HOVER_FULL_ONLY_REMOTE_CLASS = "@max-[490px]/zone:hidden";
/** On the overflow button's wrapper: shown only below the collapse width. */
export const ZONE_HOVER_COLLAPSED_ONLY_CLASS = "hidden @max-[412px]/zone:flex";
export const ZONE_HOVER_COLLAPSED_ONLY_REMOTE_CLASS = "hidden @max-[490px]/zone:flex";

/** The header's reserve class for a tab (a remote tab's cluster is one button wider). */
export function zoneHeaderReserveClass(remote: boolean): string {
  return remote ? ZONE_HEADER_HOVER_RESERVE_REMOTE_CLASS : ZONE_HEADER_HOVER_RESERVE_CLASS;
}
