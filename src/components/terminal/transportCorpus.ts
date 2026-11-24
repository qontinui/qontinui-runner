/**
 * Deterministic terminal-output corpus for the transport-cost measurement
 * (plan `2026-09-20-terminal-output-transport-is-unmeasured-encoded-broadcast`,
 * Phase 1): synchronized full-screen SGR-heavy repaints at 120×40, 200×60 and
 * 250×80, plus a small-write typing trace.
 *
 * The frames come from `scripts/tui-repaint-generator.mjs` — the same builder
 * the perf harness runs as live load — so the bench and the harness measure
 * one shape. This module only encodes them the way the runner's wire does
 * (UTF-8 bytes, then standard base64) and lays them out for the view-extraction
 * arm (one `ArrayBuffer`, frames back to back).
 *
 * NOT the Rust bench's corpus. `src-tauri/src/terminal/transport_cost.rs`
 * builds its own (xorshift PRNG, 24 frames per set, 256-colour SGR), while
 * this one is the generator's (LCG, 8 frames per set, truecolour SGR). The two
 * languages therefore time DIFFERENT bytes: compare them per byte (ns/B), never
 * as raw ns per frame.
 *
 * Test/bench-only: nothing in the app imports it.
 */

import { buildCorpus } from "../../../scripts/tui-repaint-generator.mjs";

export interface TransportCorpusSet {
  name: string;
  /** Raw PTY bytes per frame. */
  frames: Uint8Array[];
  /** `frames[i]` as the runner's wire carries it (standard base64). */
  encoded: string[];
  /** All frames back to back — the raw-IPC arm's single `ArrayBuffer`. */
  buffer: ArrayBuffer;
  /** Byte offset of each frame in `buffer`. */
  offsets: number[];
  totalBytes: number;
}

/** Standard base64 of `bytes`, byte-identical to Rust's `STANDARD.encode`. */
export function bytesToBase64(bytes: Uint8Array): string {
  let bin = "";
  // Chunked so a 100 KiB frame never builds a giant argument list.
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

/** Build the corpus: every set encoded once, up front (the bench's setup). */
export function buildTransportCorpus(framesPerSet = 8): TransportCorpusSet[] {
  const encoder = new TextEncoder();
  return buildCorpus({ framesPerSet }).map(({ name, frames: text }) => {
    const frames = text.map((t) => encoder.encode(t));
    const totalBytes = frames.reduce((n, f) => n + f.length, 0);
    const joined = new Uint8Array(totalBytes);
    const offsets: number[] = [];
    let at = 0;
    for (const f of frames) {
      offsets.push(at);
      joined.set(f, at);
      at += f.length;
    }
    return {
      name,
      frames,
      encoded: frames.map(bytesToBase64),
      buffer: joined.buffer,
      offsets,
      totalBytes,
    };
  });
}
