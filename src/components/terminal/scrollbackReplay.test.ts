import { describe, expect, it } from "vitest";
import { trimReplayedChunk } from "./scrollbackReplay";

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
