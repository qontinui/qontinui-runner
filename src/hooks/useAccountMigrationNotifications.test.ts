/**
 * The `session-account-migrated` toast text (`migratedToast`). The vitest
 * environment is `node` (no jsdom), so the pure payload → toast mapping is
 * tested rather than the hook's `listen` wiring.
 */

import { describe, it, expect, vi } from "vitest";

import {
  createBurstCoalescer,
  limitContinuedToast,
  limitParkedToast,
  migratedToast,
  type MigratedPayload,
} from "./useAccountMigrationNotifications";

const base: MigratedPayload = {
  claudeSessionId: "0c4e3025-8c5c-4792-a3ca-201ad43e0113",
  fromConfigDir: "C:\\claude\\.claude-a",
  toConfigDir: "C:\\claude\\.claude-b",
};

describe("migratedToast", () => {
  it("names the backup path, and not the session directory, when the main transcript diverged", () => {
    const backup =
      "C:\\claude\\.claude-b\\projects\\D--qontinui-root\\0c4e3025-8c5c-4792-a3ca-201ad43e0113.jsonl.diverged-1761480000000";
    const toast = migratedToast({
      ...base,
      mainTranscript: "diverged_preserved",
      mainTranscriptBackup: backup,
      // The main transcript is no longer counted in the `<sid>/` tally.
      sessionDirCarry: { copied: 2, skippedPresent: 0, skippedOther: 0, failed: 0 },
    });
    expect(toast.message).toContain("DIVERGED");
    expect(toast.message).toContain(backup);
    expect(toast.message).toContain("live conversation was resumed");
    expect(toast.message).not.toContain("session directory was NOT carried");
    expect(toast.type).toBe("info");
  });

  it("is a plain success for an ordinary copy", () => {
    const toast = migratedToast({
      ...base,
      mainTranscript: "copied",
      mainTranscriptBackup: null,
      sessionDirCarry: { copied: 3, skippedPresent: 0, skippedOther: 0, failed: 0 },
    });
    expect(toast.message).toContain("resumed on .claude-b (3 session file(s)");
    expect(toast.message).not.toContain("DIVERGED");
    expect(toast.type).toBe("success");
  });

  it("is an error when the target's own copy was resumed instead of the live one", () => {
    const toast = migratedToast({ ...base, mainTranscript: "kept_existing" });
    expect(toast.message).toContain("NOT the live one");
    expect(toast.type).toBe("error");
  });

  it("still blames the session directory when its root was refused", () => {
    const toast = migratedToast({
      ...base,
      mainTranscript: "present",
      sessionDirCarry: { copied: 0, skippedPresent: 0, skippedOther: 1, failed: 0 },
    });
    expect(toast.message).toContain("session directory was NOT carried");
    expect(toast.type).toBe("error");
  });

  it("names diverged subagent transcripts as preserved, not as entries not followed", () => {
    const toast = migratedToast({
      ...base,
      mainTranscript: "present",
      sessionDirCarry: {
        copied: 0,
        skippedPresent: 0,
        skippedOther: 0,
        subagentsDivergedPreserved: 2,
        subagentsDivergedKept: 0,
        failed: 0,
      },
    });
    expect(toast.message).toContain("2 subagent transcript(s) on the target had DIVERGED");
    expect(toast.message).toContain("*.diverged-*");
    expect(toast.message).toContain("live one carried");
    expect(toast.message).not.toContain("session directory was NOT carried");
    expect(toast.message).not.toContain("not followed");
    expect(toast.type).toBe("info");
  });

  it("is an error when diverged subagent transcripts were kept instead of the live ones", () => {
    const toast = migratedToast({
      ...base,
      mainTranscript: "present",
      sessionDirCarry: { copied: 1, skippedOther: 0, subagentsDivergedKept: 1, failed: 0 },
    });
    expect(toast.message).toContain("kept INSTEAD of the live ones");
    expect(toast.message).not.toContain("session directory was NOT carried");
    expect(toast.type).toBe("error");
  });

  it("keeps a failed carry an error and still names the preserved forks", () => {
    const toast = migratedToast({
      ...base,
      mainTranscript: "present",
      sessionDirCarry: { copied: 1, subagentsDivergedPreserved: 1, failed: 1 },
    });
    expect(toast.message).toContain("could NOT be carried");
    expect(toast.message).toContain("1 subagent transcript(s) on the target had DIVERGED");
    expect(toast.type).toBe("error");
  });

  it("tolerates an older build's payload with neither new field", () => {
    const toast = migratedToast(base);
    expect(toast.message).toBe(
      "Session 0c4e3025 migrated: .claude-a ran out of tokens → resumed on .claude-b",
    );
    expect(toast.type).toBe("success");
  });
});

describe("limitParkedToast (plan 2026-11-05 Phase 2)", () => {
  const hhmm = (ms: number) => `T${ms}`;

  it("is an info toast, never the error the skipped event raises", () => {
    const toast = limitParkedToast(
      [
        {
          claudeSessionId: "aaaaaaaa-1111",
          configDir: "/home/u/.claude-jan",
          dueAtMs: 5,
          reason: "reset-pending",
        },
      ],
      hhmm,
    );
    expect(toast?.type).toBe("info");
    expect(toast?.message).toBe(
      "Session aaaaaaaa stopped at its usage limit on .claude-jan and is parked until T5 (reset-pending)",
    );
    expect(toast?.message).not.toContain("out of tokens");
  });

  it("coalesces a burst into one count, by session, until the latest due time", () => {
    const toast = limitParkedToast(
      [
        { claudeSessionId: "s1", dueAtMs: 10 },
        { claudeSessionId: "s2", dueAtMs: 30 },
        { claudeSessionId: "s1", dueAtMs: 20 },
        { claudeSessionId: "s3", dueAtMs: 15 },
      ],
      hhmm,
    );
    expect(toast?.message).toBe("3 sessions stopped at their usage limit and are parked until T30");
  });

  it("says no time when none was sent, and nothing for an empty burst", () => {
    expect(
      limitParkedToast([{ claudeSessionId: "s1" }, { claudeSessionId: "s2" }], hhmm)?.message,
    ).toBe("2 sessions stopped at their usage limit and are parked");
    expect(limitParkedToast([], hhmm)).toBeNull();
  });
});

describe("limitContinuedToast", () => {
  it("names one session, and counts a burst", () => {
    expect(
      limitContinuedToast([
        { claudeSessionId: "bbbbbbbb-2222", configDir: "C:\\claude\\.claude-x" },
      ])?.message,
    ).toBe("Session bbbbbbbb's usage limit on .claude-x has reset — told it to continue");
    const many = limitContinuedToast([{ claudeSessionId: "a" }, { claudeSessionId: "b" }]);
    expect(many?.message).toBe("2 sessions' usage limits have reset — told each to continue");
    expect(many?.type).toBe("success");
    expect(limitContinuedToast([])).toBeNull();
  });
});

describe("createBurstCoalescer", () => {
  it("flushes everything pushed within the window once, then starts a new burst", () => {
    vi.useFakeTimers();
    try {
      const flushed: number[][] = [];
      const c = createBurstCoalescer<number>(2000, (items) => flushed.push(items));
      c.push(1);
      vi.advanceTimersByTime(1000);
      c.push(2);
      c.push(3);
      expect(flushed).toEqual([]);
      vi.advanceTimersByTime(1000);
      expect(flushed).toEqual([[1, 2, 3]]);
      c.push(4);
      vi.advanceTimersByTime(2000);
      expect(flushed).toEqual([[1, 2, 3], [4]]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("drops a pending burst on cancel", () => {
    vi.useFakeTimers();
    try {
      const flushed: number[][] = [];
      const c = createBurstCoalescer<number>(2000, (items) => flushed.push(items));
      c.push(1);
      c.cancel();
      vi.advanceTimersByTime(5000);
      expect(flushed).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });
});
