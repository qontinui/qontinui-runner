/**
 * THE operator resume path (plan `2026-10-04-runner-session-roster-restore-picker`,
 * Phase 4): one new tab, named by the display-name rule, a VERIFIED resume
 * under the known account, the open record written only on verify, and its own
 * pass through the resource guard. Every one-click Resume and the bulk queue
 * share it.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));
const stored = new Map<string, unknown>();
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
    getJSON: <T>(k: string, fallback: T) => (stored.has(k) ? (stored.get(k) as T) : fallback),
    setJSON: (k: string, v: unknown) => stored.set(k, v),
  },
}));

import { recallSessionId } from "./lastKnownSessionIds";
import {
  describeResumeFailure,
  forkInNewTab,
  pastSessionResumeTarget,
  resumeFailureNotice,
  resumeInNewTab,
  resumeProfileBatch,
  resumeProfileSession,
  resumeThrownNotice,
  retryResumeArgs,
  runVerifiedFork,
  type ResumeInNewTabDeps,
} from "./resumeInNewTab";
import {
  claimResume,
  clearResumesInFlight,
  isResumeInFlight,
  RESUME_IN_FLIGHT_GRACE_MS,
} from "./resumeGate";
import type { SessionOpenArgs } from "./sessionRecordArgs";

const HOME = "/home/u/.claude";

function deps(over: Partial<ResumeInNewTabDeps> = {}) {
  const createTerminal = vi.fn(async () => "tab-1" as string | null);
  const updateTab = vi.fn();
  const verify = vi.fn(async () => "verified" as const);
  const d: ResumeInNewTabDeps = {
    createTerminal,
    updateTab,
    terminalRefs: new Map(),
    pageId: "p1",
    liveSessionIds: async () => new Set<string>(),
    verify,
    defaultConfigHome: async () => HOME,
    ...over,
  };
  return { d, createTerminal, updateTab, verify };
}

const target = {
  claudeSessionId: "abc-123",
  displayName: "amber-otter",
  workingDir: "D:/repo",
  configDir: "C:/claude/.claude-gmail",
  provider: "claude",
};

describe("resumeInNewTab", () => {
  it("hands the new tab to onTabCreated, and only a tab that was created", async () => {
    const onTabCreated = vi.fn();
    await resumeInNewTab(deps({ onTabCreated }).d, target);
    expect(onTabCreated.mock.calls).toEqual([["tab-1"]]);

    const notCreated = vi.fn();
    await resumeInNewTab(
      deps({ onTabCreated: notCreated, createTerminal: vi.fn(async () => null) }).d,
      target,
    );
    const alreadyRunning = vi.fn();
    await resumeInNewTab(
      deps({
        onTabCreated: alreadyRunning,
        liveSessionIds: async () => new Set([target.claudeSessionId]),
      }).d,
      target,
    );
    expect(notCreated).not.toHaveBeenCalled();
    expect(alreadyRunning).not.toHaveBeenCalled();
  });

  it("opens one named tab through the resource guard and verifies the resume", async () => {
    const { d, createTerminal, verify, updateTab } = deps();
    const source = { label: "since-restart resume", queued: 3 };
    await expect(resumeInNewTab(d, target, source)).resolves.toEqual({
      kind: "verified",
      tabId: "tab-1",
    });
    expect(createTerminal).toHaveBeenCalledWith("amber-otter", "D:/repo", undefined, source);
    expect(updateTab).toHaveBeenCalledWith("tab-1", {
      claudeSessionId: "abc-123",
      claudeConfigDir: "C:/claude/.claude-gmail",
      claudeRecordConfigDir: "C:/claude/.claude-gmail",
      isReconnecting: true,
    });
    const call = verify.mock.calls[0][0];
    expect(call).toMatchObject({
      tabId: "tab-1",
      claudeSessionId: "abc-123",
      configDir: "C:/claude/.claude-gmail",
      provider: "claude",
      verifyOptions: { skipFirstProbe: true },
    });
    // The open record is the VERIFIED branch's to write, and it is authoritative.
    expect(call.recordOpen).toMatchObject({
      terminalId: "tab-1",
      pageId: "p1",
      title: "amber-otter",
      origin: "authoritative",
    });
  });

  it("refuses a session a live process already hosts — a second resume would fork it", async () => {
    const { d, createTerminal } = deps({ liveSessionIds: async () => new Set(["abc-123"]) });
    await expect(resumeInNewTab(d, target)).resolves.toEqual({
      kind: "failed",
      failure: "already-running",
      tabId: null,
    });
    expect(createTerminal).not.toHaveBeenCalled();
  });

  it("an unreadable live registry REFUSES (liveness-unknown) — a click never falls through to a resume that may fork", async () => {
    const { d, createTerminal } = deps({ liveSessionIds: async () => null });
    await expect(resumeInNewTab(d, target)).resolves.toEqual({
      kind: "failed",
      failure: "liveness-unknown",
      tabId: null,
    });
    expect(createTerminal).not.toHaveBeenCalled();
  });

  it("allowLive (the dialog's explicit 'Resume anyway') skips the liveness read entirely", async () => {
    const liveSessionIds = vi.fn(async () => new Set([target.claudeSessionId]));
    const { d, createTerminal } = deps({ liveSessionIds });
    await expect(resumeInNewTab(d, target, undefined, { allowLive: true })).resolves.toMatchObject({
      kind: "verified",
    });
    expect(liveSessionIds).not.toHaveBeenCalled();
    expect(createTerminal).toHaveBeenCalledTimes(1);
  });

  it("reports a refused spawn and an unverified resume as distinct failures", async () => {
    const refused = deps({ createTerminal: vi.fn(async () => null) });
    await expect(resumeInNewTab(refused.d, target)).resolves.toMatchObject({
      failure: "not-created",
    });
    const unverified = deps({ verify: vi.fn(async () => "failed" as const) });
    await expect(resumeInNewTab(unverified.d, target)).resolves.toEqual({
      kind: "failed",
      failure: "not-verified",
      tabId: "tab-1",
    });
    expect(describeResumeFailure("not-verified")).toMatch(/Retry/);
  });

  it("refuses a config dir that is not shell-safe (needs-account) rather than dropping it", async () => {
    const { d, createTerminal } = deps();
    await expect(resumeInNewTab(d, { ...target, configDir: "C:/claude/$(evil)" })).resolves.toEqual(
      { kind: "failed", failure: "needs-account", tabId: null },
    );
    expect(createTerminal).not.toHaveBeenCalled();
    expect(describeResumeFailure("needs-account")).toMatch(/account unknown/);
  });

  it("says why a one-click resume did not come back, and nothing when it did", () => {
    expect(resumeFailureNotice({ kind: "verified", tabId: "t" }, "amber")).toBeNull();
    expect(
      resumeFailureNotice({ kind: "failed", failure: "already-running", tabId: null }, "amber"),
    ).toBe(
      'Could not resume "amber": already running in another window — a second resume would fork it',
    );
  });

  it("never types an unsafe id", async () => {
    const { d, createTerminal } = deps();
    await expect(
      resumeInNewTab(d, { ...target, claudeSessionId: "abc; rm -rf /" }),
    ).resolves.toMatchObject({ failure: "invalid-session-id" });
    expect(createTerminal).not.toHaveBeenCalled();
  });
});

describe("the default account's explicit path stays out of tabs and profiles", () => {
  it("a default-account resume records the explicit path, but the tab and last-known id carry none", async () => {
    const { d, updateTab, verify } = deps();
    await resumeInNewTab(d, { ...target, configDir: HOME });
    expect(updateTab).toHaveBeenCalledWith("tab-1", {
      claudeSessionId: "abc-123",
      claudeConfigDir: undefined,
      claudeRecordConfigDir: HOME,
      isReconnecting: true,
    });
    expect(recallSessionId("tab-1")?.claudeConfigDir).toBeUndefined();
    const call = verify.mock.calls[0][0];
    expect(call.configDir).toBeUndefined();
    expect(call.recordOpen?.configDir).toBe(HOME);
  });

  it("a home with an apostrophe or a non-ASCII name resumes the default account: no variable, no needs-account", async () => {
    const home = "/home/Zoë O'Brien/.claude";
    const { d, verify, createTerminal } = deps({ defaultConfigHome: async () => home });
    await expect(resumeInNewTab(d, { ...target, configDir: home })).resolves.toEqual({
      kind: "verified",
      tabId: "tab-1",
    });
    expect(createTerminal).toHaveBeenCalled();
    const call = verify.mock.calls[0][0];
    expect(call.configDir).toBeUndefined();
    expect(call.recordOpen?.configDir).toBe(home);
  });

  it("with the default home unreadable, a dir that may be it is needs-account — never typed", async () => {
    const { d, createTerminal } = deps({ defaultConfigHome: async () => null });
    await expect(resumeInNewTab(d, { ...target, configDir: HOME })).resolves.toEqual({
      kind: "failed",
      failure: "needs-account",
      tabId: null,
    });
    expect(createTerminal).not.toHaveBeenCalled();
  });

  const recordOpen: SessionOpenArgs = {
    claudeSessionId: "abc-123",
    configDir: HOME,
    workingDir: "D:/repo",
    pageId: "p1",
    zoneIndex: 2,
    terminalId: "tab-9",
    origin: "authoritative",
  };

  it("a zone-profile resume of a default-home session types no CLAUDE_CONFIG_DIR and keeps the path out of the tab", async () => {
    const updateTab = vi.fn();
    const verify = vi.fn(async () => "verified" as const);
    await expect(
      resumeProfileSession(
        {
          terminalRefs: new Map(),
          updateTab,
          verify,
          defaultConfigHome: async () => HOME,
          notify: vi.fn(),
          invokeFn: vi.fn(async () => ({})),
        },
        { tabId: "tab-9", claudeSessionId: "abc-123", configDir: HOME, recordOpen },
      ),
    ).resolves.toEqual({ kind: "verified", tabId: "tab-9" });
    expect(updateTab).toHaveBeenCalledWith("tab-9", {
      claudeSessionId: "abc-123",
      claudeConfigDir: undefined,
      claudeRecordConfigDir: HOME,
    });
    const call = verify.mock.calls[0][0];
    expect(call.configDir).toBeUndefined();
    expect(call.recordOpen).toEqual({ ...recordOpen, configDir: HOME });
  });

  it("a zone-profile resume types a non-default account, and refuses an unsafe one", async () => {
    const updateTab = vi.fn();
    const verify = vi.fn(async () => "verified" as const);
    const profileDeps = {
      terminalRefs: new Map(),
      updateTab,
      verify,
      defaultConfigHome: async () => HOME,
      notify: vi.fn(),
      invokeFn: vi.fn(async () => ({})),
    };
    await resumeProfileSession(profileDeps, {
      tabId: "tab-9",
      claudeSessionId: "abc-123",
      configDir: "/home/u/.claude-x",
      recordOpen,
    });
    expect(verify.mock.calls[0][0].configDir).toBe("/home/u/.claude-x");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    await expect(
      resumeProfileSession(profileDeps, {
        tabId: "tab-9",
        claudeSessionId: "abc-123",
        configDir: "/x/$(evil)",
        recordOpen,
      }),
    ).resolves.toMatchObject({ kind: "failed", failure: "needs-account" });
    expect(verify).toHaveBeenCalledTimes(1);
    warn.mockRestore();
  });

  it("marks the session restore-pending BEFORE the resume is typed, so a failed verification is not closed while the tab sits in Retry", async () => {
    const order: string[] = [];
    const invokeFn = vi.fn(async (cmd: string) => {
      order.push(cmd);
      return {};
    });
    const verify = vi.fn(async () => {
      order.push("verify");
      return "failed" as const;
    });
    await expect(
      resumeProfileSession(
        {
          terminalRefs: new Map(),
          updateTab: vi.fn(),
          verify,
          defaultConfigHome: async () => HOME,
          notify: vi.fn(),
          invokeFn,
        },
        { tabId: "tab-9", claudeSessionId: "abc-123", configDir: HOME, recordOpen },
      ),
    ).resolves.toEqual({ kind: "failed", failure: "not-verified", tabId: "tab-9" });
    // `boot: false` — a profile resume is not a boot restore: the backend sets
    // only the pending marker (no census stamp) and skips a non-open row.
    expect(invokeFn).toHaveBeenCalledWith("terminal_session_mark_restore_pending", {
      claudeSessionId: "abc-123",
      boot: false,
    });
    expect(order).toEqual(["terminal_session_mark_restore_pending", "verify"]);
  });

  it("an account it cannot resume under holds the session awaiting an account and surfaces the refusal", async () => {
    const invokeFn = vi.fn(async () => ({}));
    const notify = vi.fn();
    const verify = vi.fn(async () => "verified" as const);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    await expect(
      resumeProfileSession(
        {
          terminalRefs: new Map(),
          updateTab: vi.fn(),
          verify,
          defaultConfigHome: async () => HOME,
          notify,
          invokeFn,
        },
        {
          tabId: "tab-9",
          claudeSessionId: "abc-123",
          configDir: "/x/$(evil)",
          recordOpen: { ...recordOpen, title: "amber-otter" },
        },
      ),
    ).resolves.toMatchObject({ kind: "failed", failure: "needs-account" });
    expect(invokeFn).toHaveBeenCalledWith("terminal_session_mark_awaiting_account", {
      claudeSessionId: "abc-123",
    });
    expect(invokeFn).not.toHaveBeenCalledWith(
      "terminal_session_mark_restore_pending",
      expect.anything(),
    );
    expect(notify).toHaveBeenCalledWith(
      `Could not resume "amber-otter": ${describeResumeFailure("needs-account")}`,
    );
    expect(verify).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});

/**
 * The zone-profile restore holds each typed id's in-flight claim (review r2
 * finding 3): in the seconds after it types `--resume`, before the restored
 * claude writes its registry file, a click would otherwise read the id as free.
 */
describe("resumeProfileBatch — the zone-profile restore under the in-flight claim", () => {
  beforeEach(() => clearResumesInFlight());
  afterEach(() => vi.useRealTimers());

  it("a held claim skips that record as in flight; the others type, after ONE registry read", async () => {
    const held = claimResume("held-id")!;
    const readLiveIds = vi.fn(async (): Promise<ReadonlySet<string> | null> => new Set<string>());
    const resumeOne = vi.fn(async (_item: { claudeSessionId: string }) => {});
    const onSkip = vi.fn();
    await resumeProfileBatch([{ claudeSessionId: "held-id" }, { claudeSessionId: "free-id" }], {
      readLiveIds,
      resumeOne,
      onSkip,
    });
    expect(resumeOne.mock.calls).toEqual([[{ claudeSessionId: "free-id" }]]);
    expect(onSkip.mock.calls).toEqual([[{ claudeSessionId: "held-id" }, "in-flight"]]);
    expect(readLiveIds).toHaveBeenCalledTimes(1);
    held.release();
  });

  it("holds a typed id's claim until its resume settles plus the grace, then releases it", async () => {
    vi.useFakeTimers();
    let finish!: () => void;
    const resumeOne = vi.fn(() => new Promise<void>((r) => (finish = r)));
    await resumeProfileBatch([{ claudeSessionId: "a" }], {
      readLiveIds: async () => new Set<string>(),
      resumeOne,
    });
    expect(resumeOne).toHaveBeenCalledTimes(1);
    // A click while the restored claude starts finds the id in flight.
    expect(claimResume("a")).toBeNull();

    finish();
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(RESUME_IN_FLIGHT_GRACE_MS - 1);
    expect(isResumeInFlight("a")).toBe(true);
    await vi.advanceTimersByTimeAsync(1);
    expect(isResumeInFlight("a")).toBe(false);
  });

  it("a live id, and every id under an unreadable registry, is skipped with its claim released at once", async () => {
    const resumeOne = vi.fn(async (_item: { claudeSessionId: string }) => {});
    const onSkip = vi.fn();
    await resumeProfileBatch([{ claudeSessionId: "live" }, { claudeSessionId: "free" }], {
      readLiveIds: async () => new Set(["live"]),
      resumeOne,
      onSkip,
    });
    expect(onSkip.mock.calls).toEqual([[{ claudeSessionId: "live" }, "skip-alive"]]);
    expect(resumeOne.mock.calls).toEqual([[{ claudeSessionId: "free" }]]);
    expect(isResumeInFlight("live")).toBe(false);

    clearResumesInFlight();
    onSkip.mockClear();
    resumeOne.mockClear();
    await resumeProfileBatch([{ claudeSessionId: "x" }, { claudeSessionId: "y" }], {
      readLiveIds: async () => null,
      resumeOne,
      onSkip,
    });
    expect(resumeOne).not.toHaveBeenCalled();
    expect(onSkip.mock.calls.map(([, why]) => why)).toEqual(["skip-unknown", "skip-unknown"]);
    expect(isResumeInFlight("x")).toBe(false);
    expect(isResumeInFlight("y")).toBe(false);
  });
});

describe("retryResumeArgs (the ResumeFailedBanner Retry)", () => {
  it("records a default-home session under its explicit path while typing no dir", () => {
    const tab = {
      id: "tab-1",
      title: "amber-otter",
      workingDir: "D:/repo",
      claudeSessionId: "abc-123",
      claudeConfigDir: undefined,
      claudeRecordConfigDir: HOME,
    };
    const retry = retryResumeArgs({ tab, assignments: { 3: "tab-1" }, tabs: [tab], pageId: "p1" });
    expect(retry?.configDir).toBeUndefined();
    expect(retry?.recordOpen).toEqual({
      claudeSessionId: "abc-123",
      configDir: HOME,
      workingDir: "D:/repo",
      pageId: "p1",
      zoneIndex: 3,
      title: "amber-otter",
      terminalId: "tab-1",
    });
  });

  it("a tab no resume path set records its own dir, and a tab with no session has no retry", () => {
    const tab = { id: "tab-1", claudeSessionId: "abc-123", claudeConfigDir: "/h/.claude-x" };
    expect(
      retryResumeArgs({ tab, assignments: {}, tabs: [tab], pageId: "p1" })?.recordOpen.configDir,
    ).toBe("/h/.claude-x");
    expect(
      retryResumeArgs({ tab: { id: "tab-2" }, assignments: {}, tabs: [], pageId: "p1" }),
    ).toBeNull();
  });
});

describe("pastSessionResumeTarget (a Previous Sessions one-click Resume, before the gate)", () => {
  const ps = {
    claudeSessionId: "abc-123",
    resumeName: "amber-otter",
    resumeAccount: { known: true, configDir: "C:/claude/.claude-gmail" },
    resumeDir: "D:/repo",
    provider: "claude",
  };

  it("resolves the target in THE resume dir, with the conversation's name", () => {
    expect(pastSessionResumeTarget(ps)).toEqual({
      kind: "target",
      target: {
        claudeSessionId: "abc-123",
        displayName: "amber-otter",
        workingDir: "D:/repo",
        configDir: "C:/claude/.claude-gmail",
        provider: "claude",
      },
      conversationName: "amber-otter",
    });
  });

  it("an unnamed session keeps the claude <id8> tab name but has no conversation name", () => {
    const r = pastSessionResumeTarget({ ...ps, resumeName: "" });
    expect(r).toMatchObject({
      kind: "target",
      target: { displayName: "claude abc-123" },
      conversationName: null,
    });
  });

  it("refuses with a reason when the account or the dir is unknown", () => {
    expect(pastSessionResumeTarget({ ...ps, resumeDir: null })).toMatchObject({
      kind: "refused",
      notice: expect.stringMatching(/no working directory/),
    });
    expect(
      pastSessionResumeTarget({ ...ps, resumeAccount: { known: false, configDir: null } }),
    ).toMatchObject({
      kind: "refused",
      notice: expect.stringMatching(/account it ran under is unknown/),
    });
  });

  it("a REJECTED resume has its own notice, never silence", () => {
    expect(resumeThrownNotice(new Error("ipc down"), "amber-otter")).toBe(
      'Could not resume "amber-otter": ipc down',
    );
  });
});

describe("forkInNewTab (the resume-live dialog's 'Fork as a new conversation')", () => {
  it("opens a tab under the fork title and binds NOTHING to the original id", async () => {
    const verifyFork = vi.fn(async () => "verified" as const);
    const liveSessionIds = vi.fn(async () => new Set([target.claudeSessionId]));
    const { d, createTerminal, updateTab, verify } = deps({ verifyFork, liveSessionIds });
    await expect(forkInNewTab(d, target, "fork of amber-otter · abc-123")).resolves.toEqual({
      kind: "verified",
      tabId: "tab-1",
    });
    expect(createTerminal.mock.calls[0][0]).toBe("fork of amber-otter · abc-123");
    expect(createTerminal.mock.calls[0][1]).toBe("D:/repo");
    // The original id belongs to the live holder: the fork tab never claims it.
    for (const [, updates] of updateTab.mock.calls) {
      expect(updates).not.toHaveProperty("claudeSessionId");
    }
    expect(verify).not.toHaveBeenCalled();
    // A fork writes a new transcript, so it needs no liveness clearance.
    expect(liveSessionIds).not.toHaveBeenCalled();
    expect(verifyFork.mock.calls[0][0]).toMatchObject({
      tabId: "tab-1",
      claudeSessionId: "abc-123",
      configDir: "C:/claude/.claude-gmail",
    });
  });

  it("reports an un-appendable command and an unverified fork as distinct failures", async () => {
    const unavailable = deps({ verifyFork: vi.fn(async () => "unavailable" as const) });
    await expect(forkInNewTab(unavailable.d, target, "f")).resolves.toMatchObject({
      failure: "fork-unavailable",
      tabId: "tab-1",
    });
    const unverified = deps({ verifyFork: vi.fn(async () => "failed" as const) });
    await expect(forkInNewTab(unverified.d, target, "f")).resolves.toMatchObject({
      failure: "fork-not-verified",
    });
    expect(describeResumeFailure("fork-unavailable")).toMatch(/--fork-session/);
  });

  it("refuses an unsafe id and an untypeable account before any tab", async () => {
    const bad = deps();
    await expect(
      forkInNewTab(bad.d, { ...target, claudeSessionId: "abc; rm -rf /" }, "f"),
    ).resolves.toMatchObject({ failure: "invalid-session-id" });
    await expect(
      forkInNewTab(bad.d, { ...target, configDir: "C:/claude/$(evil)" }, "f"),
    ).resolves.toMatchObject({ failure: "needs-account" });
    expect(bad.createTerminal).not.toHaveBeenCalled();
  });
});

describe("runVerifiedFork", () => {
  const base = {
    terminalRefs: new Map(),
    tabId: "tab-1",
    claudeSessionId: "abc-123",
    configDir: "C:/claude/.claude-gmail",
    defaultConfigHome: async () => HOME,
  };

  it("types the backend resume with --fork-session appended, and records nothing", async () => {
    const typeAndVerify = vi.fn(async () => "verified" as const);
    const updateTab = vi.fn();
    const buildCommand = vi.fn(
      async () => "claude --dangerously-skip-permissions --resume abc-123\r",
    );
    await expect(
      runVerifiedFork({ ...base, updateTab, buildCommand, typeAndVerify }),
    ).resolves.toBe("verified");
    expect(buildCommand.mock.calls[0][0]).toBe("abc-123");
    expect(buildCommand.mock.calls[0][1]).toBe("C:/claude/.claude-gmail");
    expect(typeAndVerify.mock.calls[0][2]).toBe(
      "claude --dangerously-skip-permissions --resume abc-123 --fork-session\r",
    );
    expect(updateTab).toHaveBeenLastCalledWith("tab-1", { isReconnecting: false });
    // Never parked in `resumeFailed` — a fork tab has no bound id to Retry.
    for (const [, updates] of updateTab.mock.calls) {
      expect(updates).not.toHaveProperty("resumeFailed");
    }
  });

  it("types nothing when the command does not end in --resume <id>", async () => {
    const typeAndVerify = vi.fn(async () => "verified" as const);
    const buildCommand = vi.fn(async () => "my-alias\r");
    await expect(
      runVerifiedFork({ ...base, updateTab: vi.fn(), buildCommand, typeAndVerify }),
    ).resolves.toBe("unavailable");
    expect(typeAndVerify).not.toHaveBeenCalled();
  });

  it("types nothing when the backend refuses to render", async () => {
    const typeAndVerify = vi.fn(async () => "verified" as const);
    const buildCommand = vi.fn(async () => {
      throw new Error("gateway unresolved");
    });
    await expect(
      runVerifiedFork({ ...base, updateTab: vi.fn(), buildCommand, typeAndVerify }),
    ).resolves.toBe("unavailable");
    expect(typeAndVerify).not.toHaveBeenCalled();
  });
});
