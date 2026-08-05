/**
 * Visibility-tiered pane service (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 2, root cause A4).
 *
 * Before: `visible` toggled a CSS class and triggered a re-fit — nothing else.
 * A hidden pane (an unassigned tab parked by `HiddenTerminal`, a non-maximized
 * zone under single-view, a compact zone card) kept a 250 ms ack timer, a
 * `ResizeObserver` and a full xterm parse+render pipeline running for every
 * chunk the operator could not see. With 20-40 sessions that is the dominant
 * idle cost of the terminal page.
 *
 * After: a hidden pane goes QUIET.
 *   - the ack-floor timer (and the `terminal_grid_text` title poll it carries)
 *     stops,
 *   - the `ResizeObserver` disconnects,
 *   - live chunks are dropped instead of parsed/written (see
 *     {@link routeOutputChunk}); state chips and sparklines keep updating
 *     because they are fed by the page-level output tap, not by the pane,
 *   - the pane deregisters as the terminal's render-ack consumer so the tap
 *     proxy-acks for it and backend emission never wedges.
 * On becoming visible again it re-fits, restarts the timers, and replays the
 * window it missed from the Rust scrollback ring (`terminal_get_scrollback`,
 * which also resets flow control) — so the operator sees gap-free content.
 *
 * Deliberately keyed on `visible` ALONE, independent of `classifyTabs`:
 * `classifyTabs` short-circuits every preset layout (≤9 tabs) to `"assigned"`
 * = all mounted, ignoring viewport proximity entirely, so a flow-mode-only
 * tier would leave preset layouts with no throttling at all. `visible` is the
 * one signal both layout families produce.
 *
 * Leaf module (zero imports) so vitest can drive the state machine and the
 * routing without loading xterm — same rationale as `flowControl.ts` and
 * `scrollbackReplay.ts`.
 */

/** Where a freshly-arrived `terminal-output` chunk must go. */
export type OutputRoute =
  /** Pane is hidden: don't parse or write. The ring covers the replay. */
  | "drop"
  /** Backend still initializing: stash until it exists. */
  | "buffer"
  /** A hole precedes this chunk (or a resync is running): hold for ordering. */
  | "hold"
  /** Normal path: trim against the replay boundary and write. */
  | "write";

export interface OutputRouteState {
  /** Pane is hidden (visibility tier "paused"). */
  paused: boolean;
  /** The terminal backend has been created and drained its pending bytes. */
  backendReady: boolean;
  /** This chunk starts beyond the end of the last observed one. */
  gapDetected: boolean;
  /** A scrollback-ring resync is in flight. */
  resyncInFlight: boolean;
}

/**
 * Pure routing decision for one live chunk.
 *
 * `paused` is checked FIRST and on purpose: a hidden pane must not accumulate
 * buffered or held chunks (unbounded growth for a pane nobody is watching).
 * Its recovery path is the ring replay on resume, which is bounded by the
 * ring, not by how long the pane stayed hidden.
 */
export function routeOutputChunk(state: OutputRouteState): OutputRoute {
  if (state.paused) return "drop";
  if (!state.backendReady) return "buffer";
  if (state.gapDetected || state.resyncInFlight) return "hold";
  return "write";
}

/**
 * What a pane is currently running.
 *
 *  - `none`   — the backend has not finished initializing, or the pane is gone.
 *  - `quiet`  — hidden: no ack timer, no ResizeObserver, no xterm writes. A
 *               slow title poll survives, because the tab title is the only
 *               signal a parked/compacted pane still owns (state chips and
 *               sparklines come from the page tap, the title does not).
 *  - `active` — visible: everything runs at full rate.
 */
export type PaneTier = "none" | "quiet" | "active";

/** Side effects the service drives on the owning `TerminalInstance`. */
export interface PaneVisibilityHooks {
  /** Start the ack-floor timer (with its ~1 Hz title poll) + `ResizeObserver`. */
  enterActiveTier(): void;
  /** Stop them. */
  leaveActiveTier(): void;
  /** Start the slow title-only poll a hidden pane keeps. */
  enterQuietTier(): void;
  /** Stop it. */
  leaveQuietTier(): void;
  /** Ack the rendered-but-sub-AckSize remainder before going quiet. */
  flushAck(): void;
  /** Re-fit (and focus) now that the pane has real dimensions again. */
  refit(): void;
  /**
   * Replay the missed window from the scrollback ring. May decline (a resync
   * is already in flight) or fail (the IPC rejects) — the caller reports the
   * replay landing via {@link PaneVisibilityService.noteResynced}, so nothing
   * here assumes it succeeded.
   */
  resync(): void;
  /**
   * Register/unregister this pane as the terminal's render-ack consumer.
   * While false, the page tap proxy-acks for the terminal.
   */
  setRenderConsumer(active: boolean): void;
}

/**
 * Visibility state machine for one pane. Owns the ordering rules so the
 * component effect stays declarative:
 *
 *   - timers never start before the backend is ready, and never start twice;
 *   - the ack remainder is flushed BEFORE the timer stops (otherwise the last
 *     rendered bytes of a burst never ack and the reader stays paused);
 *   - a resync is issued on reveal ONLY when output was actually dropped (so
 *     revealing an idle pane costs zero IPC) and the "dropped" flag survives
 *     until a replay actually COVERS the dropped window — measured by stream
 *     offset, not by the replay merely having run — so a declined, failed or
 *     short replay is retried on the next reveal instead of leaving a silent
 *     splice;
 *   - the render-consumer registration follows VISIBILITY ALONE, not
 *     readiness. A visible pane owns its acks from the moment it mounts: it
 *     buffers pre-ready bytes and writes them itself, so if the page tap also
 *     proxy-acked them during the (up to ~1 s) backend init the terminal would
 *     be double-acked and its emission backpressure disabled for the rest of
 *     the session.
 */
export class PaneVisibilityService {
  private visible: boolean;
  private ready = false;
  private tier: PaneTier = "none";
  private consumerActive = false;
  private missedOutput = false;
  /**
   * Exclusive end offset of the furthest chunk dropped since the pane was last
   * caught up. A replay clears {@link missedOutput} only once it has reached
   * at least this far — see {@link noteResynced}. `0` means "extent unknown"
   * (unstamped chunks), which any completed replay clears.
   */
  private missedThrough = 0;
  private disposed = false;

  constructor(
    private readonly hooks: PaneVisibilityHooks,
    initiallyVisible: boolean,
  ) {
    this.visible = initiallyVisible;
    // Claims the render-ack consumer registration immediately when visible.
    this.sync();
  }

  /** True while the pane is hidden — the routing tier used by `routeOutputChunk`. */
  get paused(): boolean {
    return !this.visible;
  }

  /** What is currently running. */
  get currentTier(): PaneTier {
    return this.tier;
  }

  /**
   * A chunk was dropped because the pane is hidden; reveal must resync.
   *
   * `endOffset` is the exclusive end of the dropped chunk in the session's
   * absolute output stream. Recording it is what makes {@link noteResynced}
   * able to tell a COMPLETED replay from one that merely ran: a chunk dropped
   * while the ring fetch was in flight sits beyond that (already-taken)
   * snapshot, so a replay of it does not catch the pane up. Omit it only for
   * unstamped chunks (a runner build predating offset stamping), where the
   * extent is unknowable and any completed replay clears the flag — the
   * pre-offset behavior.
   */
  noteMissedOutput(endOffset?: number): void {
    this.missedOutput = true;
    if (endOffset !== undefined && endOffset > this.missedThrough) {
      this.missedThrough = endOffset;
    }
  }

  /**
   * A ring replay completed, putting the stream through absolute offset
   * `replayedThrough` into the pane's buffer. Returns whether the pane is now
   * caught up.
   *
   * The flag clears ONLY when the replay actually reached every byte that was
   * dropped. Clearing it on any replay produced a silent splice: chunks
   * dropped between the ring snapshot and its arrival were marked missed, then
   * immediately un-marked by the replay of an older window — and nothing can
   * re-detect them afterwards, because `nextExpectedOffset` keeps advancing
   * across drops so the live stream looks contiguous. A `false` return means
   * the caller must retry (a newer ring covers the remainder) or surface the
   * gap.
   */
  noteResynced(replayedThrough: number): boolean {
    if (!this.missedOutput) return true;
    if (replayedThrough < this.missedThrough) return false;
    this.missedOutput = false;
    this.missedThrough = 0;
    return true;
  }

  /**
   * The replay could not be completed — the IPC rejected, the ring came back
   * empty, or the retry budget ran out. Keeps the pane armed so the next
   * reveal tries again; it does NOT advance the missed extent, because no new
   * bytes were dropped.
   */
  noteResyncFailed(): void {
    this.missedOutput = true;
  }

  /** True while output is known to be missing from this pane's buffer. */
  get behind(): boolean {
    return this.missedOutput;
  }

  /** The backend finished async init: timers may now run. */
  markReady(): void {
    if (this.disposed || this.ready) return;
    this.ready = true;
    this.sync();
  }

  /** Visibility changed (the `visible` prop). */
  setVisible(next: boolean): void {
    if (this.disposed || next === this.visible) return;
    this.visible = next;
    this.sync();
    // Layout only settles once the pane is displayed again.
    if (next && this.tier === "active") this.hooks.refit();
  }

  /** Unmount. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.sync();
  }

  /** Drive tier, consumer registration and catch-up to match the inputs. */
  private sync(): void {
    const nextTier: PaneTier =
      this.disposed || !this.ready ? "none" : this.visible ? "active" : "quiet";

    if (nextTier !== this.tier) {
      if (this.tier === "active") {
        this.hooks.flushAck();
        this.hooks.leaveActiveTier();
      } else if (this.tier === "quiet") {
        this.hooks.leaveQuietTier();
      }
      this.tier = nextTier;
      if (nextTier === "active") this.hooks.enterActiveTier();
      else if (nextTier === "quiet") this.hooks.enterQuietTier();
    }

    const wantConsumer = !this.disposed && this.visible;
    if (wantConsumer !== this.consumerActive) {
      this.consumerActive = wantConsumer;
      this.hooks.setRenderConsumer(wantConsumer);
    }

    if (this.tier === "active" && this.missedOutput) this.hooks.resync();
  }
}
