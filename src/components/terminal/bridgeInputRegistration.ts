import { registerWhenReady } from "./registerWhenReady";

/**
 * Mount-independent UI Bridge registration for a terminal pane's input element.
 *
 * WHY this exists (manual-test-loop iter 13, item 1): iteration 12 replaced the
 * one-shot registration with a retry ladder (`registerWhenReady`), but left the
 * ladder *inside* `TerminalInstance`'s ~600-line async backend-init IIFE — the
 * "create" path. That IIFE has three ways to end before it ever reaches the
 * registration, none of which leave a trace:
 *
 *  1. `if (disposed) { backend.dispose(); return; }` right after
 *     `await createTerminalBackend(...)` — a pane that unmounts inside the
 *     ~200ms backend-creation window returns early;
 *  2. `createTerminalBackend` itself rejecting (WASM load, WebGL context
 *     exhaustion when several panes rehydrate at once);
 *  3. any throw in the several hundred lines of link-provider / viewport /
 *     key-handler wiring between backend creation and the registration.
 *
 * The IIFE was invoked as `(async () => { … })()` with **no `.catch`**, so all
 * three fail silently. And because `registerWhenReady` was never *called*, its
 * give-up warning could never fire either: the pane ended up live, painted,
 * `isAlive: true` in `GET /terminals`, with no `terminal-input-<id>` element and
 * **no log line anywhere**. Measured on the iteration-12 build: 4 xterm helper
 * textareas in the DOM, 2 registered elements, 0 of 3 restored panes registered,
 * and `writeToTerminal` answering HTTP 404 `ELEMENT_NOT_FOUND`.
 *
 * Fresh spawns mostly survived because they mount once, into a settled layout,
 * one at a time. Rehydration is the opposite: five panes construct backends
 * simultaneously while the zone layout is still settling, which is precisely
 * where (1)–(3) bite.
 *
 * The fix is to stop coupling registration to backend *creation*. This helper
 * runs from its own effect on **every** mount — fresh or rehydrated — and polls
 * for the input element instead of being handed one. Whatever the init path
 * does, the registration still lands as soon as the element and the registry
 * both exist, and if they never do, the ladder actually reaches its give-up
 * warning.
 *
 * ## Instance-keyed unregistration
 *
 * The second half of the same bug: cleanup used to be
 * `unregisterElement('terminal-input-' + id)` — keyed on the **id**. Since the
 * id is per-terminal and not per-mount, a stale instance tearing down (a pane
 * moving between the hidden mount and a zone cell, or between zones) evicted
 * the element a *different, live* instance had just registered. Same visible
 * symptom, same silence.
 *
 * So cleanup here unregisters only when the registry's current entry for the id
 * is still the exact `RegisteredElement` object THIS attachment produced. A
 * newer instance's registration is left alone.
 *
 * ## Landing once is not the same as OWNING it (iter 24, item 1)
 *
 * The ladder above stops at its first success, which is right for a race
 * against a not-yet-created input element and wrong for everything that happens
 * afterwards. `terminal-input-<id>` is a SHARED key space: `TerminalBridgeProxies`
 * claims the same id whenever it reads as unowned (iteration 18), and the
 * registry is last-write-wins. So a single successful landing decides ownership
 * for exactly as long as nothing else writes.
 *
 * Measured on the iteration-23 build: soft-navigate `/terminal` → `/settings` →
 * `/terminal`, and the remounted pane's element label read
 * `[no mounted view — …]` from then on. The mounted pane — visible, painted,
 * live xterm, live PTY — was served by the hidden 1×1 proxy textarea for the
 * rest of the session. Every consequence of that is a defect of its own: the
 * proxy's `focus` moves real focus onto an offscreen node, it advertises no
 * `paste`, and its `pasteText` hardcodes bracketed-paste off, so the same call
 * produced different bytes depending on which owner happened to win.
 *
 * A mounted instance is unconditionally the better owner — real focus, real
 * bounding rect, local echo, real bracketed-paste state — so it must WIN the id
 * back, not merely try once. Hence the reclaim watchdog: after the ladder
 * lands, a cheap poll (one Map lookup per pane per tick) re-registers whenever
 * the entry stops being ours. It is the mirror image of the proxy's own
 * persistent poll, and for the same stated reason — ownership flips every time
 * a pane scrolls in or out of a flow grid, so a one-shot decision cannot hold.
 */

/** The slice of the UI Bridge registry this module needs. */
export interface BridgeInputRegistryLike {
  registerElement(id: string, element: object, options: object): unknown;
  unregisterElement(id: string): boolean;
  getElement?(id: string): unknown;
}

export interface AttachBridgeInputOptions {
  /** Registry element id, e.g. `terminal-input-<terminalId>`. */
  elementId: string;
  /** Read the CURRENT registry. Context state — may be null for a while. */
  getRegistry: () => BridgeInputRegistryLike | null | undefined;
  /**
   * Read the CURRENT input element. Renderer-created, so this is the half that
   * is usually late; a null captured at mount is the original defect.
   */
  getInputElement: () => object | null | undefined;
  /** Built fresh on the attempt that actually registers. */
  buildDescriptor: () => object;
  /**
   * Called once if the retry budget is exhausted. `lastError` carries the most
   * recent throw from an attempt, if any — a registration that keeps throwing
   * must not be reported as a plain timeout.
   */
  onGiveUp?: (elapsedMs: number, lastError?: unknown) => void;
  /** Injected for tests; forwarded to `registerWhenReady`. */
  intervalMs?: number;
  timeoutMs?: number;
  /**
   * Reclaim poll period, in ms. Default 250 — deliberately the same as
   * `attachSubordinateBridgeInput`'s `pollMs`, so the two watchers describe the
   * same hand-off granularity. Pass `0` to disable the watchdog entirely (only
   * a test that is asserting the pre-iteration-24 behaviour should).
   */
  reclaimMs?: number;
  timers?: {
    setInterval: (fn: () => void, ms: number) => ReturnType<typeof setInterval>;
    clearInterval: (handle: ReturnType<typeof setInterval>) => void;
  };
}

/**
 * Start (and keep retrying) the registration.
 *
 * @returns a cleanup that cancels any pending ladder and performs the
 * instance-keyed unregistration. Idempotent.
 */
export function attachBridgeInputRegistration(options: AttachBridgeInputOptions): () => void {
  const {
    elementId,
    getRegistry,
    getInputElement,
    buildDescriptor,
    onGiveUp,
    intervalMs,
    timeoutMs,
    reclaimMs = 250,
    // MUST wrap rather than pass the bare globals: `{ setInterval }` on an
    // object literal invokes with `this === timers`, and WebView2 throws
    // `TypeError: Illegal invocation`. See `registerWhenReady.ts`.
    timers = {
      setInterval: (fn, ms) => setInterval(fn, ms),
      clearInterval: (handle) => clearInterval(handle),
    },
  } = options;

  /**
   * The `RegisteredElement` this attachment produced, or `null` while nothing
   * has landed. Object identity is the instance key — nothing else about a
   * terminal registration is unique per mount.
   */
  let ownRegistration: unknown = null;
  let lastError: unknown;
  let released = false;
  let reclaimHandle: ReturnType<typeof setInterval> | null = null;

  const tryRegister = (): boolean => {
    const registry = getRegistry();
    const element = getInputElement();
    if (!registry || !element) return false;
    try {
      ownRegistration = registry.registerElement(elementId, element, buildDescriptor());
    } catch (err) {
      // A throwing attempt must neither kill the ladder nor take the page
      // down with it. Keep retrying; the give-up report carries the error, so
      // the failure is LOUD without being fatal. (An unguarded throw here is
      // the shape of the iteration-12 defect: it unwound into a caller that
      // swallowed it, and the pane went dark with nothing in the log.)
      lastError = err;
      return false;
    }
    return true;
  };

  /**
   * Keep the id pointing at the MOUNTED node (iter 24, item 1).
   *
   * Started only once something has landed, so a pane that never registers
   * still reports through `onGiveUp` rather than polling forever.
   */
  const startReclaimWatchdog = () => {
    if (reclaimHandle !== null || reclaimMs <= 0) return;
    reclaimHandle = timers.setInterval(() => {
      if (released) return;
      const registry = getRegistry();
      // `getElement` is optional on the registry slice. Without it there is no
      // way to tell "still ours" from "taken over", and re-registering blindly
      // every tick would churn the registry — so a registry that cannot answer
      // gets the pre-iteration-24 behaviour rather than a guess.
      if (!registry?.getElement) return;
      const current = registry.getElement(elementId);
      if (current === ownRegistration) return;
      // The entry is someone else's (the mount-independent proxy is the only
      // other claimant) or has been dropped. Take it back: a mounted view is
      // unconditionally the better owner.
      tryRegister();
    }, reclaimMs);
  };

  const cancel = registerWhenReady({
    attempt: () => {
      if (!tryRegister()) return false;
      startReclaimWatchdog();
      return true;
    },
    onGiveUp: onGiveUp ? (elapsedMs) => onGiveUp(elapsedMs, lastError) : undefined,
    intervalMs,
    timeoutMs,
    timers,
  });

  return () => {
    if (released) return;
    released = true;
    cancel();
    if (reclaimHandle !== null) {
      timers.clearInterval(reclaimHandle);
      reclaimHandle = null;
    }
    if (ownRegistration === null) return;
    const registry = getRegistry();
    if (!registry) return;
    // Instance-keyed: only tear down what THIS attachment put there. If a
    // newer instance has since re-registered the same id, the entry is its
    // element, not ours, and unregistering would blind a live pane.
    try {
      const current = registry.getElement?.(elementId);
      if (current !== undefined && current !== ownRegistration) return;
      registry.unregisterElement(elementId);
    } finally {
      ownRegistration = null;
    }
  };
}
