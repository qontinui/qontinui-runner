/**
 * Page → pop-out-window bindings (the "pop out page" feature).
 *
 * When a whole terminal PAGE is detached into its own OS window, that window is
 * bound to the page by its stable `pageId`. The authoritative store is the Rust
 * `window_assignments` registry (`WindowRecord.bound_page`), but the frontend
 * needs this mapping SYNCHRONOUSLY at first render — `useTerminalPages` must
 * know, before it picks an active page, which pages live in a pop-out so the
 * main window neither shows their tab nor tries to restore them (which would
 * race the pop-out's own restore). An async `get_window_assignments` round-trip
 * can't satisfy that.
 *
 * So this module keeps a `localStorage` MIRROR of `pageId → windowLabel`,
 * refreshed from the Rust truth on every load + window event by
 * `WindowAssignmentsContext`. It is shared across this process's windows (same
 * port-namespaced `instanceStorage`), and persists across restarts so the
 * synchronous boot read is correct before the Rust snapshot arrives.
 *
 * Mutations dispatch a `window` CustomEvent so same-window listeners
 * (`useTerminalPages`) update without polling. Cross-window propagation is
 * handled by each window recomputing from the Rust snapshot on the broadcast
 * `window-opened` / `window-closed` Tauri events.
 */

import { instanceStorage } from "./instance-storage";

const KEY = "qontinui-page-bindings";

/** Fired (same-window) whenever the binding map changes. */
export const PAGE_BINDINGS_EVENT = "qontinui:page-bindings-changed";

/** `pageId → windowLabel` for every page currently detached into a pop-out. */
export type PageBindings = Record<string, string>;

/** Synchronous read of the current bindings (safe default `{}`). */
export function getPageBindings(): PageBindings {
  const raw = instanceStorage.getJSON<PageBindings | null>(KEY, null);
  return raw && typeof raw === "object" ? raw : {};
}

/** The window hosting a detached page, or `null` when the page is docked. */
export function windowForPage(pageId: string): string | null {
  return getPageBindings()[pageId] ?? null;
}

/**
 * Overwrite the whole binding map (used when recomputing from the Rust
 * snapshot). No-ops + emits nothing when the map is unchanged.
 */
export function setPageBindings(next: PageBindings): void {
  const prev = getPageBindings();
  if (shallowEqual(prev, next)) return;
  instanceStorage.setJSON(KEY, next);
  emitChange();
}

/** Bind (`label`) or unbind (`null`) a single page. */
export function setPageBinding(pageId: string, label: string | null): void {
  const next = { ...getPageBindings() };
  if (label === null) {
    if (!(pageId in next)) return;
    delete next[pageId];
  } else {
    if (next[pageId] === label) return;
    next[pageId] = label;
  }
  instanceStorage.setJSON(KEY, next);
  emitChange();
}

/** Subscribe to binding changes. Returns an unsubscribe fn. */
export function subscribePageBindings(cb: () => void): () => void {
  if (typeof window === "undefined") return () => {};
  window.addEventListener(PAGE_BINDINGS_EVENT, cb);
  return () => window.removeEventListener(PAGE_BINDINGS_EVENT, cb);
}

function emitChange(): void {
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(PAGE_BINDINGS_EVENT));
  }
}

function shallowEqual(a: PageBindings, b: PageBindings): boolean {
  const ak = Object.keys(a);
  const bk = Object.keys(b);
  if (ak.length !== bk.length) return false;
  return ak.every((k) => a[k] === b[k]);
}
