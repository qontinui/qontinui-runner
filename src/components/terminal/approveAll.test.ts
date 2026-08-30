/**
 * `/approve-all` counts what it DELIVERED.
 *
 * The defect this file exists to keep dead: the handler wrote through
 * `terminalRefs.get(id)?.current?.writeToTerminal("y\r")` and returned the
 * number of tabs in `needs-input` — so with no `TerminalInstance` mounted it
 * reported "approved 3" having reached no process at all, on the action the
 * code's own comment calls the most irreversible on the page.
 *
 * Runs under `environment: "node"`: `deliverApprovals` takes the ref map and
 * the by-id writer as arguments, so no PTY, no xterm and no React are needed
 * — the same leaf-module split as `terminalWriteResult.ts`.
 */

import { describe, expect, it, vi } from "vitest";

import {
  deliverApprovals,
  TERMINAL_WRITE_UNREPORTED,
  type ApprovalRefs,
  type ApprovalWriteTarget,
  type WriteById,
} from "./approveAll";
import { TERMINAL_EXITED, type TerminalWriteResult } from "./terminalWriteResult";

type Refs = ApprovalRefs;

/** A mounted pane whose write answers with the given envelope. */
function mounted(result: TerminalWriteResult | undefined): { current: ApprovalWriteTarget | null } {
  return { current: { writeToTerminal: vi.fn(async () => result) } };
}

const okEnvelope: TerminalWriteResult = { success: true, bytes: 2 };
const exitedEnvelope: TerminalWriteResult = {
  success: false,
  code: TERMINAL_EXITED,
  error: "process exited",
  hint: "restart the session",
  terminalId: "t",
  exitCode: 1,
};

/** A by-id writer that records what it was asked to send. */
function recordingWriteById(result: TerminalWriteResult): {
  write: WriteById;
  wire: Array<[string, string]>;
} {
  const wire: Array<[string, string]> = [];
  const write: WriteById = async (terminalId, text) => {
    wire.push([terminalId, text]);
    return result;
  };
  return { write, wire };
}

describe("deliverApprovals — the count is deliveries, not intentions", () => {
  it("THE ZERO-REFS CASE: no mounted pane still reaches the PTY by id", async () => {
    // The exact fixture the old code silently skipped. There is no handle, so
    // the optional chain used to short-circuit and the tab was counted anyway.
    const { write, wire } = recordingWriteById(okEnvelope);
    const report = await deliverApprovals(["tab-a", "tab-b"], new Map() as Refs, "y\r", {
      writeById: write,
    });
    // It went to the wire for BOTH panes, in tab order, with the exact bytes.
    expect(wire).toEqual([
      ["tab-a", "y\r"],
      ["tab-b", "y\r"],
    ]);
    expect(report).toMatchObject({ targeted: 2, delivered: 2 });
    expect(report.deliveries.map((d) => d.route)).toEqual(["by-id", "by-id"]);
  });

  it("counts ZERO when the by-id write fails, and says why", async () => {
    const { write } = recordingWriteById(exitedEnvelope);
    const report = await deliverApprovals(["tab-a"], new Map() as Refs, "y\r", {
      writeById: write,
    });
    expect(report.targeted).toBe(1);
    expect(report.delivered).toBe(0);
    expect(report.deliveries[0]).toMatchObject({
      delivered: false,
      code: TERMINAL_EXITED,
      route: "by-id",
    });
  });

  it("prefers a mounted handle and reads ITS envelope", async () => {
    const refs: Refs = new Map([["tab-a", mounted(okEnvelope)]]);
    const { write, wire } = recordingWriteById(okEnvelope);
    const report = await deliverApprovals(["tab-a"], refs, "y\r", { writeById: write });
    expect(report.delivered).toBe(1);
    expect(report.deliveries[0].route).toBe("mounted");
    // The by-id path is NOT also used — one keystroke, one pane.
    expect(wire).toEqual([]);
  });

  it("a PARTIAL delivery reports both numbers rather than rounding either way", async () => {
    const refs: Refs = new Map([
      ["live", mounted(okEnvelope)],
      ["dead", mounted(exitedEnvelope)],
    ]);
    const report = await deliverApprovals(["live", "dead"], refs, "y\r", {
      writeById: recordingWriteById(okEnvelope).write,
    });
    expect(report.targeted).toBe(2);
    expect(report.delivered).toBe(1);
  });

  it("an UNREPORTED write is a failure, and is NEVER retried down the by-id path", async () => {
    // A handle from a build predating the envelope. It may well have written;
    // we do not know. Counting it would be the assertion this module deletes,
    // and re-sending `y\r` could answer a prompt nobody read — so it is
    // neither counted nor retried.
    const refs: Refs = new Map([["tab-a", mounted(undefined)]]);
    const { write, wire } = recordingWriteById(okEnvelope);
    const report = await deliverApprovals(["tab-a"], refs, "y\r", { writeById: write });
    expect(report.delivered).toBe(0);
    expect(report.deliveries[0].code).toBe(TERMINAL_WRITE_UNREPORTED);
    expect(wire).toEqual([]);
  });

  it("one throwing pane does not cap the approval at the panes before it", async () => {
    const throwing: { current: ApprovalWriteTarget | null } = {
      current: {
        writeToTerminal: vi.fn(async () => {
          throw new Error("boom");
        }),
      },
    };
    const refs: Refs = new Map([
      ["a", mounted(okEnvelope)],
      ["b", throwing],
      ["c", mounted(okEnvelope)],
    ]);
    const report = await deliverApprovals(["a", "b", "c"], refs, "y\r");
    expect(report.targeted).toBe(3);
    expect(report.delivered).toBe(2);
    expect(report.deliveries[1]).toMatchObject({ delivered: false, code: "TERMINAL_WRITE_THREW" });
  });

  it("an EMPTY target list is an honest zero, not an error", async () => {
    // `/approve-all` with nothing waiting. Legitimate, and it must report a
    // zero the status line can render as a no-op.
    const report = await deliverApprovals([], new Map() as Refs, "y\r");
    expect(report).toEqual({ targeted: 0, delivered: 0, deliveries: [] });
  });

  it("refuses a pane already known dead BEFORE the IPC", async () => {
    // `exitOf` lets the caller hand in the tab's own liveness so a write to a
    // dead pane is not reported as an IPC failure — the two have different
    // recoveries.
    const seen: Array<{ exitCode: number | null } | null> = [];
    const write: WriteById = async (_id, _text, exit) => {
      seen.push(exit);
      return exitedEnvelope;
    };
    await deliverApprovals(["gone"], new Map() as Refs, "y\r", {
      writeById: write,
      exitOf: () => ({ exitCode: 137 }),
    });
    expect(seen).toEqual([{ exitCode: 137 }]);
  });
});
