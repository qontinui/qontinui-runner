/**
 * Frontend half of the terminal-output transport counters (plan
 * `2026-09-20-terminal-output-transport-is-unmeasured-encoded-broadcast`,
 * Phase 1). Exposed on `window.__qontinuiTransportStats`, read (and reset) by
 * `scripts/perf-harness.mjs` through the UI Bridge `page/evaluate` probe, and
 * paired there with the runner's `GET /terminals/transport-stats`.
 *
 * What each counter measures — and where it is fed from:
 *
 *  - `decodeNs.pane` / `decodeNs.tap` (+ `decodeCalls`, `decodeBytes`): wall
 *    time around the two base64 decodes a focused chunk pays in this window —
 *    the pane's inline `atob` loop in `TerminalInstance.tsx` and the page tap's
 *    `base64ToBytes` in `TerminalSessionContext.tsx`. K1's "pane-decode +
 *    tap-decode" numerator.
 *  - `writeToRenderNs` (+ `writeToRenderCount`, `writeToRenderBytes`): from
 *    the coalesced `backend.write(bytes, cb)` in `TerminalInstance.tsx` to its
 *    render callback — K1's denominator (the emulator's parse/render share).
 *  - `eventsDelivered` / `eventsForeign`: every `terminal-output` event this
 *    window's single listener received, and the subset whose terminal no page
 *    roster or pane in THIS window owns (`terminalEventDemux.ts`). A foreign
 *    event is pure broadcast cost: deserialized here, used nowhere.
 *    Ownership is `terminalVisibilityTiers.isOwnedByThisWindow` — a published
 *    roster or a `declarePaneTier` declaration. A pane that has registered its
 *    output handler but not yet declared a tier (the mount window before its
 *    visibility service runs), in a page whose roster has not published the
 *    tab yet, therefore counts its events as FOREIGN: a small over-count
 *    confined to mount, not steady state.
 *  - `ringReplay.{fetches,bytesFetched,bytesWritten}`: at the pane's ring
 *    replay sites (mount replay, emission-gap / reveal resync). Fetched ≈ ring
 *    size per call today; written is the slice the pane actually needed.
 *  - `rawIpc`: always `null` in Phase 1. Phase 4's ranged raw ring read fills
 *    it with the per-window verdict of whether `tauri::ipc::Response` arrived
 *    as an `ArrayBuffer` (`true`) or fell back to the JSON/base64 body
 *    (`false`). `null` means "not probed", never "no".
 *
 * Cost: the counters are plain number adds on module-level state. Timing
 * (`performance.now()`, two calls per timed site) runs only while `enabled`
 * is true, which is the default — a base64 decode of a KB-sized chunk dwarfs
 * two clock reads. `performance.now()` is coarsened in webviews (tens of µs to
 * 1 ms depending on engine), so a per-call ns figure is quantized; the SUM
 * across thousands of chunks is what the harness reports. Set
 * `window.__qontinuiTransportStats.enabled = false` to drop the clock reads.
 *
 * Kept a leaf module (no React / Tauri imports) so hot components can import it
 * without pulling anything else in.
 */

export interface TransportStats {
  /** Gates the `performance.now()` reads. Counters that are plain adds always run. */
  enabled: boolean;
  /** `performance.now()` at the last reset (or module load). */
  resetAt: number;
  decodeNs: { pane: number; tap: number };
  decodeCalls: { pane: number; tap: number };
  decodeBytes: { pane: number; tap: number };
  writeToRenderNs: number;
  writeToRenderCount: number;
  writeToRenderBytes: number;
  eventsDelivered: number;
  eventsForeign: number;
  ringReplay: { fetches: number; bytesFetched: number; bytesWritten: number };
  /** Phase 4 fills this; `null` = not probed. See the module doc. */
  rawIpc: boolean | null;
  /** Zero every counter and restart the window. `enabled` and `rawIpc` are kept. */
  reset(): void;
}

const now = (): number =>
  typeof performance !== "undefined" && typeof performance.now === "function"
    ? performance.now()
    : Date.now();

function createStats(): TransportStats {
  const s: TransportStats = {
    enabled: true,
    resetAt: now(),
    decodeNs: { pane: 0, tap: 0 },
    decodeCalls: { pane: 0, tap: 0 },
    decodeBytes: { pane: 0, tap: 0 },
    writeToRenderNs: 0,
    writeToRenderCount: 0,
    writeToRenderBytes: 0,
    eventsDelivered: 0,
    eventsForeign: 0,
    ringReplay: { fetches: 0, bytesFetched: 0, bytesWritten: 0 },
    rawIpc: null,
    reset() {
      s.resetAt = now();
      s.decodeNs.pane = 0;
      s.decodeNs.tap = 0;
      s.decodeCalls.pane = 0;
      s.decodeCalls.tap = 0;
      s.decodeBytes.pane = 0;
      s.decodeBytes.tap = 0;
      s.writeToRenderNs = 0;
      s.writeToRenderCount = 0;
      s.writeToRenderBytes = 0;
      s.eventsDelivered = 0;
      s.eventsForeign = 0;
      s.ringReplay.fetches = 0;
      s.ringReplay.bytesFetched = 0;
      s.ringReplay.bytesWritten = 0;
    },
  };
  return s;
}

declare global {
  interface Window {
    __qontinuiTransportStats?: TransportStats;
  }
}

/**
 * The window's one stats object. Reuses an existing `window` instance (an HMR
 * reload of this module must not orphan the object the harness holds).
 */
export const transportStats: TransportStats = (() => {
  if (typeof window === "undefined") return createStats();
  if (!window.__qontinuiTransportStats) window.__qontinuiTransportStats = createStats();
  return window.__qontinuiTransportStats;
})();

/** Start a timed section; returns 0 (and costs nothing) while disabled. */
export function transportClockStart(): number {
  return transportStats.enabled ? now() : 0;
}

/** Nanoseconds since `start`, or 0 when timing was disabled at `start`. */
export function transportElapsedNs(start: number): number {
  return start === 0 ? 0 : (now() - start) * 1e6;
}

/** Record one base64 decode at the pane (`"pane"`) or page-tap (`"tap"`) site. */
export function noteDecode(site: "pane" | "tap", start: number, bytes: number): void {
  transportStats.decodeNs[site] += transportElapsedNs(start);
  transportStats.decodeCalls[site] += 1;
  transportStats.decodeBytes[site] += bytes;
}

/** Record one `backend.write` → render-callback interval. */
export function noteWriteRendered(start: number, bytes: number): void {
  transportStats.writeToRenderNs += transportElapsedNs(start);
  transportStats.writeToRenderCount += 1;
  transportStats.writeToRenderBytes += bytes;
}

/** Record one `terminal-output` event received by this window. */
export function noteOutputEvent(foreign: boolean): void {
  transportStats.eventsDelivered += 1;
  if (foreign) transportStats.eventsForeign += 1;
}

/** Record one ring replay: what the IPC fetched vs what the pane wrote. */
export function noteRingReplay(bytesFetched: number, bytesWritten: number): void {
  transportStats.ringReplay.fetches += 1;
  transportStats.ringReplay.bytesFetched += bytesFetched;
  transportStats.ringReplay.bytesWritten += bytesWritten;
}
