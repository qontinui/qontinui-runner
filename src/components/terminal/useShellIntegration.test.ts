/**
 * `useShellIntegration`'s Resume, entered where the page enters it: the
 * hook's own `handleResumeSession` / `resumeClick`. The vitest project runs in
 * `node` with no React renderer, so `react` is replaced by identity hooks
 * (`useCallback` returns its callback, `useRef`/`useState` hold their initial
 * value) — the hook body itself runs unmodified.
 *
 * Contracts pinned:
 * - a Resume opens its tab under the session's real `--resume` name
 *   (`resume_name`), falling back to `claude <id8>` only when the transcript
 *   carries none;
 * - the click gate (plan
 *   `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 *   Phase 3): a click on a LIVE id, or with an unreadable registry, opens the
 *   resume-live dialog and creates no tab, types nothing and records nothing
 *   (`terminal_session_record_open`) until the operator chooses.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", () => ({
  useCallback: <T>(fn: T) => fn,
  useRef: <T>(initial: T) => ({ current: initial }),
  useState: <T>(initial: T) => [initial, () => {}],
}));

const live = vi.hoisted(() => ({ response: null as unknown }));
const invoke = vi.hoisted(() =>
  vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
    if (cmd === "terminal_claude_session_list_live") return live.response;
    return {};
  }),
);
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
    getJSON: <T>(_k: string, fallback: T) => fallback,
    setJSON: () => {},
  },
}));
// The verified type-and-check, reduced to what it does on a verified
// handshake that matters here: write the OPEN record.
const runVerifiedResume = vi.hoisted(() =>
  vi.fn(async (p: { recordOpen?: unknown }) => {
    await invoke("terminal_session_record_open", p.recordOpen);
    return "verified" as const;
  }),
);
vi.mock("./useTerminalInitialization", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./useTerminalInitialization")>()),
  runVerifiedResume,
}));

import { useShellIntegration } from "./useShellIntegration";
import type { TranscriptSession } from "./useTranscriptSessions";
import type { LiveClaudeSession } from "./liveClaudeSessions";

const SESSION_ID = "9b40e22d-deec-415c-9379-5f85ae994ad8";

function transcript(resumeName: string | null): TranscriptSession {
  return {
    session_id: SESSION_ID,
    project_path: "/repo",
    // A named (non-default) account: the default home is unreadable under the
    // mocked `invoke`, and a resume under it would be refused as needs-account.
    config_dir: "/home/u/.claude-work",
    message_count: 3,
    last_modified: "2026-10-04T00:00:00Z",
    started_at: null,
    first_message_preview: "first message preview",
    has_plans: false,
    display_name: "first message preview",
    resume_name: resumeName,
  };
}

function holder(over: Partial<LiveClaudeSession> = {}): LiveClaudeSession {
  return {
    sessionId: SESSION_ID,
    name: "maxim",
    pid: 49448,
    account: { label: "hotmail", wrapper: "clh" },
    workingDir: "D:/qontinui-root",
    status: "busy",
    kind: "interactive",
    startedAt: 1,
    updatedAt: 2,
    resumeCommand: null,
    terminalId: "09ce90d9-pane",
    pageId: "e7810bcf-page",
    ...over,
  };
}

function registry(rows: LiveClaudeSession[] | null) {
  live.response = rows === null ? {} : { success: true, data: { sessions: rows } };
}

function useShellIntegrationWith(newTab: ReturnType<typeof vi.fn>, notify = vi.fn()) {
  const setRightPanelMode = vi.fn();
  const hook = useShellIntegration({
    tabs: [],
    updateTab: vi.fn(),
    renameTab: vi.fn(),
    createTerminal: newTab as never,
    setSessionStates: vi.fn() as never,
    terminalRefs: { current: new Map() },
    setRightPanelMode: setRightPanelMode as never,
    setSelectedTranscriptSessionId: vi.fn() as never,
    pageId: "default",
    notify,
  });
  return { hook, setRightPanelMode, notify };
}

const flush = () => new Promise((r) => setTimeout(r, 0));
const recordOpens = () =>
  invoke.mock.calls.filter(([cmd]) => cmd === "terminal_session_record_open").length;

beforeEach(() => {
  invoke.mockClear();
  runVerifiedResume.mockClear();
  vi.stubGlobal("navigator", { platform: "Linux x86_64" } as Navigator);
  return () => vi.unstubAllGlobals();
});

describe("useShellIntegration handleResumeSession — a free id", () => {
  it("opens the resumed tab under the session's real --resume name", async () => {
    registry([]);
    // A refused tab (null) ends the resume right after the tab request, so
    // nothing is typed and no verification loop is left running.
    const newTab = vi.fn(async () => null);
    const { hook, notify } = useShellIntegrationWith(newTab);

    await expect(hook.handleResumeSession(transcript("  my-renamed-session  "))).resolves.toBe(
      "failed",
    );

    expect(newTab).toHaveBeenCalledTimes(1);
    expect(newTab.mock.calls[0][0]).toBe("my-renamed-session");
    expect(newTab.mock.calls[0][1]).toBe("/repo");
    // The failed click is said, not silent.
    expect(notify.mock.calls[0][0]).toMatch(/Could not resume "my-renamed-session": no terminal/);
  });

  it("resumes, records the open on verify, and reports `resumed`", async () => {
    registry([holder({ sessionId: "someone-else" })]);
    const { hook } = useShellIntegrationWith(vi.fn(async () => "t-new"));
    await expect(hook.handleResumeSession(transcript("maxim"))).resolves.toBe("resumed");
    expect(recordOpens()).toBe(1);
  });
});

describe("useShellIntegration handleResumeSession — the click gate", () => {
  it("a click on a LIVE id creates no tab and records nothing before a dialog choice", async () => {
    registry([holder()]);
    const newTab = vi.fn(async () => "t-new");
    const { hook, setRightPanelMode } = useShellIntegrationWith(newTab);

    const outcome = hook.handleResumeSession(transcript("maxim"));
    await flush();
    await flush();
    expect(newTab).not.toHaveBeenCalled();
    expect(runVerifiedResume).not.toHaveBeenCalled();
    expect(recordOpens()).toBe(0);
    // The transcript panel stays open while the operator decides.
    expect(setRightPanelMode).not.toHaveBeenCalled();

    hook.answerResumeGate("cancel");
    await expect(outcome).resolves.toBe("cancelled");
    expect(newTab).not.toHaveBeenCalled();
    expect(recordOpens()).toBe(0);
  });

  it("'Resume anyway' on a live id resumes only after the choice", async () => {
    registry([holder()]);
    const newTab = vi.fn(async () => "t-new");
    const { hook } = useShellIntegrationWith(newTab);

    const outcome = hook.handleResumeSession(transcript("maxim"));
    await flush();
    expect(recordOpens()).toBe(0);
    hook.answerResumeGate("resume-anyway");
    await expect(outcome).resolves.toBe("resumed");
    expect(newTab).toHaveBeenCalledTimes(1);
    expect(recordOpens()).toBe(1);
  });

  it("'Fork' opens a 'fork of <name> · <id8>' tab and binds no record to the original id", async () => {
    registry([holder()]);
    const newTab = vi.fn(async () => null);
    const { hook } = useShellIntegrationWith(newTab);

    const outcome = hook.handleResumeSession(transcript(null));
    await flush();
    hook.answerResumeGate("fork");
    await expect(outcome).resolves.toBe("failed"); // the stub created no tab
    // The holder's registry name wins over the (absent) transcript name.
    expect(newTab.mock.calls[0][0]).toBe("fork of maxim · 9b40e22d");
    expect(runVerifiedResume).not.toHaveBeenCalled();
    expect(recordOpens()).toBe(0);
  });

  it("'Go to the existing pane' resumes nothing and reports skipped-live", async () => {
    registry([holder()]);
    const newTab = vi.fn(async () => "t-new");
    const { hook } = useShellIntegrationWith(newTab);
    const outcome = hook.handleResumeSession(transcript("maxim"));
    await flush();
    hook.answerResumeGate("go-to-pane");
    await expect(outcome).resolves.toBe("skipped-live");
    expect(newTab).not.toHaveBeenCalled();
  });

  it("an UNREADABLE registry asks too (D2) and never falls through to a resume", async () => {
    registry(null);
    const newTab = vi.fn(async () => "t-new");
    const { hook } = useShellIntegrationWith(newTab);
    const outcome = hook.handleResumeSession(transcript("maxim"));
    await flush();
    await flush();
    expect(newTab).not.toHaveBeenCalled();
    hook.answerResumeGate("cancel");
    await expect(outcome).resolves.toBe("cancelled");
    expect(newTab).not.toHaveBeenCalled();
    expect(recordOpens()).toBe(0);
  });

  it("a newer prompt cancels the older one instead of leaving it awaiting", async () => {
    registry([holder()]);
    const { hook } = useShellIntegrationWith(vi.fn(async () => "t-new"));
    const first = hook.handleResumeSession(transcript("maxim"));
    await flush();
    const second = hook.handleResumeSession(transcript("maxim"));
    await flush();
    await expect(first).resolves.toBe("cancelled");
    hook.answerResumeGate("cancel");
    await expect(second).resolves.toBe("cancelled");
  });

  it("non-interactive (bulk): a live id is skipped-live, an unreadable registry skipped-unknown — no dialog, no tab", async () => {
    const newTab = vi.fn(async () => "t-new");
    const notify = vi.fn();
    const { hook } = useShellIntegrationWith(newTab, notify);
    registry([holder()]);
    await expect(
      hook.handleResumeSession(transcript("maxim"), { interactive: false }),
    ).resolves.toBe("skipped-live");
    registry(null);
    await expect(
      hook.handleResumeSession(transcript("maxim"), { interactive: false }),
    ).resolves.toBe("skipped-unknown");
    expect(newTab).not.toHaveBeenCalled();
    expect(notify).not.toHaveBeenCalled();
  });
});
