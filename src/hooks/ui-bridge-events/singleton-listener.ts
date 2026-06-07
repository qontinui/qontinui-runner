/**
 * singleton-listener — install a Tauri event listener exactly once per
 * webview, no matter how many React components mount the corresponding
 * hook (or how many times StrictMode re-runs the effect).
 *
 * Why this exists:
 * The naive pattern
 *
 * ```ts
 * useEffect(() => {
 *   let unlisten: UnlistenFn | null = null;
 *   listen(evt, cb).then((fn) => { unlisten = fn; });
 *   return () => { if (unlisten) unlisten(); };
 * }, []);
 * ```
 *
 * has a race: `listen()` is async, so when React StrictMode mounts →
 * unmounts → remounts (which it does even in PRODUCTION builds, because
 * <React.StrictMode> is in the tree — only its dev console warnings are
 * stripped), the cleanup from the first mount runs while `unlisten` is
 * still `null`. The first listen() promise then resolves AFTER cleanup
 * and is never torn down. The remount installs a second listener. Net:
 * two (or more) live listeners on a single webview, each firing once per
 * emitted event → duplicate command execution. (Observed 4× on a fresh
 * single-window headless instance — PR #473 round-2 falsification.)
 *
 * The fix: track the subscription at MODULE scope, keyed by event name,
 * with a mount ref-count. The first mounter triggers the single
 * `listen()`; later mounters just bump the count. Cleanup decrements; the
 * underlying listener is torn down only when the count returns to zero,
 * and the teardown awaits the in-flight `listen()` promise so a cleanup
 * that races the initial subscribe still unlistens correctly.
 *
 * The HANDLER is read through a ref-like getter so the latest callback is
 * used without re-subscribing (deps-stable). This means a changing
 * handler identity never churns the subscription.
 */

import { listen, type UnlistenFn, type Event } from "@tauri-apps/api/event";

type Handler<T> = (event: Event<T>) => void | Promise<void>;

interface Subscription<T> {
  /** Count of currently-mounted consumers. */
  refCount: number;
  /** In-flight or resolved listen() promise (for race-safe teardown). */
  listenPromise: Promise<UnlistenFn> | null;
  /** Latest handler — invoked via a stable trampoline so the live
   *  listener always calls the most recent callback without re-subscribing. */
  handler: Handler<T>;
}

// Keyed by event name. `unknown` payloads; each acquire() narrows.
const subscriptions = new Map<string, Subscription<unknown>>();

/**
 * Acquire the singleton listener for `eventName`, installing it on first
 * use. Returns a release function to call on unmount.
 *
 * @param eventName Tauri event channel (e.g. "ui-bridge:invoke-request").
 * @param handler   Current handler. Updated in place on every acquire so
 *                   the live listener always dispatches to the latest
 *                   closure (no re-subscribe needed).
 */
export function acquireSingletonListener<T>(eventName: string, handler: Handler<T>): () => void {
  let sub = subscriptions.get(eventName) as Subscription<T> | undefined;

  if (!sub) {
    const created: Subscription<T> = {
      refCount: 0,
      listenPromise: null,
      handler,
    };
    // Stable trampoline: closed over `created`, so it always reads the
    // current `created.handler`. The Tauri listener is registered with
    // THIS function once and never re-registered.
    const trampoline: Handler<T> = (event) => created.handler(event);
    created.listenPromise = listen<T>(eventName, (event) => trampoline(event));
    sub = created;
    subscriptions.set(eventName, created as Subscription<unknown>);
  } else {
    // Already subscribed in this webview — just adopt the latest handler.
    sub.handler = handler;
  }

  sub.refCount += 1;
  const acquired = sub;

  let released = false;
  return function release() {
    if (released) return;
    released = true;
    acquired.refCount -= 1;
    if (acquired.refCount > 0) return;

    // Last consumer left — tear down. Await the (possibly still in-flight)
    // listen() promise so a cleanup that races the initial subscribe still
    // unlistens correctly, then drop the registry entry so a future mount
    // re-subscribes cleanly.
    const promise = acquired.listenPromise;
    subscriptions.delete(eventName);
    if (promise) {
      void promise
        .then((unlisten) => {
          // Guard against a remount that re-acquired between our refCount
          // hitting 0 and this microtask: only unlisten if no new
          // subscription took over this event name.
          if (!subscriptions.has(eventName)) {
            unlisten();
          }
        })
        .catch(() => {
          /* listen() rejected — nothing to unlisten. */
        });
    }
  };
}

/** Test-only: number of live singleton subscriptions. */
export function _activeSubscriptionCount(): number {
  return subscriptions.size;
}

/** Test-only: clear the registry (does not unlisten — tests use mocks). */
export function _resetSingletonRegistry(): void {
  subscriptions.clear();
}
