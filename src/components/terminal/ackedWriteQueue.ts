/**
 * The pane's write path for live output: coalesce, write, and ack EVERY byte
 * the runner sent — the bytes a ring replay already covered included.
 *
 * Why this exists (a pane that stops repainting while its PTY flows): the Rust
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
 *  - chunks buffered before the backend was ready, which the mount drain
 *    wrote straight to the backend with no render callback.
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
 * settled-without-write, queued behind any write still waiting for its render
 * callback, so acks leave in the order the bytes were received.
 *
 * Acking a byte the runner has already written off is harmless. Excess acks
 * come from bytes sent before a counter reset (`terminal_get_scrollback`,
 * `terminal_flow_reset`, the reconnect snapshot) and acked after it, from an
 * uncounted background hold window (drained ahead of a counted send, or
 * flushed by the visibility sweeper) that a now-visible pane writes, and from
 * settled-without-write acks for either. `TerminalSession::ack` clamps
 * `acked` at `sent`, which discards the excess: no surplus can build up, so
 * none carries into the next burst or compounds across resets. Excess can
 * still land on bytes sent after a reset that the pane has not acked yet, so
 * the gap reads low for a while: by at most what the pane held unacked when
 * the reset ran (High plus one emitted chunk) plus one hold window per tier
 * edge, until the next reset. That is a delay before backpressure, never a
 * frozen pane, PROVIDED every counted byte is acked once after it is counted.
 * The cases that broke that proviso are closed on the runner side (see
 * `TerminalSession::reset_flow_control`): an ack landing before its bytes
 * were counted, a reset erasing a concurrent ack, a reset straddling a failed
 * emit's un-count, and bytes counted after a reset and emitted before the
 * pane's runner-side listener existed (the reconnect path now resets at its
 * grid snapshot, once that listener is live).
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

/** A write whose render callback has not fired, and the settled acks queued behind it. */
interface UnrenderedWrite {
  trailing: number;
}

export class AckedWriteQueue {
  private slices: Uint8Array[] = [];
  /** Bytes staged for the next write. */
  private byteLen = 0;
  /** Bytes the runner sent for what is staged: the written part plus any trimmed prefix. */
  private ackLen = 0;
  /** The newest write still waiting for its render callback, if any. */
  private lastUnrendered: UnrenderedWrite | null = null;

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
   * ack: at once when no write is waiting to render, otherwise behind the
   * newest such write, so acks leave in receipt order. A `null` sink means the
   * pane is gone. Its unmount resets the runner's counters
   * (`terminal_flow_reset`), so the staged bytes are dropped.
   *
   * A sink that throws still settles the ack, since its render callback will
   * never fire, and the error is rethrown for the caller to log. A render
   * callback settles at most once, even if it fires and the sink then throws.
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
      if (this.lastUnrendered) {
        this.lastUnrendered.trailing += ackLen;
      } else {
        this.sendAck(this.acks.onSettledWithoutWrite(ackLen));
      }
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
    const entry: UnrenderedWrite = { trailing: 0 };
    this.lastUnrendered = entry;
    let settled = false;
    const settle = () => {
      if (settled) return;
      settled = true;
      if (this.lastUnrendered === entry) this.lastUnrendered = null;
      const rendered = this.acks.onRendered(ackLen);
      this.sendAck(rendered + this.acks.onSettledWithoutWrite(entry.trailing));
    };
    this.acks.onWriteIssued(ackLen);
    try {
      sink.write(merged, settle);
    } catch (e) {
      settle();
      throw e;
    }
  }
}
