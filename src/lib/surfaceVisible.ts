/**
 * surfaceVisible — "is this surface actually on screen right now?"
 *
 * Why this exists: `App.tsx` keeps `TerminalPage` MOUNTED on every tab
 * (line ~794 wraps it in a `hidden` div) so terminal PTYs survive a tab
 * switch. Its `window` keydown listener survives with it, so all ~23
 * terminal chords stayed live on the Builder, the Logs page and the
 * Active dashboard — where nothing they act on is visible.
 *
 * Two measured symptoms of that one defect:
 *
 *  - One `Ctrl+3` on the Active dashboard fired BOTH `DashboardPage`'s
 *    widget switch and the terminal's zone focus. Two `window`
 *    listeners on the same target both run; `stopPropagation()` does not
 *    suppress a sibling.
 *  - `Ctrl+/` `preventDefault()`ed and `stopPropagation()`ed app-wide
 *    and then focused nothing, because the CommandBar input it targets
 *    is inside that `display:none` subtree.
 *
 * A guard on the LISTENER (rather than a reassigned shortcut) is the
 * class fix: a chord a surface cannot act on should not be claimed, and
 * should not swallow the key from whoever can.
 *
 * Leaf module — no React — so it is unit-testable under vitest's
 * `environment: "node"`, same as `lib/globalChords.ts`.
 */

/**
 * True when `el` is attached to the document and rendered — i.e. not
 * inside a `display:none` / `content-visibility:hidden` subtree.
 *
 * `checkVisibility()` (Chromium 105+, which covers the Tauri WebView2
 * runtime) answers exactly that question and is used when present. The
 * fallback is `offsetParent`/`getClientRects()`, which agrees for every
 * case that matters here — a hidden ANCESTOR zeroes both.
 *
 * A null/undefined element is NOT visible. That is deliberate: the only
 * way a surface's root ref is null is that the surface has not rendered,
 * and a chord for an unrendered surface must be inert rather than
 * fail-open. Fail-open is how the leak above survived four iterations.
 */
export function isSurfaceVisible(el: Element | null | undefined): boolean {
  if (!el) return false;
  const html = el as HTMLElement & { checkVisibility?: () => boolean };
  if (typeof html.checkVisibility === "function") return html.checkVisibility();
  if (html.offsetParent !== null) return true;
  return el.getClientRects().length > 0;
}
