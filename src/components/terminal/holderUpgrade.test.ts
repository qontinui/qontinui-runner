import { describe, expect, it } from "vitest";
import {
  END_AND_RESUME_MISSING_NOTE,
  UPGRADE_UNSUPPORTED_NOTE,
  attentionLabel,
  bannerHeadline,
  describeOutcome,
  holderActions,
  invokeErrorText,
  type AttentionPane,
} from "./holderUpgrade";

describe("holderActions", () => {
  it("offers Upgrade holder only where an upgrade is supported", () => {
    const a = holderActions({ upgradeSupported: true, endAndResumeAvailable: false });
    expect(a.upgrade).toBe(true);
    expect(a.leaveRunning).toBe(true);
    expect(a.endAndResume).toBe(false);
    expect(a.note).toBeNull();
  });

  it("offers Leave running, and says why End and resume is missing, where unsupported", () => {
    const a = holderActions({ upgradeSupported: false, endAndResumeAvailable: false });
    expect(a.upgrade).toBe(false);
    expect(a.leaveRunning).toBe(true);
    expect(a.endAndResume).toBe(false);
    expect(a.note).toContain(UPGRADE_UNSUPPORTED_NOTE);
    expect(a.note).toContain(END_AND_RESUME_MISSING_NOTE);
  });

  it("offers End and resume where the resume path exists", () => {
    const a = holderActions({ upgradeSupported: false, endAndResumeAvailable: true });
    expect(a.upgrade).toBe(false);
    expect(a.endAndResume).toBe(true);
    expect(a.note).toBe(UPGRADE_UNSUPPORTED_NOTE);
  });
});

describe("attentionLabel", () => {
  const base: AttentionPane = {
    paneId: "p",
    reason: "incompatible",
    holderPid: 1,
    childPid: 2,
    holderBuild: null,
    holderVersions: [7, 8],
    detail: null,
  };
  it("names an incompatible holder's versions", () => {
    expect(attentionLabel(base, "qontinui-pty-holder 0.2.0")).toBe(
      "Incompatible holder (speaks protocol 7, 8) — this runner cannot drive it",
    );
  });
  it("names an older build against the runner's", () => {
    expect(
      attentionLabel({ ...base, reason: "older_build", holderBuild: "h 0.1.0" }, "h 0.2.0"),
    ).toBe("Older holder build h 0.1.0 — this runner ships h 0.2.0");
  });
});

describe("describeOutcome", () => {
  it("upgraded onto a newer build is ok", () => {
    const d = describeOutcome({
      kind: "upgraded",
      successorPid: 42,
      handoffOffset: 9,
      previousBuild: "h 0.1.0",
      successorBuild: "h 0.2.0",
    });
    expect(d.tone).toBe("ok");
    expect(d.text).toContain("holder pid 42");
    expect(d.text).toContain("It runs h 0.2.0.");
  });

  it("upgraded onto the SAME build is a warning that says so", () => {
    const d = describeOutcome({
      kind: "upgraded",
      successorPid: 42,
      handoffOffset: 9,
      previousBuild: "h 0.1.0",
      successorBuild: "h 0.1.0",
    });
    expect(d.tone).toBe("warn");
    expect(d.text).toContain("SAME build");
  });

  it("an unread successor build is unknown, not claimed", () => {
    const d = describeOutcome({
      kind: "upgraded",
      successorPid: 42,
      handoffOffset: 9,
      previousBuild: null,
      successorBuild: null,
    });
    expect(d.text).toContain("unknown");
  });

  it("maps every other outcome to its tone and reason", () => {
    expect(describeOutcome({ kind: "rolled_back", reason: "nak" })).toEqual({
      tone: "warn",
      text: "Rolled back — the existing holder keeps serving the pane: nak",
    });
    expect(describeOutcome({ kind: "failed", reason: "no endpoint" }).tone).toBe("error");
    expect(describeOutcome({ kind: "unsupported", reason: "win" }).text).toContain("win");
    expect(describeOutcome({ kind: "refused", reason: "same build" }).text).toBe(
      "Not attempted: same build",
    );
    const unknown = describeOutcome({
      kind: "unknown_on_timeout",
      reason: "timed out",
      holderPidNow: 77,
    });
    expect(unknown.tone).toBe("error");
    expect(unknown.text).toContain("UNKNOWN");
    expect(unknown.text).toContain("Holder pid 77");
    expect(
      describeOutcome({ kind: "unknown_on_timeout", reason: "t", holderPidNow: null }).text,
    ).toContain("No holder answered");
  });
});

describe("invokeErrorText", () => {
  it("reads strings, errors and objects", () => {
    expect(invokeErrorText("boom")).toBe("boom");
    expect(invokeErrorText(new Error("bad"))).toBe("bad");
    expect(invokeErrorText({ a: 1 })).toBe('{"a":1}');
  });
});

describe("bannerHeadline", () => {
  it("counts panes, and never reads 0 panes as a decision", () => {
    expect(bannerHeadline(1, false)).toBe("1 pane's PTY holder needs a decision");
    expect(bannerHeadline(3, true)).toBe("3 panes' PTY holders need a decision");
    expect(bannerHeadline(0, true)).toBe("PTY holder census unavailable");
    expect(bannerHeadline(0, false)).toBe("PTY holder upgrade");
  });
});
