import { describe, expect, it } from "vitest";

import { base64ToBytes } from "./terminalOutputTap";
import { decodeRingBytes } from "./backends/localScrollbackRing";
import { buildTransportCorpus, bytesToBase64 } from "./transportCorpus";

const SYNC_BEGIN = [0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x68]; // ESC[?2026h
const SYNC_END = [0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x6c]; // ESC[?2026l

/** Byte equality via Buffer — `toEqual` walks 100 KiB arrays element by element. */
function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return Buffer.from(a.buffer, a.byteOffset, a.length).equals(
    Buffer.from(b.buffer, b.byteOffset, b.length),
  );
}

describe("transport-cost corpus", () => {
  const corpus = buildTransportCorpus();
  const byName = new Map(corpus.map((s) => [s.name, s]));

  it("covers the three repaint geometries and the typing trace", () => {
    expect(corpus.map((s) => s.name)).toEqual([
      "repaint-120x40",
      "repaint-200x60",
      "repaint-250x80",
      "typing",
    ]);
  });

  it("repaint frames have the sizes the measurement depends on", () => {
    // 120×40 sits under the relay's 49 152-byte base64 hazard; the two larger
    // geometries sit over it — the corpus must exercise both sides.
    const sizes = (name: string) => byName.get(name)!.frames.map((f) => f.length);
    for (const n of sizes("repaint-120x40")) expect(n).toBeGreaterThan(20_000);
    for (const n of sizes("repaint-120x40")) expect(n).toBeLessThan(49_152);
    for (const n of sizes("repaint-200x60")) expect(n).toBeGreaterThan(49_152);
    for (const n of sizes("repaint-250x80")) expect(n).toBeGreaterThan(100_000);
    for (const n of sizes("repaint-250x80")) expect(n).toBeLessThan(262_144);
    expect(byName.get("repaint-120x40")!.frames).toHaveLength(8);
  });

  it("every repaint is one synchronized-output block", () => {
    for (const name of ["repaint-120x40", "repaint-200x60", "repaint-250x80"]) {
      for (const f of byName.get(name)!.frames) {
        expect([...f.subarray(0, 8)]).toEqual(SYNC_BEGIN);
        expect([...f.subarray(f.length - 8)]).toEqual(SYNC_END);
      }
    }
  });

  it("the typing trace is small writes", () => {
    const typing = byName.get("typing")!;
    expect(typing.frames.length).toBe(400);
    for (const f of typing.frames) expect(f.length).toBeLessThanOrEqual(16);
  });

  it("is deterministic", () => {
    const again = buildTransportCorpus();
    expect(again.map((s) => s.encoded)).toEqual(corpus.map((s) => s.encoded));
  });

  it("the pane/tap decoder and the ring decoder both invert the wire encoding", () => {
    for (const set of corpus) {
      set.frames.forEach((frame, i) => {
        const b64 = set.encoded[i];
        expect(sameBytes(base64ToBytes(b64), frame)).toBe(true);
        expect(sameBytes(decodeRingBytes(b64), frame)).toBe(true);
      });
    }
    const first = corpus[0].frames[0];
    expect(sameBytes(Buffer.from(bytesToBase64(first), "base64"), first)).toBe(true);
  });

  it("the concatenated buffer yields each frame as a view", () => {
    for (const set of corpus) {
      expect(set.buffer.byteLength).toBe(set.totalBytes);
      set.frames.forEach((frame, i) => {
        expect(sameBytes(new Uint8Array(set.buffer, set.offsets[i], frame.length), frame)).toBe(
          true,
        );
      });
    }
  });
});
