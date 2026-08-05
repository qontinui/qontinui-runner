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
 * `listen()`; later mounters just join the existing one. Cleanup removes
 * the handler; the underlying listener is torn down only when the last
 * handler leaves, and the teardown awaits the in-flight `listen()` promise
 * so a cleanup that races the initial subscribe still unlistens correctly.
 *
 * Handlers are held as a SET, one entry per acquisition, and the single
 * live listener fans out to all of them (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 2). The original
 * single-slot design (`sub.handler = handler`) meant a later acquirer
 * silently *overwrote* an earlier one, which capped the primitive at one
 * consumer per event name — unusable for `terminal-output`, which has two
 * independent consumers per window (the per-terminal demux and the page
 * output tap). Ref-counting is now simply `handlers.size`, so the
 * race-safe teardown semantics are unchanged.
 *
 * Each acquisition owns its own box, so re-acquiring with a *new* closure
 * no longer replaces the old one — release the old acquisition instead.
 * Consumers that want a deps-stable subscription with a changing callback
 * should keep doing what they already do: acquire once with a handler that
 * reads a ref.
 */

import { listen, type UnlistenFn, type Event } from "@tauri-apps/api/event";

type Handler<T> = (event: Event<T>) => void | Promise<void>;

/**
 * One acquisition's handler. Boxed so two acquisitions passing the
 * *identical* function reference still count (and release) separately.
 */
interface HandlerBox<T> {
  fn: Handler<T>;
}

interface Subscription<T> {
  /** In-flight or resolved listen() promise (for race-safe teardown). */
  listenPromise: Promise<UnlistenFn> | null;
  /** Live handlers — the single listener fans out to every one of them. */
  handlers: Set<HandlerBox<T>>;
}

// Keyed by event name. `unknown` payloads; each acquire() narrows.
const subscriptions = new Map<string, Subscription<unknown>>();

/**
 * Acquire the singleton listener for `eventName`, installing it on first
 * use. Returns a release function to call on unmount.
 *
 * Every live acquisition receives every event: one `listen()` per event
 * name per webview, N handlers behind it.
 *
 * @param eventName Tauri event channel (e.g. "ui-bridge:invoke-request").
 * @param handler   This acquisition's handler. Stays live until the
 *                   returned release function is called.
 */
export function acquireSingletonListener<T>(eventName: string, handler: Handler<T>): () => void {
  let sub = subscriptions.get(eventName) as Subscription<T> | undefined;
  const box: HandlerBox<T> = { fn: handler };

  if (!sub) {
    const created: Subscription<T> = {
      listenPromise: null,
      handlers: new Set([box]),
    };
    // Stable trampoline: closed over `created`, so it always fans out to the
    // current handler set. The Tauri listener is registered with THIS
    // function once and never re-registered. Iterating the Set directly is
    // safe — JS Set iteration tolerates deletion mid-iteration (a handler
    // that releases its own subscription) — and avoids an array allocation
    // on a per-output-chunk hot path.
    created.listenPromise = listen<T>(eventName, (event) => {
      for (const h of created.handlers) {
        try {
          void h.fn(event);
        } catch (e) {
          // One faulty consumer must not starve its siblings on a shared
          // listener — that is the whole point of the fan-out.
          console.error(`[singleton-listener] handler for "${eventName}" threw:`, e);
        }
      }
    });
    sub = created;
    subscriptions.set(eventName, created as Subscription<unknown>);
  } else {
    sub.handlers.add(box);
  }

  const acquired = sub;

  let released = false;
  return function release() {
    if (released) return;
    released = true;
    acquired.handlers.delete(box);
    if (acquired.handlers.size > 0) return;

    // Last consumer left — tear down. Await the (possibly still in-flight)
    // listen() promise so a cleanup that races the initial subscribe still
    // unlistens correctly, then drop the registry entry so a future mount
    // re-subscribes cleanly.
    //
    // The unlisten belongs to THIS subscription's own `listen()` call, so it
    // always runs: if a remount already installed a fresh subscription under
    // the same event name, that one owns a different listener and skipping
    // ours here would strand it live forever (a duplicate-delivery leak).
    const promise = acquired.listenPromise;
    if (subscriptions.get(eventName) === (acquired as Subscription<unknown>)) {
      subscriptions.delete(eventName);
    }
    if (promise) {
      void promise
        .then((unlisten) => {
          unlisten();
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

/** Test-only: number of live handlers behind one event name's listener. */
export function _handlerCount(eventName: string): number {
  return subscriptions.get(eventName)?.handlers.size ?? 0;
}

/** Test-only: clear the registry (does not unlisten — tests use mocks). */
export function _resetSingletonRegistry(): void {
  subscriptions.clear();
}
