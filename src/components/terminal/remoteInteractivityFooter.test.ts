/**
 * The remote tab's interactivity footer (plan
 * `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
 * A1). Pure: the runner's vitest runs in node, so the component's JSX shell is
 * covered by `tsc --noEmit` and the footer's every state is pinned here.
 */
import { describe, it, expect } from "vitest";
import {
  INPUT_ACK_DEADLINE_MS,
  inputAwaitingAck,
  remoteInteractivityFooter,
  type RemoteInteractivitySnapshot,
} from "./remoteTabs";

const NOW = 1_800_000_000_000;

function snap(over: Partial<RemoteInteractivitySnapshot> = {}): RemoteInteractivitySnapshot {
  return {
    attachedAtMs: NOW - 60_000,
    lastInputSent: null,
    lastInputAcked: null,
    acksReceived: 0,
    acksSinceAttach: 0,
    lastProbeSent: null,
    lastFrameReceived: { atMs: NOW - 3_000, throughOffset: 100 },
    ...over,
  };
}

function acked(
  over: Partial<NonNullable<RemoteInteractivitySnapshot["lastInputAcked"]>> = {},
): NonNullable<RemoteInteractivitySnapshot["lastInputAcked"]> {
  return {
    seq: 1,
    atMs: NOW - 1_000,
    bytes: 1,
    accepted: true,
    error: null,
    via: "traffic",
    targetAcceptedAt: null,
    ...over,
  };
}

describe("remoteInteractivityFooter", () => {
  it("before any input says so and implies nothing", () => {
    const f = remoteInteractivityFooter(snap(), NOW, "merytshost");
    expect(f.summary).toBe("last output received 3s ago · no input sent yet");
    expect(f.note).toBeNull();
  });

  it("an accepted ack reads as accepted, with its age", () => {
    const f = remoteInteractivityFooter(
      snap({
        lastInputSent: { seq: 1, atMs: NOW - 1_200, bytes: 1 },
        lastInputAcked: acked(),
        acksReceived: 1,
      }),
      NOW,
      "merytshost",
    );
    expect(f.summary).toBe("last output received 3s ago · last keystroke accepted 1s ago");
    expect(f.note).toBeNull();
  });

  it("an unacked keystroke inside the deadline is not yet a failure", () => {
    const f = remoteInteractivityFooter(
      snap({
        lastInputSent: { seq: 2, atMs: NOW - (INPUT_ACK_DEADLINE_MS - 1), bytes: 1 },
        lastInputAcked: acked(),
        acksReceived: 1,
      }),
      NOW,
      "merytshost",
    );
    expect(f.note).toBeNull();
  });

  it("an unacked keystroke past the deadline is called out, naming the device", () => {
    const f = remoteInteractivityFooter(
      snap({
        lastInputSent: { seq: 2, atMs: NOW - INPUT_ACK_DEADLINE_MS, bytes: 1 },
        lastInputAcked: acked(),
        acksReceived: 1,
      }),
      NOW,
      "merytshost",
    );
    expect(f.note).toBe("input not acknowledged by merytshost");
    expect(f.noteKind).toBe("warning");
  });

  it("a target that has NEVER acked is an older build, not a failure", () => {
    const f = remoteInteractivityFooter(
      snap({ lastInputSent: { seq: 5, atMs: NOW - 60_000, bytes: 1 }, acksReceived: 0 }),
      NOW,
      "merytshost",
    );
    expect(f.summary).toContain("keystrokes sent, none acknowledged yet");
    expect(f.note).toBe("target does not acknowledge input (older build)");
    expect(f.noteKind).toBe("info");
    expect(f.note).not.toContain("not acknowledged by");
  });

  it("a refused ack shows the target's error", () => {
    const f = remoteInteractivityFooter(
      snap({
        lastInputSent: { seq: 3, atMs: NOW - 2_000, bytes: 1 },
        lastInputAcked: acked({ seq: 3, accepted: false, error: "terminal_exited" }),
        acksReceived: 3,
      }),
      NOW,
      "spaceship",
    );
    expect(f.summary).toContain("last keystroke rejected 1s ago");
    expect(f.note).toBe("input rejected by spaceship: terminal_exited");
    expect(f.noteKind).toBe("warning");
  });

  it("a probe ack is labelled as a probe, not a keystroke", () => {
    const f = remoteInteractivityFooter(
      snap({ lastInputAcked: acked({ via: "probe", bytes: 0 }), acksReceived: 1 }),
      NOW,
      "x",
    );
    expect(f.summary).toContain("input path probed 1s ago");
  });

  it("no frame received yet reads as no output yet", () => {
    const f = remoteInteractivityFooter(snap({ lastFrameReceived: null }), NOW, "x");
    expect(f.summary.startsWith("no output yet")).toBe(true);
  });

  it("ages roll into minutes and hours", () => {
    const f = remoteInteractivityFooter(
      snap({ lastFrameReceived: { atMs: NOW - 125_000, throughOffset: 0 } }),
      NOW,
      "x",
    );
    expect(f.summary).toContain("2m ago");
    const g = remoteInteractivityFooter(
      snap({ lastFrameReceived: { atMs: NOW - 7_300_000, throughOffset: 0 } }),
      NOW,
      "x",
    );
    expect(g.summary).toContain("2h ago");
  });
});

describe("inputAwaitingAck", () => {
  it("compares by seq, and by arrival when the ack carries none", () => {
    const sent = { seq: 4, atMs: NOW - 1_000, bytes: 1 };
    expect(inputAwaitingAck(snap({ lastInputSent: sent }))).toBe(true);
    expect(inputAwaitingAck(snap({ lastInputSent: sent, lastInputAcked: acked({ seq: 4 }) }))).toBe(
      false,
    );
    expect(inputAwaitingAck(snap({ lastInputSent: sent, lastInputAcked: acked({ seq: 3 }) }))).toBe(
      true,
    );
    expect(
      inputAwaitingAck(
        snap({ lastInputSent: sent, lastInputAcked: acked({ seq: null, atMs: NOW - 500 }) }),
      ),
    ).toBe(false);
    expect(inputAwaitingAck(snap())).toBe(false);
  });
});
