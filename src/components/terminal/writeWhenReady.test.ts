/**
 * Tests for `writeWhenReady`'s RESULT contract — manual-test-loop iteration 10,
 * item 2.
 *
 * The defect: this helper returned `void` and dropped the
 * `Promise<TerminalWriteResult>` `writeToTerminal` resolves, so a
 * `TERMINAL_EXITED` / `TERMINAL_WRITE_FAILED` write was indistinguishable from
 * one that landed. vitest runs `environment: "node"`, so the refs map is a
 * hand-built double rather than a mounted `TerminalInstance`.
 */

import { describe, it, expect, vi } from "vitest";
import { writeWhenReady } from "./writeWhenReady";
import { TERMINAL_EXITED, TERMINAL_WRITE_FAILED } from "./terminalWriteResult";
import type { TerminalWriteResult } from "./terminalWriteResult";

const refs = (result: TerminalWriteResult | (() => Promise<TerminalWriteResult>)) =>
  new Map([
    [
      "tab-1",
      {
        current: {
          writeToTerminal: typeof result === "function" ? result : async () => result,
        },
      },
    ],
  ]) as never;

describe("writeWhenReady", () => {
  it("resolves the handle's OK envelope", async () => {
    const out = await writeWhenReady(refs({ success: true, bytes: 7 }), "tab-1", "x");
    expect(out).toEqual({ success: true, bytes: 7 });
  });

  it("propagates a TERMINAL_EXITED refusal instead of swallowing it", async () => {
    const failure: TerminalWriteResult = {
      success: false,
      code: TERMINAL_EXITED,
      error: "TERMINAL_EXITED: gone",
      hint: "restart it",
      terminalId: "tab-1",
      exitCode: 1,
    };
    const out = await writeWhenReady(refs(failure), "tab-1", "x");
    expect(out).toEqual(failure);
  });

  it("a ref that never becomes ready resolves the SAME typed envelope, not void", async () => {
    const onTimeout = vi.fn();
    const out = await writeWhenReady(new Map() as never, "ghost", "x", {
      maxWaitMs: 0,
      onTimeout,
    });
    expect(out.success).toBe(false);
    if (out.success) throw new Error("unreachable");
    expect(out.code).toBe(TERMINAL_WRITE_FAILED);
    expect(out.terminalId).toBe("ghost");
    expect(out.error).toContain("never became ready");
    // `onTimeout` is still called — it is additive now, not the only signal.
    expect(onTimeout).toHaveBeenCalledWith("ghost");
  });

  it("a THROWING handle becomes a typed envelope, never an unhandled rejection", async () => {
    const out = await writeWhenReady(
      refs(async () => {
        throw new Error("boom");
      }),
      "tab-1",
      "x",
    );
    expect(out.success).toBe(false);
    if (out.success) throw new Error("unreachable");
    expect(out.code).toBe(TERMINAL_WRITE_FAILED);
    expect(out.error).toContain("boom");
  });
});
