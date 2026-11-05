/**
 * `useShellIntegration`'s transcript-panel Resume, entered where the page
 * enters it: the hook's own `handleResumeSession`. The vitest project runs in
 * `node` with no React renderer, so `react` is replaced by identity hooks
 * (`useCallback` returns its callback, `useRef`/`useState` hold their initial
 * value) — the hook body itself runs unmodified.
 *
 * The contract pinned: a Resume opens its tab under the session's real
 * `--resume` name (`resume_name`, the `/rename` / ai-title label), falling
 * back to the `claude <id8>` label only when the transcript carries none.
 */
import { describe, expect, it, vi } from "vitest";

vi.mock("react", () => ({
  useCallback: <T>(fn: T) => fn,
  useRef: <T>(initial: T) => ({ current: initial }),
  useState: <T>(initial: T) => [initial, () => {}],
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
    getJSON: <T>(_k: string, fallback: T) => fallback,
    setJSON: () => {},
  },
}));

import { useShellIntegration } from "./useShellIntegration";
import type { TranscriptSession } from "./useTranscriptSessions";
import { PaneOutputStream } from "./paneOutputStream";
import { shellMarksSink } from "./shellIntegrationEvents";
import { RunnerGate, chunk, paneWriter } from "./__fixtures__/runnerGateModel.testkit";

const SESSION_ID = "0123abcd-4567-89ef-0123-456789abcdef";

function transcript(resumeName: string | null): TranscriptSession {
  return {
    session_id: SESSION_ID,
    project_path: "/repo",
    config_dir: "",
    message_count: 3,
    last_modified: "2026-10-04T00:00:00Z",
    started_at: null,
    first_message_preview: "first message preview",
    has_plans: false,
    display_name: "first message preview",
    resume_name: resumeName,
  };
}

function useShellIntegrationWith(newTab: ReturnType<typeof vi.fn>) {
  return useShellIntegration({
    tabs: [],
    updateTab: vi.fn(),
    renameTab: vi.fn(),
    createTerminal: newTab as never,
    setSessionStates: vi.fn() as never,
    terminalRefs: { current: new Map() },
    setRightPanelMode: vi.fn() as never,
    setSelectedTranscriptSessionId: vi.fn() as never,
    pageId: "default",
  });
}

describe("useShellIntegration handleResumeSession", () => {
  it("opens the resumed tab under the session's real --resume name", async () => {
    // A refused tab (null) ends the resume right after the tab request, so
    // nothing is typed and no verification loop is left running.
    const newTab = vi.fn(async () => null);
    const hook = useShellIntegrationWith(newTab);

    await hook.handleResumeSession(transcript("  my-renamed-session  "));

    expect(newTab).toHaveBeenCalledTimes(1);
    expect(newTab.mock.calls[0][0]).toBe("my-renamed-session");
    expect(newTab.mock.calls[0][1]).toBe("/repo");
  });
});

/**
 * The prompt-state witness "Move them" reads (`lastPromptMark`): the LAST of a
 * pane's 633;A (`prompt_start`) and 633;C (`command_execute`) marks, per tab,
 * and nothing for a pane that emitted neither.
 */
describe("useShellIntegration lastPromptMark", () => {
  it("tracks each pane's last prompt or command-start mark and nothing else", () => {
    const hook = useShellIntegrationWith(vi.fn());
    expect(hook.lastPromptMark("a")).toBeUndefined();

    hook.handleShellIntegration("a", { type: "prompt_start" });
    expect(hook.lastPromptMark("a")).toBe("prompt_start");

    hook.handleShellIntegration("a", { type: "command_line", command: "Get-Content -Wait x" });
    hook.handleShellIntegration("a", { type: "command_execute" });
    expect(hook.lastPromptMark("a")).toBe("command_execute");

    // The command ends: 633;D then the next prompt's 633;A.
    hook.handleShellIntegration("a", { type: "command_done", exitCode: 0 });
    expect(hook.lastPromptMark("a")).toBe("command_execute");
    hook.handleShellIntegration("a", { type: "prompt_start" });
    expect(hook.lastPromptMark("a")).toBe("prompt_start");

    // Marks are per pane.
    expect(hook.lastPromptMark("b")).toBeUndefined();
  });
});

/**
 * The capability is judged per prompt: `633;P;CommandMarks=1` covers the prompt
 * it is sent with, and a later prompt without it (a PowerShell nested prompt)
 * withdraws it.
 */
describe("useShellIntegration announcesCommandStarts", () => {
  it("is carried by the prompt it was sent with, and withdrawn by one without it", () => {
    const hook = useShellIntegrationWith(vi.fn());
    hook.handleShellIntegration("a", { type: "command_marks" });
    expect(hook.announcesCommandStarts("a")).toBe(false); // not until its prompt
    hook.handleShellIntegration("a", { type: "prompt_start" });
    expect(hook.announcesCommandStarts("a")).toBe(true);
    hook.handleShellIntegration("a", { type: "command_execute" });
    // `$Host.EnterNestedPrompt()`: a prompt, but the outer script is suspended.
    hook.handleShellIntegration("a", { type: "prompt_start" });
    expect(hook.lastPromptMark("a")).toBe("prompt_start");
    expect(hook.announcesCommandStarts("a")).toBe(false);
  });
});

/**
 * The prompt-state readings go stale the moment this window stops parsing the
 * pane, and must be forgotten then; a remount re-derives them from its ring
 * replay. Wired exactly as `TerminalInstance` wires it: the output stream's
 * missed-output hook, the mount's cleanup and the replay bracket all feed the
 * pane's `shellMarksSink`, whose events reach the page's real handler.
 */
describe("useShellIntegration tracks marks across loss and remount", () => {
  function usePane() {
    const hook = useShellIntegrationWith(vi.fn());
    const queued: Array<() => void> = [];
    const marks = shellMarksSink(
      () => (event) => hook.handleShellIntegration("t", event),
      (cb) => queued.push(cb),
    );
    const state = { paused: false };
    const stream = new PaneOutputStream(paneWriter(new RunnerGate()).queue, {
      isPaused: () => state.paused,
      hasBackend: () => true,
      noteMissedOutput: () => marks.outputLost(),
      requestResync: vi.fn(),
      scheduleFlush: vi.fn(),
    });
    // The pane's shell drew a prompt that carried the capability.
    marks.osc633("P;CommandMarks=1");
    marks.osc633("A");
    expect(hook.lastPromptMark("t")).toBe("prompt_start");
    expect(hook.announcesCommandStarts("t")).toBe(true);
    const runQueued = () => queued.splice(0).forEach((cb) => cb());
    return { hook, marks, stream, state, runQueued };
  }

  it("a chunk dropped while the pane is hidden clears its prompt mark and capability", () => {
    const { hook, stream, state, runQueued } = usePane();
    // Hidden: e.g. `Get-Content -Wait` typed into it — echo and 633;C unparsed.
    state.paused = true;
    expect(stream.receive(chunk(0, 64))).toBe("drop");
    expect(hook.lastPromptMark("t")).toBeUndefined();
    expect(hook.announcesCommandStarts("t")).toBe(false);
    runQueued();
    expect(hook.lastPromptMark("t")).toBeUndefined();
  });

  it("an unmount clears its prompt mark and capability", () => {
    const { hook, marks } = usePane();
    marks.unmounted();
    expect(hook.lastPromptMark("t")).toBeUndefined();
    expect(hook.announcesCommandStarts("t")).toBe(false);
  });

  it("a chunk routed to the pane's write path (not dropped) leaves them", () => {
    const { hook, stream } = usePane();
    stream.drainPending(); // the backend is ready: chunks route to "write"
    expect(stream.receive(chunk(0, 64))).toBe("write");
    expect(hook.lastPromptMark("t")).toBe("prompt_start");
    expect(hook.announcesCommandStarts("t")).toBe(true);
  });

  it("a remount whose ring replay ends at a covered prompt reads current", () => {
    const { hook, marks } = usePane();
    marks.unmounted(); // page switch: this mount goes…
    marks.beginReplay(); // …and the next one replays the ring
    for (const m of ["E;make", "C", "D;0", "P;CommandMarks=1", "A", "B"]) marks.osc633(m);
    marks.endReplay();
    expect(hook.lastPromptMark("t")).toBe("prompt_start");
    expect(hook.announcesCommandStarts("t")).toBe(true);
  });

  it("a remount whose ring replay ends mid-command reads busy", () => {
    const { hook, marks } = usePane();
    marks.unmounted();
    marks.beginReplay();
    for (const m of ["P;CommandMarks=1", "A", "E;Get-Content -Wait log", "C"]) marks.osc633(m);
    marks.endReplay();
    expect(hook.lastPromptMark("t")).toBe("command_execute");
  });
});
