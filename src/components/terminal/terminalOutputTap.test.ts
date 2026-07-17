/**
 * Tap parity tests — plan `2026-06-30-terminal-flow-grid-virtualization.md`
 * §Phase 2.
 *
 * Phase 2 moves session-state tracking off the per-instance `onOutput` prop and
 * onto ONE global `terminal-output` listener (the "tap") that decodes + feeds
 * `useSessionStateTracking.handleOutput` regardless of which `TerminalInstance`
 * components are mounted. These tests assert the tap produces the SAME data the
 * instance path used to produce, so downstream consumers (`sessionStates`,
 * `lastOutputLines`, activity sums) are unaffected.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom / React Testing
 * Library — see `TerminalInstance.test.tsx`), so — following the
 * `consumeInputChunk.test.ts` precedent — we exercise the pure core:
 *
 *   1. DECODE parity: the tap's per-terminal streaming `TextDecoder` yields
 *      byte-identical text to `TerminalInstance`'s own per-instance decoder for
 *      the same event byte-sequence, INCLUDING a multibyte code point split
 *      across a chunk boundary (the reason both use `{ stream: true }`).
 *   2. TRACKING parity under coalescing: feeding `detectSessionState` (the pure
 *      transition the hook applies per `handleOutput` call) with the tap's
 *      once-per-frame COALESCED text yields the same final `SessionState` as the
 *      instance path's line-by-line calls — the plan's explicit requirement that
 *      "coalesced text over a frame must produce the same tracking result as
 *      line-by-line".
 *   3. Housekeeping: activity-byte-length parity, per-tab isolation + ordering,
 *      and decoder pruning on tab close.
 */

import { describe, it, expect } from "vitest";
import { TerminalOutputCoalescer, base64ToBytes } from "./terminalOutputTap";
import { detectSessionState } from "./sessionStateDetector";
import type { SessionState } from "./useZoneLayout";

const enc = new TextEncoder();

/** UTF-8 encode `s`, then re-slice the bytes at the given absolute offsets to
 *  simulate PTY read boundaries that may fall MID code point. */
function bytesSplitAt(s: string, offsets: number[]): Uint8Array[] {
  const all = enc.encode(s);
  const bounds = [0, ...offsets, all.length];
  const out: Uint8Array[] = [];
  for (let i = 0; i < bounds.length - 1; i++) {
    out.push(all.subarray(bounds[i], bounds[i + 1]));
  }
  return out;
}

/**
 * Reference for `TerminalInstance`'s old `onOutput` decode: ONE streaming
 * decoder for the terminal's lifetime, `decode(bytes, { stream: true })` per
 * event, collecting each event's text (what `onOutput` received per event).
 */
function instanceDecodeTexts(chunks: Uint8Array[]): string[] {
  const decoder = new TextDecoder();
  return chunks.map((c) => decoder.decode(c, { stream: true }));
}

/** Fold `detectSessionState` over a sequence of text pieces from `start`,
 *  applying the hook's exact transition rule (ignore null / unchanged). */
function foldState(pieces: string[], start: SessionState = "idle"): SessionState {
  let state = start;
  for (const text of pieces) {
    const detected = detectSessionState(text, state);
    if (detected && detected !== state) state = detected;
  }
  return state;
}

describe("base64ToBytes", () => {
  it("decodes base64 identically to the instance atob path", () => {
    const original = enc.encode("hello 世界\n");
    const b64 = Buffer.from(original).toString("base64");
    expect(Array.from(base64ToBytes(b64))).toEqual(Array.from(original));
  });
});

describe("TerminalOutputCoalescer — decode parity", () => {
  it("coalesced frame text equals the instance's per-event decoded text, joined", () => {
    // "café" — the é is 0xC3 0xA9; split the chunk BETWEEN those two bytes so a
    // naive per-chunk decode would corrupt it. Both the instance decoder and the
    // tap decoder hold the partial byte across the boundary.
    const s = "Reading café.rs and rendéring…\n";
    const eIdx = enc.encode("Reading caf").length; // byte offset of 0xC3
    const chunks = bytesSplitAt(s, [eIdx + 1]); // split inside the 2-byte é

    const instanceTexts = instanceDecodeTexts(chunks);
    expect(instanceTexts.join("")).toBe(s); // sanity: instance path reconstructs

    const coalescer = new TerminalOutputCoalescer();
    for (const c of chunks) coalescer.push("tab-1", c);
    const drained = coalescer.drain();

    expect(drained).toHaveLength(1);
    const [tabId, coalesced] = drained[0];
    expect(tabId).toBe("tab-1");
    // The load-bearing property: tap-coalesced text is byte-identical to the
    // concatenation of what the instance's onOutput produced per event.
    expect(coalesced).toBe(instanceTexts.join(""));
    expect(coalesced).toBe(s);
  });

  it("holds an incomplete trailing multibyte sequence until its continuation", () => {
    const coalescer = new TerminalOutputCoalescer();
    const eBytes = enc.encode("é"); // [0xC3, 0xA9]
    coalescer.push("t", eBytes.subarray(0, 1)); // just 0xC3 — incomplete
    // First frame: nothing decodable yet, so no staged text at all.
    expect(coalescer.hasPending()).toBe(false);
    coalescer.push("t", eBytes.subarray(1)); // 0xA9 completes it
    const drained = coalescer.drain();
    expect(drained).toEqual([["t", "é"]]);
  });
});

describe("TerminalOutputCoalescer — tracking parity under coalescing", () => {
  it("final SessionState matches line-by-line when frames don't straddle a state flip", () => {
    // Instance path granularity = complete lines (one handleOutput per line).
    // Tap path granularity = one coalesced handleOutput per animation frame.
    // Real runtime never packs a needs-input prompt AND its resolution into the
    // same ~16ms frame, so each frame below holds only same-direction output.
    const frames: string[][] = [
      ["Reading files.rs\n", "Editing main.rs\n"], // working burst
      ["Allow Bash tool? (y/n)\n"], // needs-input
      ["Running command: cargo test\n"], // resumed → working
    ];

    // Line-by-line (instance) fold: every line applied individually.
    const lineByLine = foldState(frames.flat());

    // Tap fold: coalesce each frame, apply once per frame.
    let tapState: SessionState = "idle";
    for (const frameLines of frames) {
      const coalescer = new TerminalOutputCoalescer();
      for (const line of frameLines) coalescer.push("tab", enc.encode(line));
      const [[, coalesced]] = coalescer.drain();
      const detected = detectSessionState(coalesced, tapState);
      if (detected && detected !== tapState) tapState = detected;
    }

    expect(lineByLine).toBe("working");
    expect(tapState).toBe("working");
    expect(tapState).toBe(lineByLine);
  });

  it("coalescing a same-direction working burst is state-identical to per-line", () => {
    const lines = ["Reading a.ts\n", "Reading b.ts\n", "Editing c.ts\n"];
    const perLine = foldState(lines);

    const coalescer = new TerminalOutputCoalescer();
    for (const l of lines) coalescer.push("z", enc.encode(l));
    const [[, coalesced]] = coalescer.drain();
    const coalescedState = foldState([coalesced]);

    expect(perLine).toBe("working");
    expect(coalescedState).toBe(perLine);
  });

  it("preserves total byte-length (activity-sparkline parity)", () => {
    const lines = ["one\n", "two two\n", "three three three\n"];
    const coalescer = new TerminalOutputCoalescer();
    for (const l of lines) coalescer.push("a", enc.encode(l));
    const [[, coalesced]] = coalescer.drain();
    // handleOutput accumulates `text.length` into the activity ring; one
    // coalesced call with the summed length equals N per-line calls.
    expect(coalesced.length).toBe(lines.reduce((n, l) => n + l.length, 0));
  });
});

describe("TerminalOutputCoalescer — routing, ordering, pruning", () => {
  it("keeps per-tab buffers isolated and preserves per-tab arrival order", () => {
    const coalescer = new TerminalOutputCoalescer();
    coalescer.push("a", enc.encode("a1 "));
    coalescer.push("b", enc.encode("b1 "));
    coalescer.push("a", enc.encode("a2"));
    coalescer.push("b", enc.encode("b2"));
    const drained = new Map(coalescer.drain());
    expect(drained.get("a")).toBe("a1 a2");
    expect(drained.get("b")).toBe("b1 b2");
  });

  it("drain empties the pending buffers", () => {
    const coalescer = new TerminalOutputCoalescer();
    coalescer.push("a", enc.encode("x"));
    expect(coalescer.hasPending()).toBe(true);
    coalescer.drain();
    expect(coalescer.hasPending()).toBe(false);
    expect(coalescer.drain()).toEqual([]);
  });

  it("retain() drops decoders + buffers for closed tabs", () => {
    const coalescer = new TerminalOutputCoalescer();
    coalescer.push("keep", enc.encode("k"));
    coalescer.push("drop", enc.encode("d"));
    coalescer.retain(new Set(["keep"]));
    const drained = new Map(coalescer.drain());
    expect(drained.has("keep")).toBe(true);
    expect(drained.has("drop")).toBe(false);
  });
});
