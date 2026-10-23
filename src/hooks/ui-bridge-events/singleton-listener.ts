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
  /**
   * In-flight or resolved listen() promise (for race-safe teardown). `null`
   * once a listen() has REJECTED: that listener never existed, so it is
   * dropped, and the next acquisition or readiness probe listens again.
   */
  listenPromise: Promise<UnlistenFn> | null;
  /** Live handlers — the single listener fans out to every one of them. */
  handlers: Set<HandlerBox<T>>;
  /** The stable trampoline every listen() for this subscription registers. */
  dispatch: (event: Event<T>) => void;
}

// Keyed by event name. `unknown` payloads; each acquire() narrows.
const subscriptions = new Map<string, Subscription<unknown>>();

/**
 * Start (or restart) `sub`'s listen(). A rejection drops the promise, so a
 * later acquisition or `singletonListenerReady` retries instead of every
 * consumer waiting forever on a listener that was never installed. The
 * handler set is kept: a retry that succeeds reaches every live handler.
 */
function startListen<T>(eventName: string, sub: Subscription<T>): Promise<UnlistenFn> {
  const promise = listen<T>(eventName, sub.dispatch);
  sub.listenPromise = promise;
  promise.catch((e) => {
    console.warn(`[singleton-listener] listen("${eventName}") failed; will retry:`, e);
    if (sub.listenPromise === promise) sub.listenPromise = null;
  });
  return promise;
}

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
    const handlers = new Set([box]);
    const created: Subscription<T> = {
      listenPromise: null,
      handlers,
      // Stable trampoline: closed over the handler set, so it always fans out
      // to the current handlers. The Tauri listener is registered with THIS
      // function (again only if a listen() failed). Iterating the Set directly
      // is safe — JS Set iteration tolerates deletion mid-iteration (a handler
      // that releases its own subscription) — and avoids an array allocation
      // on a per-output-chunk hot path.
      dispatch: (event) => {
        for (const h of handlers) {
          try {
            void h.fn(event);
          } catch (e) {
            // One faulty consumer must not starve its siblings on a shared
            // listener — that is the whole point of the fan-out.
            console.error(`[singleton-listener] handler for "${eventName}" threw:`, e);
          }
        }
      },
    };
    startListen(eventName, created);
    sub = created;
    subscriptions.set(eventName, created as Subscription<unknown>);
  } else {
    sub.handlers.add(box);
    // Its listen() failed: this acquisition retries it.
    if (!sub.listenPromise) startListen(eventName, sub);
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

/**
 * Resolves once the listener for `eventName` is live in the backend: Tauri's
 * `listen()` resolves only after the runner has registered it, and events
 * emitted before that reach no handler. `acquireSingletonListener` starts the
 * `listen()` without awaiting it, so a caller that must not miss an event
 * emitted after some point (a flow-control reset whose bytes it has to ack)
 * awaits this first. Resolves `true` when the listener is live, and `false`
 * when there is no subscription or its `listen()` failed twice: it never
 * rejects, so awaiting it cannot hang a mount. When the attempt it awaits
 * fails, it retries once before answering (and the next acquisition retries
 * too).
 */
export async function singletonListenerReady(eventName: string): Promise<boolean> {
  const sub = subscriptions.get(eventName);
  if (!sub) return false;
  const live = async (promise: Promise<UnlistenFn>) => {
    try {
      await promise;
      return true;
    } catch {
      return false;
    }
  };
  // Await the attempt in flight, or start one if the last one failed.
  if (await live(sub.listenPromise ?? startListen(eventName, sub))) return true;
  // That attempt failed. The pane waiting here is the one that needs the
  // listener (the terminal demux registers only once, so no later acquisition
  // would retry for it): try ONE more time, unless the subscription was torn
  // down or someone else already retried, in which case await theirs.
  if (subscriptions.get(eventName) !== sub) return false;
  return live(sub.listenPromise ?? startListen(eventName, sub));
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
