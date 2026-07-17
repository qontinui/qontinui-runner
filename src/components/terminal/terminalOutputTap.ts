/**
 * Pure, DOM-free helpers for the global `terminal-output` tap
 * (plan `2026-06-30-terminal-flow-grid-virtualization.md` §Phase 2).
 *
 * Session-state tracking used to be driven entirely by `onOutput` callbacks
 * fired from *mounted* `TerminalInstance` components. Phase 3 unmounts
 * offscreen instances, which would make those sessions state-blind. Phase 2
 * moves the tracking feed to ONE always-mounted `terminal-output` listener at
 * the session-provider level (see `TerminalSessionContext.PageSessionScope`).
 *
 * This module is the testable core of that tap:
 *
 *   - `base64ToBytes` mirrors `TerminalInstance`'s base64 → `Uint8Array`
 *     decode of the event payload exactly.
 *   - `TerminalOutputCoalescer` decodes each raw chunk with ONE streaming
 *     `TextDecoder` per terminalId — matching each `TerminalInstance`'s own
 *     per-instance `outputDecoderRef` byte-for-byte (partial multibyte
 *     sequences carry across chunk boundaries) — and coalesces the decoded
 *     text per tab so the frame flush calls `handleOutput` ONCE per tab per
 *     animation frame instead of once per raw event.
 *
 * The OOM lesson from incident #532 (unthrottled per-output work in the
 * renderer amplifies memory pressure) is why the coalescer exists: the caller
 * `push()`es on every event but only `drain()`s once per `requestAnimationFrame`.
 *
 * Kept as a leaf module (zero React / Tauri imports) so it unit-tests under
 * the runner's `environment: "node"` vitest config, following the
 * `consumeInputChunk.ts` precedent.
 */

/**
 * Decode a base64 payload to raw bytes, identical to `TerminalInstance`'s
 * inline `atob` + charCode loop. Kept in lock-step so the tap sees the exact
 * same bytes the instance's xterm-write path sees.
 */
export function base64ToBytes(b64: string): Uint8Array {
  const raw = atob(b64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) {
    bytes[i] = raw.charCodeAt(i);
  }
  return bytes;
}

/**
 * Per-terminal streaming decode + per-frame coalescing.
 *
 * Ordering is preserved per tab (chunks are appended in arrival order), and
 * the streaming decoder holds back an incomplete trailing multibyte sequence
 * until its continuation bytes arrive — exactly as `TerminalInstance`'s
 * per-instance `TextDecoder({ stream: true })` does. Concatenating the
 * per-chunk decoded strings therefore yields byte-identical text to what the
 * instance's `onOutput` prop produced for the same event sequence.
 */
export class TerminalOutputCoalescer {
  private readonly decoders = new Map<string, TextDecoder>();
  private readonly buffers = new Map<string, string>();

  /**
   * Decode one raw chunk for `terminalId` and stage its text on that tab's
   * pending-frame buffer. Decode errors are swallowed (matching the instance's
   * `try { … } catch { /* ignore decode errors *\/ }`). Empty decodes (an
   * incomplete multibyte sequence with no completed code points yet) stage
   * nothing — they contribute no text to the coalesced frame.
   */
  push(terminalId: string, bytes: Uint8Array): void {
    let decoder = this.decoders.get(terminalId);
    if (!decoder) {
      decoder = new TextDecoder();
      this.decoders.set(terminalId, decoder);
    }
    let text: string;
    try {
      text = decoder.decode(bytes, { stream: true });
    } catch {
      return;
    }
    if (!text) return;
    this.buffers.set(terminalId, (this.buffers.get(terminalId) ?? "") + text);
  }

  /** True when at least one tab has staged text awaiting a flush. */
  hasPending(): boolean {
    return this.buffers.size > 0;
  }

  /**
   * Drain every tab's staged text and clear the pending buffers. Returns
   * `[terminalId, coalescedText]` pairs in tab-insertion order; per-tab
   * tracking is independent, so cross-tab order is irrelevant.
   */
  drain(): Array<[string, string]> {
    const out: Array<[string, string]> = [];
    for (const [id, text] of this.buffers) {
      if (text) out.push([id, text]);
    }
    this.buffers.clear();
    return out;
  }

  /**
   * Forget the decoder + pending buffer for a closed tab, so a long session
   * that opens and closes many terminals doesn't accumulate dead decoders.
   * Any tab id NOT in `keep` is dropped.
   */
  retain(keep: Set<string>): void {
    for (const id of this.decoders.keys()) {
      if (!keep.has(id)) this.decoders.delete(id);
    }
    for (const id of this.buffers.keys()) {
      if (!keep.has(id)) this.buffers.delete(id);
    }
  }
}
