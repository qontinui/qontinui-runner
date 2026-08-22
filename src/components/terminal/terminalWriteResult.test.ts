/**
 * Tests for the PTY-write result envelope (B5).
 *
 * THE DEFECT these cover: every `invoke("terminal_write", …)` in
 * `TerminalInstance` ended in `.catch(() => {})`. A write to a terminal whose
 * process had exited reported nothing — the imperative handle returned `void`
 * and the UI Bridge `writeToTerminal` / `sendKeys` custom actions resolved,
 * which the SDK executor reports as `success: true`. Input that reached no
 * process came back green.
 */

import { describe, expect, it } from "vitest";

import {
  buildWriteFailure,
  throwIfWriteFailed,
  TERMINAL_EXITED,
  TERMINAL_WRITE_FAILED,
  type TerminalWriteResult,
} from "./terminalWriteResult";

describe("buildWriteFailure", () => {
  it("names the exit and points at the restart affordance", () => {
    const failure = buildWriteFailure("term-7", { exitCode: 1 }, null);
    expect(failure.success).toBe(false);
    expect(failure.code).toBe(TERMINAL_EXITED);
    expect(failure.exitCode).toBe(1);
    expect(failure.error).toContain("term-7");
    expect(failure.error).toContain("exited with code 1");
    expect(failure.hint).toMatch(/restart/i);
  });

  it("still reports an exit whose code was never observed", () => {
    // `null` is "the PTY is gone but nobody recorded a code" — it must not
    // read as a successful exit, and it must not blank the message.
    const failure = buildWriteFailure("term-8", { exitCode: null }, null);
    expect(failure.error).toContain("unknown");
  });

  it("distinguishes an IPC failure on a LIVE pane from a dead process", () => {
    // Different recovery: retry / read the log, versus restart the session.
    const failure = buildWriteFailure("term-9", null, new Error("ipc closed"));
    expect(failure.code).toBe(TERMINAL_WRITE_FAILED);
    expect(failure.error).toContain("ipc closed");
    expect(failure.hint).not.toMatch(/restart the session/i);
  });

  it("stringifies a non-Error cause instead of dropping it", () => {
    expect(buildWriteFailure("t", null, "plain string boom").error).toContain(
      "plain string boom",
    );
  });
});

describe("throwIfWriteFailed", () => {
  it("passes a success through untouched", () => {
    const ok: TerminalWriteResult = { success: true, bytes: 3 };
    expect(throwIfWriteFailed(ok)).toBe(ok);
  });

  it("THROWS on failure — the only signal the SDK executor reports as success:false", () => {
    // `executeAction` treats any RESOLVED custom-action handler as a success
    // regardless of what it resolved with, so returning the envelope would
    // still surface `success: true` with the failure buried in `result`.
    const failure = buildWriteFailure("term-1", { exitCode: 137 }, null);
    let thrown: unknown;
    try {
      throwIfWriteFailed(failure);
    } catch (err) {
      thrown = err;
    }
    expect(thrown).toBeInstanceOf(Error);
    expect((thrown as Error).message).toContain(TERMINAL_EXITED);
    expect((thrown as Error & { code?: string }).code).toBe(TERMINAL_EXITED);
    expect((thrown as Error & { exitCode?: number }).exitCode).toBe(137);
  });
});
