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
    timers,
  } = options;

  /**
   * The `RegisteredElement` this attachment produced, or `null` while nothing
   * has landed. Object identity is the instance key — nothing else about a
   * terminal registration is unique per mount.
   */
  let ownRegistration: unknown = null;
  let lastError: unknown;

  const cancel = registerWhenReady({
    attempt: () => {
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
    },
    onGiveUp: onGiveUp ? (elapsedMs) => onGiveUp(elapsedMs, lastError) : undefined,
    intervalMs,
    timeoutMs,
    timers,
  });

  let released = false;
  return () => {
    if (released) return;
    released = true;
    cancel();
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
