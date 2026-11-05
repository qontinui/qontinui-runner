/**
 * Which terminal pages of THIS window are listening for `terminal-page-changed`.
 *
 * A live move (`terminal_set_page`, or `POST /terminals/{id}/move`) is adopted
 * by the TARGET page's `useTerminalManager` listener, and evicted by the source
 * page's. A page the reorganize has just created has no listener until its
 * `PageSessionScope` mounts and its `listen()` IPC resolves. A move made in
 * that window misses the event: the source evicts the tab at once, and the
 * still-running PTY has no tab anywhere until the target page's next re-sync
 * (`resyncTabs`, on its mount and after any terminal exit) lists it there. The
 * mover therefore waits here for the target page before it moves anything onto
 * it, so the tab lands with the move rather than at some later re-sync.
 *
 * Module state is per webview window, which is the right scope: the event is
 * broadcast to every window, but a page's tab lives in the window that shows it.
 */

import { createLogger } from "@/lib/logger";

const logger = createLogger("PageMoveListeners");

/** pageId → how many listeners are registered (StrictMode double-mounts). */
const listening = new Map<string, number>();
/** pageId → callbacks waiting for its first listener. */
const waiters = new Map<string, Set<() => void>>();

/**
 * Record that `pageId` now listens for moves. Returns the release, which the
 * listener's cleanup calls; releasing twice is a no-op.
 */
export function markPageMoveListening(pageId: string): () => void {
  listening.set(pageId, (listening.get(pageId) ?? 0) + 1);
  const pending = waiters.get(pageId);
  if (pending) {
    waiters.delete(pageId);
    for (const wake of pending) wake();
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    const n = (listening.get(pageId) ?? 1) - 1;
    if (n <= 0) listening.delete(pageId);
    else listening.set(pageId, n);
  };
}

/** Whether `pageId` has a registered move listener right now. */
export function isPageMoveListening(pageId: string): boolean {
  return listening.has(pageId);
}

/**
 * Resolve `true` once `pageId` listens for moves (immediately when it already
 * does), or `false` after `timeoutMs` — a page that never came up, which the
 * caller must treat as "do not move onto it".
 */
export function waitForPageMoveListener(pageId: string, timeoutMs = 8_000): Promise<boolean> {
  if (listening.has(pageId)) return Promise.resolve(true);
  return new Promise((resolve) => {
    let pending = waiters.get(pageId);
    if (!pending) {
      pending = new Set();
      waiters.set(pageId, pending);
    }
    const set = pending;
    const settle = (ok: boolean) => {
      clearTimeout(timer);
      set.delete(wake);
      if (set.size === 0 && waiters.get(pageId) === set) waiters.delete(pageId);
      resolve(ok);
    };
    const wake = () => settle(true);
    const timer = setTimeout(() => settle(false), timeoutMs);
    set.add(wake);
  });
}

/**
 * Hold one page's `terminal-page-changed` registration for a React effect.
 * `registration` is the pending `listen()` call; the page is recorded as
 * listening only once it RESOLVES (the IPC registered the handler), and the
 * returned cleanup releases both. A registration that resolves after the
 * cleanup ran is unlistened at once and never recorded — the page is gone, and
 * a leaked handler would go on adopting and evicting tabs for it. A rejected
 * registration is logged and the page is never recorded: it is not listening,
 * so a mover waiting on it times out rather than moving a tab nobody adopts.
 */
export function holdPageMoveListener(
  pageId: string,
  registration: Promise<() => void>,
): () => void {
  let unlisten: (() => void) | null = null;
  let release: (() => void) | null = null;
  let disposed = false;
  void registration.then(
    (fn) => {
      if (disposed) {
        fn();
        return;
      }
      unlisten = fn;
      release = markPageMoveListening(pageId);
    },
    (err: unknown) => {
      logger.warn(
        `terminal-page-changed listener for page ${pageId} failed to register; moves onto it are not adopted until its next re-sync: ${String(err)}`,
      );
    },
  );
  return () => {
    disposed = true;
    unlisten?.();
    release?.();
  };
}
