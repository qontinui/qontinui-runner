/**
 * The pane's write path for live output: coalesce, write, and ack EVERY byte
 * the runner sent — the bytes a ring replay already covered included.
 *
 * Why this exists (the termhost-bench freeze, CI run 38351621760): the Rust
 * reader gates webview emission on `bytes_sent − bytes_acked`. It pauses above
 * 100 000 and reopens only at or below 5 000 (`FLOW_HIGH_WATERMARK` /
 * `FLOW_LOW_WATERMARK` in `src-tauri/src/terminal/session.rs`). The pane acked
 * only bytes it WROTE. Two kinds of received bytes were never written, and so
 * never acked:
 *
 *  - bytes trimmed by `trimReplayedChunk` because a ring replay already put
 *    them in the buffer. A chunk emitted after the ring read reset the counters,
 *    but tee'd into the ring before the snapshot, is counted as sent. A held
 *    DEC-2026 sync frame is the large case: it is up to 256 KiB, and wholly
 *    inside the snapshot.
 *  - chunks buffered before the backend was ready (`pendingBytes`), which the
 *    mount drain wrote straight to the backend with no render callback.
 *
 * Each remount and each resync leaves that remainder on the runner's books.
 * Once it exceeds 5 000, the next burst that trips the gate wedges it. The pane
 * renders and acks everything it was given, the gap settles at the leaked
 * remainder, which is still above the Low watermark, and the runner never sends
 * another byte. No chunk arrives, so no gap is detected, the resume marker
 * never fires, and the pane is frozen while its PTY keeps flowing.
 *
 * The rule this module enforces: every chunk the pane receives (any route but
 * "drop") is acked in full exactly once. The written part is acked when its
 * render callback fires. A chunk with nothing left to write is acked as
 * settled-without-write, in order with the writes around it. Bytes emitted
 * before a reset and acked after it read as a briefly NEGATIVE gap, which the
 * runner saturates to 0. That is the bounded, documented blind margin on
 * `TerminalSession::reset_flow_control`, not a leak.
 *
 * Leaf module (no xterm / Tauri imports), so vitest can drive it, for the same
 * reason as `flowControl.ts`.
 */

import type { RenderAckAccumulator } from "./flowControl";
import { trimReplayedChunk, type OffsetChunk } from "./scrollbackReplay";

/** The one backend capability the queue needs: a write with a render callback. */
export interface RenderedWriteSink {
  write(data: Uint8Array, onRendered: () => void): void;
}

export class AckedWriteQueue {
  private slices: Uint8Array[] = [];
  /** Bytes staged for the next write. */
  private byteLen = 0;
  /** Bytes the runner sent for what is staged: the written part plus any trimmed prefix. */
  private ackLen = 0;

  constructor(
    private readonly acks: RenderAckAccumulator,
    private readonly sendAck: (bytes: number) => void,
  ) {}

  /**
   * Stage one received chunk: trim it against the ring-replay boundary, queue
   * what is left for writing, and owe an ack for ALL of it.
   */
  stageChunk(chunk: OffsetChunk, replayedThrough: number): void {
    const slice = trimReplayedChunk(chunk, replayedThrough);
    this.stage(slice ? [slice] : [], chunk.bytes.length);
  }

  /**
   * Stage already-trimmed slices whose source chunks were `receivedBytes` long
   * in total (`drainHeldChunks` returns both).
   */
  stage(slices: readonly Uint8Array[], receivedBytes: number): void {
    for (const s of slices) {
      if (s.length === 0) continue;
      this.slices.push(s);
      this.byteLen += s.length;
    }
    if (receivedBytes > 0) this.ackLen += receivedBytes;
  }

  /** True when there is nothing to write AND nothing to ack. */
  get isEmpty(): boolean {
    return this.byteLen === 0 && this.ackLen === 0;
  }

  /**
   * Write everything staged as ONE backend write, owing the full ack for it.
   * With nothing to write (every staged byte was already replayed), settle the
   * ack at once. A `null` sink means the pane is gone. Its unmount resets the
   * runner's counters (`terminal_flow_reset`), so the staged bytes are dropped.
   *
   * A sink that throws still settles the ack, since its render callback will
   * never fire, and the error is rethrown for the caller to log.
   */
  flush(sink: RenderedWriteSink | null): void {
    const slices = this.slices;
    const byteLen = this.byteLen;
    const ackLen = this.ackLen;
    this.slices = [];
    this.byteLen = 0;
    this.ackLen = 0;
    if (!sink) return;
    if (byteLen === 0) {
      this.sendAck(this.acks.onSettledWithoutWrite(ackLen));
      return;
    }
    let merged: Uint8Array;
    if (slices.length === 1) {
      merged = slices[0];
    } else {
      merged = new Uint8Array(byteLen);
      let at = 0;
      for (const s of slices) {
        merged.set(s, at);
        at += s.length;
      }
    }
    this.acks.onWriteIssued(ackLen);
    try {
      sink.write(merged, () => {
        this.sendAck(this.acks.onRendered(ackLen));
      });
    } catch (e) {
      this.sendAck(this.acks.onRendered(ackLen));
      throw e;
    }
  }
}
