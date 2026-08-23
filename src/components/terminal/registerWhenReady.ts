/**
 * Keep attempting a registration until it actually lands.
 *
 * WHY this exists (manual-test-loop iter 12, item 1): `TerminalInstance`
 * registered its `terminal-input-<id>` UI Bridge element **once**, at mount,
 * behind `if (uiBridge?.registry && backend.getInputElement())`. Both halves
 * of that guard can legitimately be false at that instant:
 *
 *  - the xterm helper textarea is created by the renderer, so
 *    `getInputElement()` can still read `null` right after `backend.open()`
 *    (and a Ghostty backend's `term.textarea` attaches later still), and
 *  - the UI Bridge registry is context state, so a pane that mounts before
 *    the provider finishes wiring sees `registry == null`.
 *
 * A one-shot attempt that loses either race leaves a **live pane with no
 * registered element** — and since every terminal custom action
 * (`writeToTerminal`, `sendKeys`, `pasteText`, `getScrollback`) is dispatched
 * through that element, the whole end-to-end observability surface for that
 * pane is silently gone. Nothing retries, nothing reports; the pane looks
 * fine and answers `ELEMENT_NOT_FOUND` forever.
 *
 * Polling rather than a DOM event: the two failure halves are different kinds
 * of "not ready" (a DOM mutation vs. a React context value), so a
 * `MutationObserver` would only cover one of them. One predicate, polled,
 * covers both — and costs nothing in the overwhelmingly common case where the
 * first synchronous attempt already succeeds and no timer is ever created.
 */
export interface RegisterWhenReadyOptions {
  /**
   * One registration attempt. Return `true` once the registration landed;
   * `false` means "not ready yet, try again". Must be side-effect-free when
   * it returns `false`.
   */
  attempt: () => boolean;
  /** Poll period. Default 100ms. */
  intervalMs?: number;
  /**
   * Total budget before giving up. Default 15000ms. A bounded ladder, not an
   * infinite one: a pane that never attaches its input in 15s is broken in a
   * way retrying cannot fix, and an immortal timer on a disposed pane is its
   * own leak.
   */
  timeoutMs?: number;
  /** Called once if the budget is exhausted without a successful attempt. */
  onGiveUp?: (elapsedMs: number) => void;
  /** Injected for tests. Defaults to the global timers. */
  timers?: {
    setInterval: (fn: () => void, ms: number) => ReturnType<typeof setInterval>;
    clearInterval: (handle: ReturnType<typeof setInterval>) => void;
  };
}

/**
 * Runs `attempt()` immediately, then on an interval until it returns `true`
 * or the budget runs out.
 *
 * @returns a cancel function. Idempotent, and safe to call after the loop has
 * already stopped — callers wire it straight into an effect cleanup.
 */
export function registerWhenReady(options: RegisterWhenReadyOptions): () => void {
  const {
    attempt,
    intervalMs = 100,
    timeoutMs = 15000,
    onGiveUp,
    timers = { setInterval, clearInterval },
  } = options;

  // The synchronous first try. When the input and the registry are both
  // already there — the normal case — this is the whole function and no timer
  // is ever allocated.
  if (attempt()) return () => {};

  let handle: ReturnType<typeof setInterval> | null = null;
  let elapsed = 0;

  const stop = () => {
    if (handle !== null) {
      timers.clearInterval(handle);
      handle = null;
    }
  };

  handle = timers.setInterval(() => {
    elapsed += intervalMs;
    if (attempt()) {
      stop();
      return;
    }
    if (elapsed >= timeoutMs) {
      stop();
      onGiveUp?.(elapsed);
    }
  }, intervalMs);

  return stop;
}
