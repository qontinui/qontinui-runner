/**
 * The `session-account-migrated` toast text (`migratedToast`). The vitest
 * environment is `node` (no jsdom), so the pure payload → toast mapping is
 * tested rather than the hook's `listen` wiring.
 */

import { describe, it, expect, vi } from "vitest";

import {
  LIMIT_BURST_MS,
  continuedBurstToast,
  createBurstCoalescer,
  formatDue,
  gaveUpBurstToast,
  parkIsToasted,
  migratedToast,
  parkedBurstToast,
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

describe("usage-limit park and continue toasts (plan 2026-11-05 Phase 2)", () => {
  const hhmm = (ms: number) => `T${ms}`;

  it("coalesces a burst of parks into one info toast naming the count and the time", () => {
    const toast = parkedBurstToast(
      [
        { claudeSessionId: "aaaaaaaa-1", dueAtMs: 5, reason: "not-yet-reset" },
        { claudeSessionId: "bbbbbbbb-2", dueAtMs: 5, reason: "not-yet-reset" },
        { claudeSessionId: "cccccccc-3", dueAtMs: 5, reason: "no-usable-target" },
      ],
      hhmm,
    );
    expect(toast.message).toBe("3 sessions parked at a usage limit until T5");
    expect(toast.type).toBe("info");
    expect(toast.message).not.toContain("out of tokens");
  });

  it("names one session, and a span of due times", () => {
    expect(parkedBurstToast([{ claudeSessionId: "0c4e3025-8c5c", dueAtMs: 7 }], hhmm).message).toBe(
      "Session 0c4e3025 parked at a usage limit until T7",
    );
    expect(parkedBurstToast([{ dueAtMs: 7 }], hhmm).message).toBe(
      "A session parked at a usage limit until T7",
    );
    expect(parkedBurstToast([{ dueAtMs: 9 }, { dueAtMs: 3 }], hhmm).message).toContain(
      "until T3–T9",
    );
  });

  it("says continued for a reset-continue burst", () => {
    expect(continuedBurstToast([{ claudeSessionId: "0c4e3025-x" }, {}]).message).toBe(
      "2 sessions continued after the usage limit reset",
    );
  });

  it("is an error toast when the runner gave up continuing", () => {
    const toast = gaveUpBurstToast([{ claudeSessionId: "0c4e3025-x", attempts: 3 }]);
    expect(toast.message).toBe(
      "Session 0c4e3025 stopped at a usage limit that has reset, but the runner could not continue it; it needs a prompt by hand",
    );
    expect(toast.type).toBe("error");
    expect(gaveUpBurstToast([{}, {}]).message).toBe(
      "2 sessions stopped at a usage limit that has reset, but the runner could not continue them; they need a prompt by hand",
    );
  });

  it("names the day when a park is due on another day", () => {
    const now = new Date(2026, 10, 16, 9, 0).getTime();
    const later = new Date(2026, 10, 16, 18, 30).getTime();
    const days = new Date(2026, 10, 19, 18, 30).getTime();
    expect(formatDue(later, now)).not.toContain(" ");
    expect(formatDue(later, now)).toContain("18");
    expect(formatDue(days, now)).toContain(" ");
    expect(formatDue(days, now)).toContain("19");
    expect(formatDue(days, now).endsWith(formatDue(later, now))).toBe(true);
  });

  it("raises no park toast for the runner's own continue retry", () => {
    expect(parkIsToasted({ reason: "continue-retry" })).toBe(false);
    expect(parkIsToasted({ reason: "migration-nudge-wait" })).toBe(false);
    expect(parkIsToasted({ reason: "not-yet-reset" })).toBe(true);
    expect(parkIsToasted({})).toBe(true);
  });

  it("flushes one burst per window", () => {
    vi.useFakeTimers();
    try {
      const flushed: number[][] = [];
      const c = createBurstCoalescer<number>(LIMIT_BURST_MS, (b) => flushed.push(b));
      for (let i = 0; i < 97; i++) c.push(i);
      vi.advanceTimersByTime(LIMIT_BURST_MS - 1);
      expect(flushed).toHaveLength(0);
      vi.advanceTimersByTime(1);
      expect(flushed).toHaveLength(1);
      expect(flushed[0]).toHaveLength(97);
      c.push(100);
      vi.advanceTimersByTime(LIMIT_BURST_MS);
      expect(flushed).toEqual([flushed[0], [100]]);
      c.push(101);
      c.cancel();
      vi.advanceTimersByTime(LIMIT_BURST_MS);
      expect(flushed).toHaveLength(2);
    } finally {
      vi.useRealTimers();
    }
  });
});
