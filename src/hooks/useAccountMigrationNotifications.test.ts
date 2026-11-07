/**
 * The `session-account-migrated` toast text (`migratedToast`). The vitest
 * environment is `node` (no jsdom), so the pure payload → toast mapping is
 * tested rather than the hook's `listen` wiring.
 */

import { describe, it, expect } from "vitest";

import { migratedToast, type MigratedPayload } from "./useAccountMigrationNotifications";

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
