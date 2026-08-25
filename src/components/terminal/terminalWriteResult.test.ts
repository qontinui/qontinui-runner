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

  // ── The backend's own refusal (manual-test-loop iter 16) ────────────────
  //
  // `TerminalSession::write` in Rust now refuses a write to an exited PTY.
  // A pane that has not yet processed its `terminal-exit` event calls this
  // with `exit === null`, so without recognising the backend envelope the
  // typed TERMINAL_EXITED diagnosis was downgraded to TERMINAL_WRITE_FAILED
  // — and `resumeVerification` retries TERMINAL_WRITE_FAILED for 31s against
  // a process that is already gone.
  it("classifies the BACKEND's TERMINAL_EXITED refusal even before the exit event lands", () => {
    const backendRefusal = new Error(
      "TERMINAL_EXITED: terminal term-4 is not writable -- its process exited with code 137.",
    );
    const failure = buildWriteFailure("term-4", null, backendRefusal);

    expect(failure.code).toBe(TERMINAL_EXITED);
    expect(failure.exitCode).toBe(137);
    expect(failure.hint).toMatch(/restart the session/i);
  });

  it("reports a null exitCode when the backend refusal says the code is unknown", () => {
    const failure = buildWriteFailure(
      "term-5",
      null,
      new Error("TERMINAL_EXITED: terminal term-5 is not writable -- its process exited with code unknown."),
    );
    expect(failure.code).toBe(TERMINAL_EXITED);
    expect(failure.exitCode).toBeNull();
  });

  // The falsifiable other half: recognising the backend envelope must not
  // swallow every failure into TERMINAL_EXITED. A live pane's IPC failure is
  // still retryable and must stay TERMINAL_WRITE_FAILED.
  it("does NOT promote an unrelated IPC failure to TERMINAL_EXITED", () => {
    const failure = buildWriteFailure("term-6", null, new Error("Terminal not found: term-6"));
    expect(failure.code).toBe(TERMINAL_WRITE_FAILED);
  });

  // A locally-observed exit still wins: its exitCode is authoritative over
  // anything parsed out of the cause string.
  it("prefers the locally observed exit code over the backend text", () => {
    const failure = buildWriteFailure(
      "term-7",
      { exitCode: 2 },
      new Error("TERMINAL_EXITED: ... exited with code 137."),
    );
    expect(failure.code).toBe(TERMINAL_EXITED);
    expect(failure.exitCode).toBe(2);
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
