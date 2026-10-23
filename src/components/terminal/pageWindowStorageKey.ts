/**
 * The per-(page, window) `instanceStorage` key shape the terminal page uses
 * for everything it persists about one page in one window.
 *
 * Pop-out windows share the main window's localStorage (same WebView2 profile
 * per process), so per-window state MUST be namespaced by `windowLabel` or two
 * windows clobber each other. `"main"` keeps the un-suffixed key, and the
 * `"default"` page keeps the bare base key, so state written before either
 * namespace existed is still read.
 *
 * Shared by the zone layout (`useZoneLayout`) and the Session Manager sidebar
 * (`sessionSidebarPersistence`), so the two can never disagree about which
 * page/window a stored value belongs to.
 */
export function pageWindowStorageKey(
  base: string,
  pageId: string,
  windowLabel: string = "main",
): string {
  const pageKey = pageId === "default" ? base : `page:${pageId}:${base}`;
  return windowLabel === "main" ? pageKey : `${pageKey}:window:${windowLabel}`;
}
