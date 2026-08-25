import type { BridgeInputRegistryLike } from "./bridgeInputRegistration";

/**
 * Mount-INDEPENDENT UI Bridge registration for a terminal pane's input element.
 *
 * ## The gap this closes (manual-test-loop iter 18, item 1)
 *
 * Iteration 13 moved the registration out of `TerminalInstance`'s async
 * backend-init IIFE and into its own mount effect
 * (`./bridgeInputRegistration.ts`). That decoupled registration from backend
 * *creation* — but left it coupled to `TerminalInstance` *mounting*, and a
 * terminal pane is not always mounted.
 *
 * Flow-grid virtualization (`26fb8777`, 2026-07-18 — five weeks BEFORE the
 * iteration-13 fix, so this is a gap the fix never covered, not a regression it
 * introduced) classifies an assigned zone that is far offscreen as
 * `assigned-virtual`: `ZoneGrid` renders a `CompactZoneCard` and mounts **zero**
 * `TerminalInstance`s for it (`classifyTabs.ts`, `ZoneGrid.tsx`
 * `shouldMountInstance`). Worse, `nearViewport` starts as an EMPTY Set
 * (`useZoneVirtualization.ts`), so on the first commit after a restore
 * *every* assigned tab is virtual until the `IntersectionObserver` delivers its
 * first batch — and a pane below the fold never mounts at all until scrolled to.
 *
 * The visible result is exactly what iteration 17 measured after a runner
 * restart: a pane with a painted zone header and close button (both rendered
 * unconditionally by `ZoneCellInner`), an `isAlive: true` row in
 * `GET /terminals`, **no** `terminal-input-<id>` element, `404
 * ELEMENT_NOT_FOUND` from the element route — and no give-up warning, because
 * the retry ladder lives in a component that never mounted, so it never
 * *started*. It also explains why the absence survived a hard refresh: the zone
 * is still offscreen afterwards.
 *
 * Restore makes this far likelier than steady-state use, since restore is what
 * re-creates ten-plus panes at once and drives the layout into flow mode.
 *
 * ## The shape of the fix
 *
 * The codebase already solved the same problem once for writes:
 * `writeToTerminalById` prefers a mounted ref and falls back to the id-addressed
 * `terminal_write` command "whether or not its `TerminalInstance` is currently
 * mounted". The bridge element never got that treatment. This module is it — a
 * registration that lives as long as the TAB, not as long as the view.
 *
 * ## Subordinate, never competing
 *
 * A mounted `TerminalInstance` owns the real xterm helper textarea and must
 * always win: it is the element that can focus, receive keys and report a real
 * bounding rect. So this attachment is strictly subordinate. On every tick:
 *
 *  - id unowned  ⇒ claim it (register the proxy element);
 *  - id owned by US ⇒ nothing;
 *  - id owned by ANYONE ELSE ⇒ yield: forget our registration and never touch
 *    the entry. It belongs to a live instance.
 *
 * Because a child's effects run before its parent's, a mounting
 * `TerminalInstance` registers first and this attachment simply yields. When
 * that instance later unmounts (scrolled out of view) its instance-keyed
 * cleanup unregisters, and the next tick reclaims — so the element is present
 * across the whole tab lifetime with at most one poll interval of gap.
 *
 * The poll is persistent by design (no budget): ownership flips every time a
 * pane scrolls in or out of the flow grid, and a one-shot ladder would stop
 * watching after the first hand-off.
 *
 * ## The give-up report is the OTHER half of the item
 *
 * `onUnowned` fires once if the id stays unowned for the whole budget. That is
 * the give-up warning made genuinely reachable: it does not require a mounted
 * component, so it covers the never-mounted case that made iteration 17's
 * failure silent. Callers route it to the Rust side, because a webview console
 * line never reaches the runner log (the same reason
 * `terminal_report_tree_reset` exists).
 */
export interface SubordinateBridgeInputOptions {
  /** Registry element id, e.g. `terminal-input-<terminalId>`. */
  elementId: string;
  /** Read the CURRENT registry. Context state — may be null for a while. */
  getRegistry: () => BridgeInputRegistryLike | null | undefined;
  /**
   * Read the CURRENT proxy element. Rendered by the host component, so it is
   * normally there on the first tick; read live anyway because a ref callback
   * can land after the effect.
   */
  getElement: () => object | null | undefined;
  /** Built fresh on the tick that actually claims. */
  buildDescriptor: () => object;
  /** Poll period. Default 250ms — one Map lookup per tab per tick. */
  pollMs?: number;
  /**
   * How long the id may stay unowned before {@link onUnowned} fires. Default
   * 15000ms, matching `registerWhenReady`'s ladder budget so both reporters
   * describe the same "never landed" threshold.
   */
  unownedTimeoutMs?: number;
  /** Fires at most once per attachment. `elapsedMs` is the time unowned. */
  onUnowned?: (elapsedMs: number, lastError?: unknown) => void;
  /** Injected for tests. Wrapped so `this` stays the global. */
  timers?: {
    setInterval: (fn: () => void, ms: number) => ReturnType<typeof setInterval>;
    clearInterval: (handle: ReturnType<typeof setInterval>) => void;
  };
}

/**
 * Start the subordinate registration watcher.
 *
 * @returns a cleanup that stops the watcher and releases the id only if this
 * attachment still owns it. Idempotent.
 */
export function attachSubordinateBridgeInput(options: SubordinateBridgeInputOptions): () => void {
  const {
    elementId,
    getRegistry,
    getElement,
    buildDescriptor,
    pollMs = 250,
    unownedTimeoutMs = 15000,
    onUnowned,
    // MUST wrap rather than pass the bare globals: `{ setInterval }` on an
    // object literal invokes with `this === timers`, and WebView2 throws
    // `TypeError: Illegal invocation`. This exact mistake was manual-test-loop
    // iteration 12's whole rehydration failure — see `registerWhenReady.ts`.
    timers = {
      setInterval: (fn, ms) => setInterval(fn, ms),
      clearInterval: (handle) => clearInterval(handle),
    },
  } = options;

  /** What THIS attachment registered, or null while it owns nothing. */
  let ownRegistration: unknown = null;
  let lastError: unknown;
  let unownedFor = 0;
  let reported = false;
  let released = false;

  const tick = (advanceMs: number) => {
    const registry = getRegistry();
    if (registry) {
      const current = registry.getElement?.(elementId);
      if (current !== undefined && current !== null && current !== ownRegistration) {
        // A live `TerminalInstance` owns the id. Yield: forget our own record
        // so cleanup can never evict a registration that is not ours (the
        // instance-keyed rule `bridgeInputRegistration.ts` established, applied
        // from the other side).
        ownRegistration = null;
        unownedFor = 0;
        return;
      }
      if (current === ownRegistration && ownRegistration !== null) {
        unownedFor = 0;
        return;
      }
      // Unowned. Claim it if we have something to register.
      const element = getElement();
      if (element) {
        try {
          ownRegistration = registry.registerElement(elementId, element, buildDescriptor());
          unownedFor = 0;
          return;
        } catch (err) {
          // Never fatal, never silent: keep polling, and carry the error into
          // the give-up report so a throwing registry is not misread as a
          // plain timeout.
          lastError = err;
        }
      }
    }
    unownedFor += advanceMs;
    if (!reported && unownedFor >= unownedTimeoutMs) {
      reported = true;
      onUnowned?.(unownedFor, lastError);
    }
  };

  // Synchronous first tick: in the common case the proxy element and the
  // registry are both already there and no ownership gap is ever observable.
  tick(0);

  const handle = timers.setInterval(() => {
    if (released) return;
    tick(pollMs);
  }, pollMs);

  return () => {
    if (released) return;
    released = true;
    timers.clearInterval(handle);
    if (ownRegistration === null) return;
    const registry = getRegistry();
    if (!registry) return;
    try {
      const current = registry.getElement?.(elementId);
      if (current !== undefined && current !== ownRegistration) return;
      registry.unregisterElement(elementId);
    } finally {
      ownRegistration = null;
    }
  };
}
