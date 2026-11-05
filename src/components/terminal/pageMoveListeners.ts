/**
 * Which terminal pages of THIS window are listening for `terminal-page-changed`.
 *
 * A live move (`terminal_set_page`, or `POST /terminals/{id}/move`) is adopted
 * by the TARGET page's `useTerminalManager` listener, and evicted by the source
 * page's. A page the reorganize has just created has no listener until its
 * `PageSessionScope` mounts and its `listen()` IPC resolves; a move made before
 * that drops the event — the source evicts the tab and nothing adopts it, so a
 * still-running PTY vanishes from every tab. The mover therefore waits here for
 * the target page before it moves anything onto it.
 *
 * Module state is per webview window, which is the right scope: the event is
 * broadcast to every window, but a page's tab lives in the window that shows it.
 */

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
