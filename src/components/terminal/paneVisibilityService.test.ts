/**
 * paneVisibilityService tests — the hidden-pane throttle and its gap-free
 * reveal (plan `2026-07-28-runner-many-sessions-performance` Phase 2 / A4).
 *
 * Two halves:
 *  1. the state machine (what starts/stops when, and in which order);
 *  2. a byte-exact stream simulation proving that dropping writes while hidden
 *     and replaying from the scrollback ring on reveal reproduces the stream
 *     EXACTLY — no hole, no duplicated boundary bytes.
 */

import { describe, it, expect, vi } from "vitest";
import {
  PaneVisibilityService,
  routeOutputChunk,
  type PaneVisibilityHooks,
} from "./paneVisibilityService";
import {
  drainHeldChunks,
  resyncSliceStart,
  trimReplayedChunk,
  isEmissionGap,
  type OffsetChunk,
} from "./scrollbackReplay";

function makeHooks() {
  const calls: string[] = [];
  const hooks: PaneVisibilityHooks = {
    enterActiveTier: vi.fn(() => void calls.push("active:on")),
    leaveActiveTier: vi.fn(() => void calls.push("active:off")),
    enterQuietTier: vi.fn(() => void calls.push("quiet:on")),
    leaveQuietTier: vi.fn(() => void calls.push("quiet:off")),
    flushAck: vi.fn(() => void calls.push("flushAck")),
    refit: vi.fn(() => void calls.push("refit")),
    resync: vi.fn(() => void calls.push("resync")),
    setRenderConsumer: vi.fn((active: boolean) =>
      calls.push(active ? "consumer:on" : "consumer:off"),
    ),
  };
  return { hooks, calls };
}

describe("routeOutputChunk", () => {
  const base = { paused: false, backendReady: true, gapDetected: false, resyncInFlight: false };

  it("writes on the normal path", () => {
    expect(routeOutputChunk(base)).toBe("write");
  });

  it("buffers before the backend exists", () => {
    expect(routeOutputChunk({ ...base, backendReady: false })).toBe("buffer");
  });

  it("holds across an emission gap and while a resync is in flight", () => {
    expect(routeOutputChunk({ ...base, gapDetected: true })).toBe("hold");
    expect(routeOutputChunk({ ...base, resyncInFlight: true })).toBe("hold");
  });

  it("drops while hidden — ahead of buffering and holding", () => {
    // A hidden pane must never accumulate pending/held chunks: it can stay
    // hidden indefinitely, and its recovery path is the (bounded) ring.
    expect(routeOutputChunk({ ...base, paused: true })).toBe("drop");
    expect(routeOutputChunk({ ...base, paused: true, backendReady: false })).toBe("drop");
    expect(routeOutputChunk({ ...base, paused: true, gapDetected: true })).toBe("drop");
    expect(routeOutputChunk({ ...base, paused: true, resyncInFlight: true })).toBe("drop");
  });
});

describe("PaneVisibilityService", () => {
  it("claims the render-ack consumer at mount, before the backend is ready", () => {
    // Load-bearing: a visible pane buffers pre-backend bytes and writes them
    // itself, so the page tap must NOT proxy-ack them too — a double ack
    // permanently disables that session's emission backpressure.
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, true);
    expect(calls).toEqual(["consumer:on"]);
    expect(svc.currentTier).toBe("none");
    svc.markReady();
    expect(calls).toEqual(["consumer:on", "active:on"]);
    expect(svc.currentTier).toBe("active");
  });

  it("a pane that mounts HIDDEN runs only the quiet tier", () => {
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, false);
    svc.markReady();
    expect(svc.paused).toBe(true);
    expect(svc.currentTier).toBe("quiet");
    // No ack timer, no ResizeObserver — just the slow title poll.
    expect(hooks.enterActiveTier).not.toHaveBeenCalled();
    // ...and the page tap owns its flow-control acks, since it renders nothing.
    expect(hooks.setRenderConsumer).not.toHaveBeenCalled();
    expect(calls).toEqual(["quiet:on"]);
  });

  it("flushes the ack remainder BEFORE stopping the timer that would have drained it", () => {
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, true);
    svc.markReady();
    calls.length = 0;
    svc.setVisible(false);
    expect(calls).toEqual(["flushAck", "active:off", "quiet:on", "consumer:off"]);
    expect(svc.paused).toBe(true);
  });

  it("resyncs on reveal only when output was actually dropped", () => {
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, true);
    svc.markReady();

    // Hidden while idle: nothing missed, so revealing costs zero IPC.
    svc.setVisible(false);
    calls.length = 0;
    svc.setVisible(true);
    expect(calls).toEqual(["quiet:off", "active:on", "consumer:on", "refit"]);

    // Hidden while streaming: the dropped window must be replayed.
    svc.setVisible(false);
    svc.noteMissedOutput();
    calls.length = 0;
    svc.setVisible(true);
    expect(calls).toEqual(["quiet:off", "active:on", "consumer:on", "resync", "refit"]);
  });

  it("stays armed until the replay actually lands", () => {
    // A resync can decline (one is already in flight against an older ring
    // snapshot) or fail (the IPC rejects). Clearing the flag on request would
    // splice those bytes out silently and forever: `nextExpectedOffset` keeps
    // advancing across drops, so no later gap detection can see the hole.
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, true);
    svc.markReady();
    svc.setVisible(false);
    svc.noteMissedOutput();
    svc.setVisible(true);
    expect(calls).toContain("resync");

    // Resync declined/failed — the next reveal must retry.
    svc.setVisible(false);
    calls.length = 0;
    svc.setVisible(true);
    expect(calls).toContain("resync");

    // Once the replay lands, reveals are free again.
    svc.noteResynced();
    svc.setVisible(false);
    calls.length = 0;
    svc.setVisible(true);
    expect(calls).not.toContain("resync");
  });

  it("resyncs at markReady when the pane was revealed mid-init after dropping", () => {
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, false);
    svc.noteMissedOutput(); // chunk arrived while hidden and pre-ready
    svc.setVisible(true); // revealed before the backend finished loading
    expect(calls).toEqual(["consumer:on"]); // still no timers
    svc.markReady();
    expect(calls).toEqual(["consumer:on", "active:on", "resync"]);
  });

  it("ignores redundant visibility updates", () => {
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, true);
    svc.markReady();
    calls.length = 0;
    svc.setVisible(true);
    svc.setVisible(true);
    expect(calls).toEqual([]);
    svc.setVisible(false);
    svc.setVisible(false);
    expect(calls).toEqual(["flushAck", "active:off", "quiet:on", "consumer:off"]);
  });

  it("dispose stops everything and hands acking back to the page tap", () => {
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, true);
    svc.markReady();
    calls.length = 0;
    svc.dispose();
    expect(calls).toEqual(["flushAck", "active:off", "consumer:off"]);
    expect(svc.currentTier).toBe("none");
    // Post-dispose transitions are inert — nothing may restart a timer on an
    // unmounted pane.
    svc.setVisible(false);
    svc.setVisible(true);
    svc.markReady();
    expect(calls).toEqual(["flushAck", "active:off", "consumer:off"]);
    expect(hooks.enterQuietTier).not.toHaveBeenCalled();
  });

  it("disposing a hidden pane stops the quiet tier and starts nothing", () => {
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, false);
    svc.markReady();
    calls.length = 0;
    svc.dispose();
    expect(calls).toEqual(["quiet:off"]);
    expect(hooks.flushAck).not.toHaveBeenCalled();
  });

  it("runs no tier at all when disposed before the backend was ready", () => {
    const { hooks, calls } = makeHooks();
    const svc = new PaneVisibilityService(hooks, true);
    svc.dispose();
    expect(calls).toEqual(["consumer:on", "consumer:off"]);
    expect(hooks.enterActiveTier).not.toHaveBeenCalled();
    expect(hooks.enterQuietTier).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Gap-free reveal: byte-exact stream simulation
// ---------------------------------------------------------------------------

/** A produced stream chunk plus its absolute offset (what Rust stamps). */
interface StreamChunk {
  bytes: Uint8Array;
  offset: number;
}

/**
 * Minimal model of a `TerminalInstance`'s output path: the SAME routing
 * decision (`routeOutputChunk`) and the SAME boundary math
 * (`scrollbackReplay`) the component uses, with the xterm write replaced by an
 * append to a byte log we can compare against the produced stream.
 */
class PaneModel {
  /** Everything "rendered", in write order. */
  readonly rendered: number[] = [];
  private writtenThrough = 0;
  private replayedThrough = 0;
  private nextExpectedOffset: number | null = null;
  private resyncInFlight = false;
  private held: OffsetChunk[] = [];
  readonly service: PaneVisibilityService;

  constructor(
    private readonly ring: { bytes: number[]; startOffset: number; endOffset: number },
    visible: boolean,
  ) {
    this.service = new PaneVisibilityService(
      {
        enterActiveTier: () => {},
        leaveActiveTier: () => {},
        enterQuietTier: () => {},
        leaveQuietTier: () => {},
        flushAck: () => {},
        refit: () => {},
        resync: () => this.resyncFromRing(),
        setRenderConsumer: () => {},
      },
      visible,
    );
    this.service.markReady();
  }

  /** Feed one live `terminal-output` chunk. */
  push(chunk: StreamChunk): void {
    const gapDetected = isEmissionGap(this.nextExpectedOffset, chunk.offset);
    this.nextExpectedOffset = Math.max(
      this.nextExpectedOffset ?? 0,
      chunk.offset + chunk.bytes.length,
    );
    const route = routeOutputChunk({
      paused: this.service.paused,
      backendReady: true,
      gapDetected,
      resyncInFlight: this.resyncInFlight,
    });
    if (route === "drop") {
      this.service.noteMissedOutput();
      return;
    }
    if (route === "hold") {
      this.held.push(chunk);
      if (gapDetected) this.resyncFromRing();
      return;
    }
    const unreplayed = trimReplayedChunk(chunk, this.replayedThrough);
    if (unreplayed && unreplayed.length > 0) this.write(unreplayed);
    this.writtenThrough = Math.max(this.writtenThrough, chunk.offset + chunk.bytes.length);
  }

  setVisible(visible: boolean): void {
    this.service.setVisible(visible);
  }

  /** The reveal path: ring fetch → missed slice → drain held chunks. */
  private resyncFromRing(): void {
    this.resyncInFlight = true;
    const from = resyncSliceStart(this.ring, this.writtenThrough);
    const slice = this.ring.bytes.slice(from);
    if (slice.length > 0) this.write(Uint8Array.from(slice));
    this.replayedThrough = Math.max(this.replayedThrough, this.ring.endOffset);
    this.writtenThrough = Math.max(this.writtenThrough, this.ring.endOffset);
    this.nextExpectedOffset = Math.max(this.nextExpectedOffset ?? 0, this.ring.endOffset);
    this.service.noteResynced();

    const drained = drainHeldChunks(this.held, this.writtenThrough, this.replayedThrough);
    this.held = drained.reheld;
    this.writtenThrough = drained.writtenThrough;
    for (const w of drained.writable) this.write(w);
    this.resyncInFlight = false;
  }

  private write(bytes: Uint8Array): void {
    for (const b of bytes) this.rendered.push(b);
  }
}

/** Deterministic stream: byte value = (offset % 251) + 1, chunked evenly. */
function makeStream(chunkSize: number, chunkCount: number): StreamChunk[] {
  const chunks: StreamChunk[] = [];
  for (let i = 0; i < chunkCount; i++) {
    const offset = i * chunkSize;
    const bytes = new Uint8Array(chunkSize);
    for (let j = 0; j < chunkSize; j++) bytes[j] = ((offset + j) % 251) + 1;
    chunks.push({ bytes, offset });
  }
  return chunks;
}

function flatten(chunks: StreamChunk[]): number[] {
  return chunks.flatMap((c) => [...c.bytes]);
}

/** The Rust ring: the tail `capacity` bytes of everything produced so far. */
function ringOver(all: number[], capacity: number) {
  const start = Math.max(0, all.length - capacity);
  return { bytes: all.slice(start), startOffset: start, endOffset: all.length };
}

describe("hidden → visible resync is gap-free", () => {
  it("replays exactly the missed window (no hole, no duplicate)", () => {
    const stream = makeStream(16, 10);
    const all = flatten(stream);
    const pane = new PaneModel(ringOver(all, 4096), true);

    // Visible: chunks 0-2 render live.
    for (const c of stream.slice(0, 3)) pane.push(c);
    expect(pane.rendered).toEqual(all.slice(0, 48));

    // Hidden: chunks 3-7 are dropped — no parse, no write.
    pane.setVisible(false);
    for (const c of stream.slice(3, 8)) pane.push(c);
    expect(pane.rendered).toEqual(all.slice(0, 48));

    // Revealed: the ring (which holds everything produced so far) replays the
    // missed window, and the remaining live chunks continue from there.
    pane.setVisible(true);
    for (const c of stream.slice(8)) pane.push(c);

    expect(pane.rendered).toEqual(all);
  });

  it("holds and trims chunks that arrive during the reveal resync", () => {
    const stream = makeStream(16, 8);
    const all = flatten(stream);
    // Ring snapshot taken when 7 of 8 chunks had been produced — so the final
    // chunk arrives live while the resync is still settling.
    const ring = ringOver(all.slice(0, 112), 4096);
    const pane = new PaneModel(ring, true);

    for (const c of stream.slice(0, 2)) pane.push(c);
    pane.setVisible(false);
    for (const c of stream.slice(2, 7)) pane.push(c);
    pane.setVisible(true);
    pane.push(stream[7]);

    expect(pane.rendered).toEqual(all);
  });

  it("replays the whole ring when the hole predates it, without duplicating", () => {
    const stream = makeStream(16, 12);
    const all = flatten(stream);
    // Tiny ring: the pane stayed hidden far longer than the ring's capacity,
    // so the earliest missed bytes are genuinely gone (backend-side loss, the
    // documented `resyncSliceStart` case) — but nothing may be duplicated and
    // the tail must be exact.
    const ring = ringOver(all, 64);
    const pane = new PaneModel(ring, true);

    for (const c of stream.slice(0, 2)) pane.push(c);
    pane.setVisible(false);
    for (const c of stream.slice(2)) pane.push(c);
    pane.setVisible(true);

    // First 32 bytes rendered live; then the ring's whole 64-byte window.
    expect(pane.rendered).toEqual([...all.slice(0, 32), ...all.slice(all.length - 64)]);
    // Rendered content is a subsequence of the stream with no repeats of the
    // boundary bytes: the live prefix ends at 32 and the ring starts at 128.
    expect(pane.rendered).toHaveLength(96);
  });

  it("keeps rendering normally when the pane is never hidden", () => {
    const stream = makeStream(16, 6);
    const all = flatten(stream);
    const pane = new PaneModel(ringOver(all, 4096), true);
    for (const c of stream) pane.push(c);
    expect(pane.rendered).toEqual(all);
  });
});
