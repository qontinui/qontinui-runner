/**
 * request-dedupe — exactly-once execution guard for tagged UI Bridge
 * request/response flows (`ui-bridge:invoke-request` /
 * `ui-bridge:evaluate-request`).
 *
 * Background (round-2, PR #473 falsification):
 * Even after the Rust side switched from a global `app_handle.emit(...)`
 * broadcast to `emit_to(main_window, ...)`, a single
 * `POST /ui-bridge/control/page/evaluate` was observed executing the
 * inner command 4× on a FRESH single-window headless instance. The cause
 * is NOT multiple windows — it is multiple Tauri listeners accumulated
 * INSIDE the one main webview (async `listen()` + synchronous cleanup
 * race, amplified by React StrictMode's mount→unmount→remount). N
 * listeners on the one window each fire once per emitted event → N
 * executions of the same `request_id`.
 *
 * The structural fix is a singleton subscription (see
 * `singleton-listener.ts`), but listener accumulation is a recurring
 * footgun, so this module adds a cheap defense-in-depth layer: a
 * per-webview, module-level, bounded record of `request_id`s that have
 * already been claimed. The FIRST listener to see a `request_id` claims
 * it and proceeds; every subsequent listener (or duplicate event)
 * observes the claim and drops the call. Net result: one execution and
 * one response per `request_id` regardless of how many listeners exist.
 *
 * Bounded: we keep at most {@link MAX_TRACKED} ids (insertion-ordered via
 * a Set) and evict the oldest when the cap is exceeded, so a long-lived
 * webview never leaks memory. The cap is generous relative to the number
 * of in-flight requests, so an evicted id is one whose response has long
 * since been delivered — a re-fire that old would be a non-issue anyway.
 */

/**
 * Maximum number of recently-seen request_ids retained per channel.
 * Far exceeds realistic concurrent in-flight requests; sized so an
 * evicted id is definitionally stale.
 */
export const MAX_TRACKED = 200;

/**
 * A bounded, insertion-ordered set of claimed request_ids. One instance
 * per logical channel (invoke vs evaluate) so the two flows never
 * cross-contaminate each other's ids.
 */
export class RequestDedupe {
  private readonly seen = new Set<string>();
  private readonly max: number;

  constructor(max: number = MAX_TRACKED) {
    this.max = max;
  }

  /**
   * Attempt to claim a request_id for execution.
   *
   * @returns `true` if THIS caller won the claim (first to see the id —
   *   proceed with execution). `false` if the id was already claimed by a
   *   prior listener/event (drop the call — someone else is handling it).
   *
   * Empty / non-string ids are never claimed and always return `false`;
   * the caller is responsible for its own missing-id handling (it can't
   * route a response without one anyway).
   */
  claim(requestId: string): boolean {
    if (typeof requestId !== "string" || requestId.length === 0) {
      return false;
    }
    if (this.seen.has(requestId)) {
      return false;
    }
    this.seen.add(requestId);
    // Evict oldest entries once over the cap. Set preserves insertion
    // order, so the first key is the oldest.
    while (this.seen.size > this.max) {
      const oldest = this.seen.values().next().value;
      if (oldest === undefined) break;
      this.seen.delete(oldest);
    }
    return true;
  }

  /** Test-only: current number of tracked ids. */
  size(): number {
    return this.seen.size;
  }

  /** Test-only: drop all tracked ids. */
  reset(): void {
    this.seen.clear();
  }
}

/**
 * Module-level singletons — one per channel. Shared across every mount of
 * the corresponding handler hook within this webview, which is exactly
 * what makes the guard effective against accumulated listeners (they all
 * consult the same Set).
 */
export const invokeRequestDedupe = new RequestDedupe();
export const evaluateRequestDedupe = new RequestDedupe();
