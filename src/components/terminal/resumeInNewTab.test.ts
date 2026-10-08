/**
 * THE operator resume path (plan `2026-10-04-runner-session-roster-restore-picker`,
 * Phase 4): one new tab, named by the display-name rule, a VERIFIED resume
 * under the known account, the open record written only on verify, and its own
 * pass through the resource guard. Every one-click Resume and the bulk queue
 * share it.
 */

import { describe, expect, it, vi } from "vitest";

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
  pastSessionResumeNotice,
  resumeFailureNotice,
  resumeInNewTab,
  resumeProfileSession,
  retryResumeArgs,
  type ResumeInNewTabDeps,
} from "./resumeInNewTab";
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

  it("an unreadable live registry does not refuse an operator's explicit resume", async () => {
    const { d } = deps({ liveSessionIds: async () => null });
    await expect(resumeInNewTab(d, target)).resolves.toMatchObject({ kind: "verified" });
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

describe("pastSessionResumeNotice (a Previous Sessions one-click Resume)", () => {
  const ps = {
    claudeSessionId: "abc-123",
    resumeName: "amber-otter",
    resumeAccount: { known: true, configDir: "C:/claude/.claude-gmail" },
    resumeDir: "D:/repo",
    provider: "claude",
  };

  it("resumes in THE resume dir and says nothing when it came back", async () => {
    const resume = vi.fn(async () => ({ kind: "verified" as const, tabId: "t" }));
    await expect(pastSessionResumeNotice(ps, resume)).resolves.toBeNull();
    expect(resume).toHaveBeenCalledWith({
      claudeSessionId: "abc-123",
      displayName: "amber-otter",
      workingDir: "D:/repo",
      configDir: "C:/claude/.claude-gmail",
      provider: "claude",
    });
  });

  it("a REJECTED resume surfaces as a notice, never silence", async () => {
    const resume = vi.fn(async () => {
      throw new Error("ipc down");
    });
    await expect(pastSessionResumeNotice(ps, resume)).resolves.toBe(
      'Could not resume "amber-otter": ipc down',
    );
  });

  it("refuses with a reason when the account or the dir is unknown", async () => {
    const resume = vi.fn();
    await expect(pastSessionResumeNotice({ ...ps, resumeDir: null }, resume)).resolves.toMatch(
      /no working directory/,
    );
    await expect(
      pastSessionResumeNotice({ ...ps, resumeAccount: { known: false, configDir: null } }, resume),
    ).resolves.toMatch(/account it ran under is unknown/);
    expect(resume).not.toHaveBeenCalled();
  });
});
