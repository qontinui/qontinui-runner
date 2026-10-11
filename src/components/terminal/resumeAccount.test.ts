/**
 * Zone-profile / transcript resume account resolution (follow-up to the
 * 2026-10-08 boot-restore defect, runner #2088).
 *
 * A profile session saved WITHOUT an account (`claudeConfigDir` null or
 * undefined) used to type a bare `claude --resume <id>` into a pane carrying
 * the spawn picker's account, while the transcript lived under the default
 * `~/.claude` — "No conversation found". Every non-boot resume now asks the
 * backend resolver for the transcript's account and types it explicitly.
 */

import { describe, it, expect, vi } from "vitest";

import {
  partitionParkedProfileSessions,
  planProfileTabs,
  profileResumeCommand,
  profileSessionAccount,
  resolveResumeAccount,
  withResolvedResumeAccounts,
  type InvokeFn,
} from "./resumeAccount";
import type { ZoneSessionInfo } from "./zoneProfileStorage";

const DEFAULT_DIR = "/home/u/.claude";
const PICKER_DIR = "/home/u/.claude-iris";

/** A backend that resolves every id to the dir its transcript lives under. */
function backendResolving(dir: string | undefined): InvokeFn & ReturnType<typeof vi.fn> {
  return vi.fn(async () => ({
    success: true,
    data:
      dir === undefined
        ? { transcriptExists: false }
        : { resumeConfigDir: dir, transcriptExists: true },
  }));
}

describe("zone-profile resume (null configDir, transcript only under the default dir)", () => {
  it("resumes under the transcript's default dir, typed explicitly", async () => {
    const invokeFn = backendResolving(DEFAULT_DIR);
    const saved: ZoneSessionInfo[] = [
      { zoneIndex: 0, claudeSessionId: "sess-null" },
      { zoneIndex: 1, claudeSessionId: "sess-undef", claudeConfigDir: undefined },
    ];
    const resolved = await withResolvedResumeAccounts(saved, (q) =>
      resolveResumeAccount(q, invokeFn),
    );

    for (const s of resolved) {
      expect(profileSessionAccount(s)).toBe(DEFAULT_DIR);
      const cmd = profileResumeCommand(s);
      // Platform-neutral: `CLAUDE_CONFIG_DIR="…" ` (POSIX) or
      // `$env:CLAUDE_CONFIG_DIR="…"; ` (PowerShell on a Windows CI leg).
      expect(cmd).toContain(`CLAUDE_CONFIG_DIR="${DEFAULT_DIR}"`);
      expect(cmd.indexOf("CLAUDE_CONFIG_DIR")).toBeLessThan(cmd.indexOf("claude "));
      expect(cmd).toContain(`--resume ${s.claudeSessionId}\r`);
    }
    // The backend was asked with NO account, so it searches by the transcript.
    expect(invokeFn).toHaveBeenCalledWith("terminal_resolve_resume_account", {
      claudeSessionId: "sess-null",
      configDir: null,
      workingDir: null,
    });
  });

  it("pins a freshly spawned profile pane to the resolved default dir", async () => {
    const resolved = await withResolvedResumeAccounts(
      [
        { zoneIndex: 0, claudeSessionId: "reuse-me" },
        { zoneIndex: 3, claudeSessionId: "spawn-me" },
      ],
      async () => DEFAULT_DIR,
    );
    const plan = planProfileTabs(resolved, ["tab-existing"]);
    expect(plan[0]).toMatchObject({ reuseTabId: "tab-existing" });
    expect(plan[0].pin).toBeUndefined();
    expect(plan[1].reuseTabId).toBeUndefined();
    expect(plan[1].pin).toBe(DEFAULT_DIR);
    expect(plan[1].session.zoneIndex).toBe(3);
  });

  it("the transcript's account outranks a stale saved one (the picker's must not win)", () => {
    const s: ZoneSessionInfo = {
      zoneIndex: 0,
      claudeSessionId: "s",
      claudeConfigDir: PICKER_DIR,
      resumeConfigDir: DEFAULT_DIR,
    };
    expect(profileSessionAccount(s)).toBe(DEFAULT_DIR);
    expect(profileResumeCommand(s)).toContain(`CLAUDE_CONFIG_DIR="${DEFAULT_DIR}"`);
    expect(profileResumeCommand(s)).not.toContain(PICKER_DIR);
  });
});

describe("resolveResumeAccount (transcript / Past Sessions resume)", () => {
  it("a blank recorded configDir (a Past Sessions null row) resolves to the default dir", async () => {
    const invokeFn = backendResolving(DEFAULT_DIR);
    await expect(
      resolveResumeAccount({ claudeSessionId: "s", configDir: "", workingDir: "/w" }, invokeFn),
    ).resolves.toBe(DEFAULT_DIR);
    expect(invokeFn).toHaveBeenCalledWith("terminal_resolve_resume_account", {
      claudeSessionId: "s",
      configDir: null,
      workingDir: "/w",
    });
  });

  it("falls back to the recorded dir only when the backend names none or cannot be asked", async () => {
    await expect(
      resolveResumeAccount(
        { claudeSessionId: "s", configDir: "/acct/x" },
        backendResolving(undefined),
      ),
    ).resolves.toBe("/acct/x");
    const failing: InvokeFn = async () => {
      throw new Error("ipc down");
    };
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await expect(
        resolveResumeAccount({ claudeSessionId: "s", configDir: "/acct/x" }, failing),
      ).resolves.toBe("/acct/x");
      expect(warn).toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it("never splices an unsafe dir into the typed command", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await expect(
        resolveResumeAccount(
          { claudeSessionId: "s", configDir: "/acct/x" },
          backendResolving('/bad";rm -rf ~'),
        ),
      ).resolves.toBe("/acct/x");
      expect(warn).toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });
});

describe("partitionParkedProfileSessions (no resume into another session's pane)", () => {
  it("never types a session's resume into a pane placed for a different session", () => {
    // Session A is bound to pane PA (zone 3); PA was auto-filled into zone 0
    // (B's zone) before its explicit placement. B is bound to PB. Neither may
    // be resumed into PA at zone 0.
    const a: ZoneSessionInfo = { zoneIndex: 3, claudeSessionId: "A", expectedTabId: "PA" };
    const b: ZoneSessionInfo = { zoneIndex: 0, claudeSessionId: "B", expectedTabId: "PB" };
    const early = partitionParkedProfileSessions([a, b], { 0: "PA" });
    expect(early.toResume).toEqual([]);
    expect(early.remaining).toEqual([a, b]);

    // Once each pane holds its own session's zone, each resumes exactly once.
    const settled = partitionParkedProfileSessions([a, b], { 0: "PB", 3: "PA" });
    expect(settled.toResume).toEqual([
      { s: a, tabId: "PA" },
      { s: b, tabId: "PB" },
    ]);
    expect(settled.remaining).toEqual([]);
  });

  it("keeps unbound sessions on the zone-only rule, and never types an unsafe id", () => {
    const unbound: ZoneSessionInfo = { zoneIndex: 1, claudeSessionId: "U" };
    const unsafe: ZoneSessionInfo = { zoneIndex: 2, claudeSessionId: "x; rm -rf ~" };
    const out = partitionParkedProfileSessions([unbound, unsafe], { 1: "T1", 2: "T2" });
    expect(out.toResume).toEqual([{ s: unbound, tabId: "T1" }]);
    expect(out.remaining).toEqual([unsafe]);
  });
});
