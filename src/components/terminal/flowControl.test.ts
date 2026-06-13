/**
 * Pure-logic tests for the render-based ack accumulator extracted from
 * `TerminalInstance`'s flow-control path. See `flowControl.ts` for why the
 * accounting lives in a leaf module (TerminalInstance can't be imported under
 * Node — it pulls `@xterm/addon-canvas`).
 */

import { describe, it, expect } from "vitest";
import { RenderAckAccumulator, ACK_SIZE } from "./flowControl";

describe("RenderAckAccumulator", () => {
  it("does not ack before AckSize rendered bytes accumulate", () => {
    const acc = new RenderAckAccumulator(ACK_SIZE);
    acc.onWriteIssued(3000);
    expect(acc.onRendered(3000)).toBe(0);
    expect(acc.pending).toBe(3000);
    expect(acc.outstanding).toBe(0);
  });

  it("fires an ack of the accumulated total once rendered bytes cross AckSize", () => {
    const acc = new RenderAckAccumulator(ACK_SIZE);
    acc.onWriteIssued(3000);
    acc.onWriteIssued(3000);
    expect(acc.onRendered(3000)).toBe(0); // 3000 < 5000
    // 6000 >= AckSize → ack the full pending accumulator (6000), then reset.
    expect(acc.onRendered(3000)).toBe(6000);
    expect(acc.pending).toBe(0);
  });

  it("acks at exactly AckSize", () => {
    const acc = new RenderAckAccumulator(ACK_SIZE);
    acc.onWriteIssued(ACK_SIZE);
    expect(acc.onRendered(ACK_SIZE)).toBe(ACK_SIZE);
  });

  it("tracks in-flight bytes between issue and render", () => {
    const acc = new RenderAckAccumulator(ACK_SIZE);
    acc.onWriteIssued(4000);
    expect(acc.outstanding).toBe(4000);
    expect(acc.pending).toBe(0);
    acc.onRendered(4000);
    expect(acc.outstanding).toBe(0);
    expect(acc.pending).toBe(4000);
  });

  it("flush() returns the sub-AckSize trailing remainder (timer floor)", () => {
    const acc = new RenderAckAccumulator(ACK_SIZE);
    acc.onWriteIssued(1234);
    expect(acc.onRendered(1234)).toBe(0); // below AckSize, no auto-ack
    expect(acc.flush()).toBe(1234); // floor timer drains the remainder
    expect(acc.flush()).toBe(0); // nothing left
  });

  it("clamps a double-fired render callback so it cannot over-ack", () => {
    const acc = new RenderAckAccumulator(ACK_SIZE);
    acc.onWriteIssued(2000);
    acc.onRendered(2000);
    // A spurious second callback for the same write: nothing in flight to
    // settle, so it adds nothing.
    expect(acc.onRendered(2000)).toBe(0);
    expect(acc.pending).toBe(2000);
  });

  it("a single coalesced write accounts its full byte length", () => {
    const acc = new RenderAckAccumulator(ACK_SIZE);
    // Phase 4: many terminal-output events are coalesced into one backend
    // write; the onRendered callback fires once for the whole coalesced length.
    acc.onWriteIssued(8000);
    expect(acc.onRendered(8000)).toBe(8000);
  });
});
