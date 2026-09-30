/**
 * Transport-cost bench, TypeScript half of plan
 * `2026-09-20-terminal-output-transport-is-unmeasured-encoded-broadcast`
 * Phase 1: what the webview pays to turn one delivered frame into bytes.
 *
 *   npx vitest bench --run src/components/terminal/transportCost.bench.ts
 *
 * Per corpus set, one bench iteration processes EVERY frame in the set:
 *  - `pane decode (1x)`      — `base64ToBytes` once: the pane's decode in
 *                              `TerminalInstance.tsx` `handleOutputPayload`
 *                              (the one-decode target of Phase 2);
 *  - `pane + tap decode (2x)` — `base64ToBytes` twice: the pane's decode plus
 *                              the page tap's (`TerminalSessionContext.tsx`) —
 *                              what a focused chunk costs today;
 *  - `Uint8Array view`       — `new Uint8Array(buffer, off, len)` over one
 *                              concatenated buffer: the raw-IPC shape (Phase 6).
 *
 * Encoding happens once, in setup (module scope), never inside a timed body.
 *
 * The Rust bench (`src-tauri/src/terminal/transport_cost.rs`) uses a different
 * corpus (see `transportCorpus.ts`), so compare the two per byte, not per frame.
 */

import { bench, describe } from "vitest";

import { base64ToBytes } from "./terminalOutputTap";
import { buildTransportCorpus } from "./transportCorpus";

const corpus = buildTransportCorpus();

/** Keeps the JIT from discarding the decoded arrays. */
let sink = 0;

for (const set of corpus) {
  const frameCount = set.frames.length;
  describe(`${set.name} (${frameCount} frames, ${set.totalBytes} bytes)`, () => {
    bench("pane decode (1x)", () => {
      for (const b64 of set.encoded) sink ^= base64ToBytes(b64).length;
    });

    bench("pane + tap decode (2x)", () => {
      for (const b64 of set.encoded) {
        sink ^= base64ToBytes(b64).length; // pane
        sink ^= base64ToBytes(b64).length; // tap
      }
    });

    bench("Uint8Array view", () => {
      for (let i = 0; i < frameCount; i++) {
        sink ^= new Uint8Array(set.buffer, set.offsets[i], set.frames[i].length).length;
      }
    });
  });
}

export const __benchSink = () => sink;
