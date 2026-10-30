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
 *   (`terminal_session_record_open`) until the operator chooses;
 * - the in-flight claim (review M3): two quick clicks on one free conversation
 *   start ONE writer, and a click acknowledges itself before its liveness read.
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
import { clearResumesInFlight } from "./resumeGate";
import type { TranscriptSession } from "./useTranscriptSessions";
import type { LiveClaudeSession } from "./liveClaudeSessions";

const SESSION_ID = "9b40e22d-deec-415c-9379-5f85ae994ad8";
const OTHER_ID = "3da13dea-0000-4000-8000-000000000000";

function transcript(resumeName: string | null, sessionId = SESSION_ID): TranscriptSession {
  return {
    session_id: sessionId,
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
  // The in-flight registry is module state; each test starts with none held.
  clearResumesInFlight();
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

    // No tab was opened, so the attempt was REFUSED (bulk waits no spacing).
    await expect(hook.handleResumeSession(transcript("  my-renamed-session  "))).resolves.toBe(
      "refused",
    );
    expect(newTab).toHaveBeenCalledTimes(1);
    expect(newTab.mock.calls[0][0]).toBe("my-renamed-session");
    expect(newTab.mock.calls[0][1]).toBe("/repo");
    // The click acknowledged itself first, then said why it did not come back.
    expect(notify.mock.calls[0][0]).toMatch(/Resuming "my-renamed-session" — checking/);
    expect(notify.mock.calls.at(-1)![0]).toMatch(
      /Could not resume "my-renamed-session": no terminal/,
    );
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
    await expect(outcome).resolves.toBe("refused"); // the stub created no tab
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

  it("a newer prompt (another conversation) cancels the older one instead of leaving it awaiting", async () => {
    registry([holder(), holder({ sessionId: OTHER_ID, pid: 50000, terminalId: "other-pane" })]);
    const { hook } = useShellIntegrationWith(vi.fn(async () => "t-new"));
    const first = hook.handleResumeSession(transcript("maxim"));
    await flush();
    const second = hook.handleResumeSession(transcript("other", OTHER_ID));
    await flush();
    await expect(first).resolves.toBe("cancelled");
    hook.answerResumeGate("cancel");
    await expect(second).resolves.toBe("cancelled");
  });

  it("a second click on a conversation whose dialog is open opens no second dialog (in flight)", async () => {
    registry([holder()]);
    const newTab = vi.fn(async () => "t-new");
    const { hook, notify } = useShellIntegrationWith(newTab);
    const first = hook.handleResumeSession(transcript("maxim"));
    await flush();
    await expect(hook.handleResumeSession(transcript("maxim"))).resolves.toBe("skipped-live");
    expect(notify.mock.calls.at(-1)![0]).toMatch(/"maxim" is already being resumed/);
    // The first click's dialog is still the one open, and still answerable.
    hook.answerResumeGate("cancel");
    await expect(first).resolves.toBe("cancelled");
    expect(newTab).not.toHaveBeenCalled();
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

describe("useShellIntegration handleResumeSession — two quick clicks (review M3)", () => {
  it("two concurrent clicks on one FREE conversation create exactly one tab", async () => {
    // Both clicks would read the registry as free: the new claude has not
    // written its registry file yet. Only the in-flight claim tells them apart.
    registry([]);
    const newTab = vi.fn(async () => "t-new");
    const { hook, notify } = useShellIntegrationWith(newTab);

    const outcomes = await Promise.all([
      hook.handleResumeSession(transcript("maxim")),
      hook.handleResumeSession(transcript("maxim")),
    ]);
    expect([...outcomes].sort()).toEqual(["resumed", "skipped-live"]);
    expect(newTab).toHaveBeenCalledTimes(1);
    expect(recordOpens()).toBe(1);
    expect(notify.mock.calls.some(([m]) => /already being resumed/.test(m as string))).toBe(true);

    // Inside the grace after a resume that opened a tab, a third click is
    // still refused: the registry may not show the new holder yet.
    await expect(hook.handleResumeSession(transcript("maxim"))).resolves.toBe("skipped-live");
    expect(newTab).toHaveBeenCalledTimes(1);
  });

  it("a click acknowledges itself BEFORE its liveness read resolves", async () => {
    let answer!: (rows: LiveClaudeSession[]) => void;
    const notify = vi.fn();
    const hook = useShellIntegration({
      tabs: [],
      updateTab: vi.fn(),
      renameTab: vi.fn(),
      createTerminal: vi.fn(async () => null) as never,
      setSessionStates: vi.fn() as never,
      terminalRefs: { current: new Map() },
      setRightPanelMode: vi.fn() as never,
      setSelectedTranscriptSessionId: vi.fn() as never,
      pageId: "default",
      notify,
      // The gate's read waits for the test; the pre-type re-read (the same
      // injected oracle) answers at once.
      readLiveSessions: (() => {
        let calls = 0;
        return () =>
          calls++ === 0
            ? new Promise<LiveClaudeSession[]>((r) => (answer = r))
            : Promise.resolve([]);
      })(),
    });
    const outcome = hook.handleResumeSession(transcript("maxim"));
    await flush();
    expect(notify).toHaveBeenCalledTimes(1);
    expect(notify.mock.calls[0]).toEqual([
      'Resuming "maxim" — checking whether it is already open…',
      "success",
    ]);
    answer([]);
    await expect(outcome).resolves.toBe("refused");
  });

  it("the 'Since restart' queue's resume is held under the same claim", async () => {
    registry([]);
    const newTab = vi.fn(async () => "t-new");
    const { hook } = useShellIntegrationWith(newTab);
    const target = {
      claudeSessionId: SESSION_ID,
      displayName: "maxim",
      workingDir: "/repo",
      configDir: "/home/u/.claude-work",
    };
    const [click, queued] = await Promise.all([
      hook.handleResumeSession(transcript("maxim")),
      hook.resumeTarget(target),
    ]);
    expect(click).toBe("resumed");
    expect(queued).toEqual({ kind: "failed", failure: "in-flight", tabId: null });
    expect(newTab).toHaveBeenCalledTimes(1);
  });
});
