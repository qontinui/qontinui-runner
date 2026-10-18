import { describe, expect, it, vi } from "vitest";
import {
  END_AND_RESUME_MISSING_NOTE,
  UPGRADE_UNSUPPORTED_NOTE,
  attentionLabel,
  bannerHeadline,
  describeOutcome,
  holderActions,
  invokeErrorText,
  nextCensusDelayMs,
  createCensusPoller,
  CENSUS_IDLE_REFRESH_MS,
  CENSUS_REFRESH_MS,
  type AttentionPane,
  type HolderAttention,
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

describe("holderActions with the runner's reason", () => {
  it("shows a non-Linux Unix reason: Leave running only, no Upgrade", () => {
    const a = holderActions({
      upgradeSupported: false,
      upgradeUnsupportedReason: "needs Linux's SHUT_RD",
      endAndResumeAvailable: false,
    });
    expect(a.upgrade).toBe(false);
    expect(a.leaveRunning).toBe(true);
    expect(a.note).toContain("needs Linux's SHUT_RD");
    expect(a.note).toContain(END_AND_RESUME_MISSING_NOTE);
  });
});

describe("nextCensusDelayMs", () => {
  const pane: AttentionPane = {
    paneId: "p",
    reason: "incompatible",
    holderPid: 1,
    childPid: 2,
    holderBuild: null,
    holderVersions: [],
    detail: null,
  };
  it("backs off only after an empty, error-free census", () => {
    expect(nextCensusDelayMs({ panes: [], censusError: null }, null)).toBe(CENSUS_IDLE_REFRESH_MS);
    expect(nextCensusDelayMs({ panes: [pane], censusError: null }, null)).toBe(CENSUS_REFRESH_MS);
    expect(nextCensusDelayMs({ panes: [], censusError: "unreadable" }, null)).toBe(
      CENSUS_REFRESH_MS,
    );
    expect(nextCensusDelayMs({ panes: [], censusError: null }, "boom")).toBe(CENSUS_REFRESH_MS);
    expect(nextCensusDelayMs(null, null)).toBe(CENSUS_REFRESH_MS);
  });
});

describe("createCensusPoller", () => {
  it("two equal read errors in a row still schedule a third read (review p8f r2, N1)", async () => {
    vi.useFakeTimers();
    try {
      let reads = 0;
      const applied: string[] = [];
      const p = createCensusPoller(
        async () => {
          reads += 1;
          throw new Error("census unavailable");
        },
        (r) => applied.push(r.ok ? "ok" : r.error),
      );
      await p.refresh();
      expect(reads).toBe(1);
      await vi.advanceTimersByTimeAsync(CENSUS_REFRESH_MS);
      expect(reads).toBe(2);
      expect(applied).toEqual(["census unavailable", "census unavailable"]);
      await vi.advanceTimersByTimeAsync(CENSUS_REFRESH_MS);
      expect(reads).toBe(3);
      p.stop();
      await vi.advanceTimersByTimeAsync(CENSUS_IDLE_REFRESH_MS);
      expect(reads).toBe(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it("backs off after an empty clean census and applies only the newest read", async () => {
    vi.useFakeTimers();
    try {
      let reads = 0;
      const resolvers: Array<(a: HolderAttention) => void> = [];
      const applied: number[] = [];
      const p = createCensusPoller(
        () =>
          new Promise<HolderAttention>((res) => {
            reads += 1;
            resolvers.push(res);
          }),
        (r) => applied.push(r.ok ? r.attention.panes.length : -1),
      );
      const empty: HolderAttention = {
        upgradeSupported: true,
        upgradeUnsupportedReason: null,
        endAndResumeAvailable: false,
        runnerHolderBuild: "h",
        panes: [],
        censusError: null,
      };
      const first = p.refresh();
      const second = p.refresh();
      resolvers[1](empty);
      resolvers[0]({ ...empty, panes: [] });
      await Promise.all([first, second]);
      expect(applied).toEqual([0]);
      await vi.advanceTimersByTimeAsync(CENSUS_REFRESH_MS);
      expect(reads).toBe(2);
      await vi.advanceTimersByTimeAsync(CENSUS_IDLE_REFRESH_MS - CENSUS_REFRESH_MS);
      expect(reads).toBe(3);
      p.stop();
    } finally {
      vi.useRealTimers();
    }
  });
});
