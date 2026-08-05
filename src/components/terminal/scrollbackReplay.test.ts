import { describe, expect, it } from "vitest";
import {
  trimReplayedChunk,
  isEmissionGap,
  resyncSliceStart,
  drainHeldChunks,
  lostWindowBytes,
  formatLostOutputMarker,
  RESYNC_INCOMPLETE_MARKER,
} from "./scrollbackReplay";

const bytes = (...vals: number[]) => new Uint8Array(vals);

describe("trimReplayedChunk", () => {
  it("returns the chunk whole when it carries no offset (pre-offset runner build)", () => {
    const b = bytes(1, 2, 3);
    expect(trimReplayedChunk({ bytes: b }, 100)).toBe(b);
  });

  it("returns the chunk whole when nothing was replayed", () => {
    const b = bytes(1, 2, 3);
    expect(trimReplayedChunk({ bytes: b, offset: 0 }, 0)).toBe(b);
  });

  it("drops a chunk entirely covered by the replay", () => {
    expect(trimReplayedChunk({ bytes: bytes(1, 2, 3), offset: 10 }, 13)).toBeNull();
    expect(trimReplayedChunk({ bytes: bytes(1, 2, 3), offset: 10 }, 50)).toBeNull();
  });

  it("returns the chunk whole when it starts at or after the boundary", () => {
    const b = bytes(1, 2, 3);
    expect(trimReplayedChunk({ bytes: b, offset: 13 }, 13)).toBe(b);
    expect(trimReplayedChunk({ bytes: b, offset: 20 }, 13)).toBe(b);
  });

  it("trims a straddling chunk to its unreplayed suffix", () => {
    // Chunk covers [10, 15); replay covers [_, 12) → keep bytes at 12, 13, 14.
    const out = trimReplayedChunk({ bytes: bytes(10, 11, 12, 13, 14), offset: 10 }, 12);
    expect(out).not.toBeNull();
    expect(Array.from(out!)).toEqual([12, 13, 14]);
  });

  it("treats an exact-boundary chunk end as fully covered (half-open ranges)", () => {
    // Chunk [10, 13), replay through 13 → covered; through 12 → 1 byte survives.
    expect(trimReplayedChunk({ bytes: bytes(1, 2, 3), offset: 10 }, 13)).toBeNull();
    const out = trimReplayedChunk({ bytes: bytes(1, 2, 3), offset: 10 }, 12);
    expect(Array.from(out!)).toEqual([3]);
  });
});

describe("isEmissionGap", () => {
  it("never signals without a baseline (nothing observed yet)", () => {
    expect(isEmissionGap(null, 500)).toBe(false);
  });

  it("never signals for an unstamped chunk (pre-offset runner build)", () => {
    expect(isEmissionGap(100, undefined)).toBe(false);
  });

  it("does not signal for a contiguous or overlapping chunk", () => {
    expect(isEmissionGap(100, 100)).toBe(false); // exactly contiguous
    expect(isEmissionGap(100, 40)).toBe(false); // replayed overlap / re-delivery
  });

  it("signals when the chunk starts beyond the expected offset", () => {
    expect(isEmissionGap(100, 101)).toBe(true);
    expect(isEmissionGap(100, 100_000)).toBe(true);
  });
});

describe("resyncSliceStart", () => {
  const ring = { startOffset: 1_000, endOffset: 5_000 };

  it("replays the whole ring when the hole predates the ring window", () => {
    expect(resyncSliceStart(ring, 0)).toBe(0);
    expect(resyncSliceStart(ring, 1_000)).toBe(0); // exactly at the ring start
  });

  it("starts at the written boundary when it falls inside the ring", () => {
    expect(resyncSliceStart(ring, 1_001)).toBe(1);
    expect(resyncSliceStart(ring, 4_999)).toBe(3_999);
  });

  it("returns an empty slice when the ring holds nothing new", () => {
    expect(resyncSliceStart(ring, 5_000)).toBe(4_000);
    expect(resyncSliceStart(ring, 9_000)).toBe(4_000);
  });
});

describe("drainHeldChunks", () => {
  it("drains contiguous chunks, trimming replayed prefixes", () => {
    const held = [
      { bytes: bytes(1, 2, 3), offset: 100 }, // fully below replayedThrough=103 → dropped
      { bytes: bytes(4, 5, 6), offset: 103 }, // straddles nothing, writable whole
    ];
    const res = drainHeldChunks(held, 106, 103);
    expect(res.reheld).toEqual([]);
    expect(res.writable.map((w) => Array.from(w))).toEqual([[4, 5, 6]]);
    expect(res.writtenThrough).toBe(106);
  });

  it("stops at the first hole and re-holds it plus everything after — never splices", () => {
    const held = [
      { bytes: bytes(1, 2), offset: 100 }, // contiguous (writtenThrough=102 after)
      { bytes: bytes(7, 8), offset: 110 }, // HOLE [102,110) precedes → re-held
      { bytes: bytes(9), offset: 112 }, // after the hole → re-held too
    ];
    const res = drainHeldChunks(held, 100, 0);
    expect(res.writable.map((w) => Array.from(w))).toEqual([[1, 2]]);
    expect(res.writtenThrough).toBe(102); // NOT advanced past the hole
    expect(res.reheld).toEqual([held[1], held[2]]);
  });

  it("re-holds everything when the very first chunk sits beyond a hole", () => {
    const held = [{ bytes: bytes(7, 8), offset: 110 }];
    const res = drainHeldChunks(held, 102, 0);
    expect(res.writable).toEqual([]);
    expect(res.writtenThrough).toBe(102);
    expect(res.reheld).toEqual(held);
  });

  it("writes unstamped chunks as-is (they cannot witness a hole)", () => {
    const held = [{ bytes: bytes(1) }, { bytes: bytes(2), offset: 200 }];
    // Unstamped chunk drains; the stamped one at 200 > writtenThrough=100 is a hole.
    const res = drainHeldChunks(held, 100, 0);
    expect(res.writable.map((w) => Array.from(w))).toEqual([[1]]);
    expect(res.reheld).toEqual([held[1]]);
  });

  it("drops zero-length (resume-marker) chunks while still advancing bookkeeping", () => {
    const held = [
      { bytes: new Uint8Array(0), offset: 100 }, // marker at the boundary: no hole, no write
      { bytes: bytes(5), offset: 100 },
    ];
    const res = drainHeldChunks(held, 100, 0);
    expect(res.writable.map((w) => Array.from(w))).toEqual([[5]]);
    expect(res.reheld).toEqual([]);
    expect(res.writtenThrough).toBe(101);
  });
});

describe("lostWindowBytes", () => {
  it("reports nothing lost when the ring still covers where the pane left off", () => {
    expect(lostWindowBytes({ startOffset: 0, endOffset: 1000 }, 400)).toBe(0);
    // Exactly abutting: the ring's oldest byte is the next one this pane needs.
    expect(lostWindowBytes({ startOffset: 400, endOffset: 1000 }, 400)).toBe(0);
  });

  it("reports the unrecoverable span when the ring rolled past the hole", () => {
    // The pane wrote through 400; the backend's oldest retained byte is 900.
    // Bytes [400, 900) exist nowhere any more.
    expect(lostWindowBytes({ startOffset: 900, endOffset: 2000 }, 400)).toBe(500);
  });

  it("treats a cold pane as lossless — a ring starting above 0 is just history", () => {
    // Nothing was ever written by this instance, so the ring not reaching back
    // to offset 0 is the session predating the mount, not dropped output.
    expect(lostWindowBytes({ startOffset: 5000, endOffset: 6000 }, 0)).toBe(0);
  });
});

describe("loss markers", () => {
  it("names the byte count and stays inside one line", () => {
    const marker = formatLostOutputMarker(512);
    expect(marker).toContain("512 bytes");
    // Leading + trailing CRLF only: the notice must not wrap into the stream.
    expect(marker.startsWith("\r\n")).toBe(true);
    expect(marker.endsWith("\r\n")).toBe(true);
    expect(marker.slice(2, -2)).not.toContain("\n");
    // Resets SGR so the pane's own colours are not left tinted.
    expect(marker).toContain("\x1b[0m");
  });

  it("the incomplete-resync marker is a distinct, self-terminating notice", () => {
    expect(RESYNC_INCOMPLETE_MARKER).not.toBe(formatLostOutputMarker(0));
    expect(RESYNC_INCOMPLETE_MARKER).toContain("\x1b[0m");
    expect(RESYNC_INCOMPLETE_MARKER.endsWith("\r\n")).toBe(true);
  });
});
