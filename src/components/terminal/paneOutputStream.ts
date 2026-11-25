/**
 * One pane's view of its terminal's output stream: where each live chunk goes,
 * and the offsets that make a ring replay and the live stream line up.
 *
 * This logic was inlined in `TerminalInstance`'s mount effect. It is a leaf
 * module (no xterm / Tauri imports) because the component cannot be imported
 * under the runner's `environment: "node"`. That left the routing untestable,
 * and that is how two defects shipped:
 *
 *  - Received bytes were never acked unless they were written. That is fixed in
 *    `AckedWriteQueue`, which this class stages every chunk through.
 *  - The reconnect path attached its handler late. Bytes the runner sent between
 *    the mount's ring read (a flow-control reset) and the attach reached no
 *    handler, and `nextExpectedOffset` was still null, so nothing noticed the
 *    hole. The grid paint that follows already shows those bytes, so the fix
 *    must not replay them either. See {@link PaneOutputStream.applyMountRing}
 *    and {@link PaneOutputStream.lateAttachSettled}.
 *
 * The component keeps the side effects that need the backend: the resync's
 * ring read and write, and the flush. This class owns the bookkeeping those
 * side effects read and advance.
 */

import type { AckedWriteQueue } from "./ackedWriteQueue";
import { routeOutputChunk, type OutputRoute } from "./paneVisibilityService";
import { drainHeldChunks, isEmissionGap, type OffsetChunk } from "./scrollbackReplay";
import { base64ToBytes } from "./terminalOutputTap";

/** A `terminal-output` payload's wire shape (base64 data + absolute offset). */
export interface EncodedOutputPayload {
  data: string;
  offset?: number;
}

/** Decode one `terminal-output` payload to its bytes and offset. */
export function decodeOutputPayload(payload: EncodedOutputPayload): OffsetChunk {
  return { bytes: base64ToBytes(payload.data), offset: payload.offset };
}

/** What the stream needs from its pane, injected so tests can drive it. */
export interface PaneOutputHooks {
  /** The pane is hidden (its visibility tier is quiet). */
  isPaused(): boolean;
  /** The backend exists (a held or written chunk needs one). */
  hasBackend(): boolean;
  /** A chunk was dropped while hidden; `endOffset` is its exclusive end. */
  noteMissedOutput(endOffset: number | undefined): void;
  /** Start (or queue) a ring resync: a hole precedes the held chunks. */
  requestResync(): void;
  /** Flush the write queue on the next microtask. */
  scheduleFlush(): void;
}

/** {@link PaneOutputStream.receive}'s result: the route, or `discard` when there is no backend. */
export type ReceiveRoute = OutputRoute | "discard";

export class PaneOutputStream {
  /** Chunks received before the backend was ready, drained after the mount ring replay. */
  private pending: OffsetChunk[] = [];
  /** Post-gap chunks held until a resync's ring slice covers the hole. */
  private held: OffsetChunk[] = [];
  /** The backend exists and the mount drain has run. */
  backendReady = false;
  /** A resync's ring read is in flight. The component sets this. */
  resyncInFlight = false;
  /** A late attach is holding live chunks until its grid paint lands. */
  private awaitingPaint = false;
  /** A resync was asked for while the late attach waited; run it once the paint settles. */
  private resyncDeferred = false;
  /** Exclusive end of the newest ring replay written into the pane. */
  replayedThrough = 0;
  /** Exclusive end of the bytes the pane has (ring replays plus live chunks). */
  writtenThrough = 0;
  /** Exclusive end of the last chunk observed. `null` means no baseline yet, so no gap can be seen. */
  nextExpectedOffset: number | null = null;

  constructor(
    private readonly queue: AckedWriteQueue,
    private readonly hooks: PaneOutputHooks,
  ) {}

  /** Route one received chunk, and do the bookkeeping that route needs. */
  receive(chunk: OffsetChunk): ReceiveRoute {
    const { bytes, offset } = chunk;
    // Emission-gap detection: the runner gates webview emission (never the PTY
    // read) under backpressure, so a dropped chunk shows up as the next one
    // starting past the end of the last. Pre-ready holes are covered wholesale
    // by the mount ring replay, so only a ready pane looks.
    const gapDetected = this.backendReady && isEmissionGap(this.nextExpectedOffset, offset);
    // Advance in EVERY tier, hidden included: it is what stops the first chunk
    // after a reveal from reading as a fresh gap.
    if (offset !== undefined) {
      this.nextExpectedOffset = Math.max(this.nextExpectedOffset ?? 0, offset + bytes.length);
    }

    const route = routeOutputChunk({
      paused: this.hooks.isPaused(),
      backendReady: this.backendReady,
      gapDetected,
      // A late attach holds like a resync does, until its paint lands.
      resyncInFlight: this.resyncInFlight || this.awaitingPaint,
    });

    if (route === "drop") {
      // Hidden: no parse, no write, no ack (the runner does not count a
      // background-tier send). `writtenThrough` stays put, so the reveal resync
      // replays exactly this window from the ring.
      this.hooks.noteMissedOutput(offset === undefined ? undefined : offset + bytes.length);
      return route;
    }
    if (route === "buffer") {
      this.pending.push(chunk);
      return route;
    }
    if (!this.hooks.hasBackend()) return "discard";
    if (route === "hold") {
      this.held.push(chunk);
      // While a late attach waits for its paint, a gap is the paint's to
      // settle: a resync now would replay bytes the paint is about to show.
      if (gapDetected && !this.awaitingPaint) this.hooks.requestResync();
      return route;
    }
    // A chunk emitted before a ring snapshot can still be DELIVERED after the
    // replay (event delivery and invoke responses are not mutually ordered).
    // The queue trims it to its unreplayed suffix and acks all of it.
    this.queue.stageChunk(chunk, this.replayedThrough);
    if (!this.queue.isEmpty) this.hooks.scheduleFlush();
    this.advanceWritten(chunk);
    return route;
  }

  /**
   * The mount's ring read returned a window ending at `endOffset`. Call it for
   * every ring the read returns, an EMPTY one included: an empty ring is still
   * a valid anchor (offset 0 on a fresh terminal), and skipping it leaves
   * `nextExpectedOffset` null, so a later chunk past an unseen hole is spliced
   * in instead of read as a gap.
   *
   * Seeding the offsets is what makes a late attach safe. The ring read reset
   * the runner's counters. Anything sent after it that reached no handler
   * leaves the first live chunk starting past `endOffset`, which reads as a
   * gap, and a late attach that cannot paint ({@link lateAttachSettled})
   * replays from `writtenThrough`, so it starts exactly where this ring ended.
   */
  applyMountRing(endOffset: number): void {
    this.replayedThrough = endOffset;
    this.writtenThrough = Math.max(this.writtenThrough, endOffset);
    this.nextExpectedOffset = Math.max(this.nextExpectedOffset ?? 0, endOffset);
  }

  /**
   * The backend is ready: stage every chunk buffered during init (trimmed
   * against the mount ring, acked in full) and start routing live chunks to
   * the write path. Returns the bytes drained. The caller flushes
   * synchronously, so they land right behind the ring replay.
   */
  drainPending(): number {
    let drained = 0;
    for (const c of this.pending) {
      this.queue.stageChunk(c, this.replayedThrough);
      this.advanceWritten(c);
      drained += c.bytes.length;
    }
    this.pending = [];
    this.backendReady = true;
    return drained;
  }

  /**
   * The reconnect path is about to register its output handler, AFTER the
   * mount ring read (it defers the handler past the bootstrap resize) and
   * BEFORE it requests the grid snapshot it paints. Live chunks are held from
   * here until {@link lateAttachSettled}, so none races the paint.
   *
   * That snapshot request also resets the runner's flow-control counters
   * (`terminal_get_grid` with `resetFlowControl`), and {@link lateAttach}
   * issues it only once the runner-side listener is live. So every byte the
   * runner counts after that reset reaches this pane, and every byte counted
   * before is covered by it. The mount
   * ring read's own reset could not promise that: a frame tee'd into the ring
   * before it but counted after it went to no handler, and nothing acked it.
   * A byte past the snapshot was absorbed by the grid after it, so it is
   * emitted after the handler exists too (or skipped by the gate, which then
   * sends a resume marker).
   */
  beginLateAttach(): void {
    this.awaitingPaint = true;
  }

  /**
   * The reconnect path's attach, in the order the flow-control reset needs:
   * hold live chunks ({@link beginLateAttach}), register the handler, WAIT
   * until the runner-side listener is live, and only then request the
   * snapshot, whose reset is what every later byte relies on reaching this
   * pane. `register` returns as soon as the JS handler exists, but Tauri's
   * `listen()` is asynchronous: a reset issued before it resolves can count
   * bytes that are emitted to no listener, and nothing would ever ack them.
   *
   * Returns `undefined` without fetching when `isCancelled` says the pane was
   * disposed while it waited: the snapshot RESETS the terminal's counters,
   * which the pane that now shows this terminal (or the page tap) relies on,
   * and a stray reset would clear the runner's `emission_skipped` under it.
   */
  async lateAttach<T>(steps: {
    register(): void;
    listening(): Promise<unknown>;
    isCancelled(): boolean;
    snapshot(): Promise<T>;
  }): Promise<T | undefined> {
    this.beginLateAttach();
    steps.register();
    await steps.listening();
    if (steps.isCancelled()) return undefined;
    return steps.snapshot();
  }

  /**
   * Whether a resync may run now. While a late attach waits for its paint it
   * may not: its ring replay would start at the mount ring's end, under bytes
   * the paint is about to show. The request is remembered instead, and
   * {@link lateAttachSettled} issues it once the offsets reflect the paint.
   */
  mayResyncNow(): boolean {
    if (!this.awaitingPaint) return true;
    this.resyncDeferred = true;
    return false;
  }

  /**
   * The reconnect path's grid paint landed, or was skipped.
   *
   * `snapshotOffset` is the snapshot's `stream_offset`: where the grid had
   * absorbed the stream, at a point no escape sequence was open.
   *
   * - Painted (`painted` and an offset): the pane shows the stream up to the
   *   snapshot, so replaying any of it would apply relative output twice.
   *   What the pane shows is SET to the snapshot (not raised to it), and held chunks
   *   drain against it: what it covers is acked without a write, the rest
   *   written. Nothing needs acking for bytes no handler saw, since the
   *   snapshot's reset covered them. No ring read unless a held chunk starts
   *   past it, or the mount ring went further than the snapshot.
   *
   *   Set, because the mount ring can end past the snapshot: the grid's
   *   offset stops before a sequence a read boundary left open, and the ring
   *   does not. The paint's own escapes abort that open sequence in xterm, so
   *   writing the live remainder from the ring's end would print the tail of
   *   it (`196m`) as text. Anchored at the snapshot, the next chunk reads as a
   *   hole, and the resync replays from the boundary. Those bytes, [snapshot,
   *   ring end), were also written by the mount ring replay, so they may
   *   appear twice in scrollback (a known limitation, like the scrollback
   *   hole `TerminalSession::grid_snapshot` documents).
   * - Not painted: replay from the mount ring's end, by resync, unless the
   *   snapshot proves nothing was produced since the ring.
   *
   * Returns how far the snapshot is BEHIND the mount ring's end (0 normally).
   * A positive value is replayed from the snapshot by the resync requested
   * here; the caller logs it.
   */
  lateAttachSettled(painted: boolean, snapshotOffset: number | undefined): number {
    this.awaitingPaint = false;
    const deferred = this.resyncDeferred;
    this.resyncDeferred = false;
    if (painted && snapshotOffset !== undefined) {
      const behind = Math.max(0, this.writtenThrough - snapshotOffset);
      this.replayedThrough = snapshotOffset;
      this.writtenThrough = snapshotOffset;
      // `nextExpectedOffset` tracks what the runner has SENT, not what the
      // pane shows, so it keeps its maximum: lowering it would make the next
      // contiguous live chunk read as a fresh gap.
      this.nextExpectedOffset = Math.max(this.nextExpectedOffset ?? 0, snapshotOffset);
      if (!this.drainHeld() || deferred || behind > 0) this.hooks.requestResync();
      else if (!this.queue.isEmpty) this.hooks.scheduleFlush();
      return behind;
    }
    const noneHeld = this.drainHeld();
    const nothingMissed = snapshotOffset !== undefined && snapshotOffset <= this.writtenThrough;
    if (!noneHeld || !nothingMissed || deferred) this.hooks.requestResync();
    return 0;
  }

  /** A resync's ring window through `endOffset` was written into the pane. */
  applyResyncRing(endOffset: number): void {
    this.replayedThrough = Math.max(this.replayedThrough, endOffset);
    this.writtenThrough = Math.max(this.writtenThrough, endOffset);
    this.nextExpectedOffset = Math.max(this.nextExpectedOffset ?? 0, endOffset);
  }

  /**
   * Drain held chunks up to the first remaining hole, staging them (acked in
   * full). Returns whether nothing is left held.
   */
  drainHeld(): boolean {
    const drained = drainHeldChunks(this.held, this.writtenThrough, this.replayedThrough);
    this.held = drained.reheld;
    this.writtenThrough = drained.writtenThrough;
    // A held sync frame can sit wholly inside the snapshot, so ack by what was
    // consumed, not by what was written.
    this.queue.stage(drained.writable, drained.receivedBytes);
    if (!this.queue.isEmpty) this.hooks.scheduleFlush();
    return drained.reheld.length === 0;
  }

  /**
   * The resync gave up or finished: stage whatever is still held even though a
   * hole may precede it (spliced beats frozen), acked in full.
   */
  releaseHeld(): void {
    const rest = this.held;
    this.held = [];
    for (const c of rest) {
      this.queue.stageChunk(c, this.replayedThrough);
      this.advanceWritten(c);
    }
    if (!this.queue.isEmpty) this.hooks.scheduleFlush();
  }

  /**
   * The pane was re-rendered from a fresh ring ending at `endOffset` (remote
   * earlier-output). Set, not max'd: a live chunk that landed between the ring
   * snapshot and the reset was wiped with the old buffer, and anchoring below
   * it lets the ordinary gap detection pull it back.
   */
  reanchor(endOffset: number): void {
    this.replayedThrough = endOffset;
    this.writtenThrough = endOffset;
    this.nextExpectedOffset = endOffset;
  }

  private advanceWritten(c: OffsetChunk): void {
    if (c.offset !== undefined) {
      this.writtenThrough = Math.max(this.writtenThrough, c.offset + c.bytes.length);
    }
  }
}
