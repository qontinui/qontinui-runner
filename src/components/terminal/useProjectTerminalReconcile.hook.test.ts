/**
 * "Move them", entered where the chip enters it: `useProjectTerminalReconcile`'s
 * own `moveThem()`, with the hook's real wiring from the page's terminal session
 * (its tabs, create/close, and the shell-integration readings) and the process
 * probe.
 *
 * One-render harness (`renderToStaticMarkup`, as `useTerminalPages.test.ts`):
 * the hook renders once and its `moveThem` is called afterwards. The session
 * context and the probe are mocked; the second suite renders the page's REAL
 * `useShellIntegration` beside the hook and feeds it through the same sink a
 * `TerminalInstance` uses.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import type { PaneClaudeProbe } from "./resumeVerification";
import type { ShellPromptMark } from "./useShellIntegration";

const session = vi.hoisted(() => ({ current: null as unknown }));
const probeMock = vi.hoisted(() => vi.fn());
vi.mock("./contexts/TerminalSessionContext", () => ({
  useTerminalSession: () => session.current,
}));
vi.mock("./resumeVerification", () => ({
  probeClaudeInPane: (tabId: string) => probeMock(tabId),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));

import { shellMarksSink } from "./shellIntegrationEvents";
import { useProjectTerminalReconcile } from "./useProjectTerminalReconcile";
import { useShellIntegration } from "./useShellIntegration";

const ROOT = "D:\\projects\\pizzeria";
const ELSEWHERE = "D:\\projects\\book-notes";
const PANE = { id: "pwsh-1", title: "pwsh", isAlive: true, workingDir: ELSEWHERE };

/** pwsh at zero descendants — what `Get-Content -Wait` (in-process) looks like. */
const pwshNoChildren: PaneClaudeProbe = {
  state: "absent",
  sessionIds: [],
  root: { image: "pwsh.exe", isShell: true, descendants: 0 },
};

function effects() {
  return {
    createTerminal: vi.fn(async () => "replacement-1"),
    closeTerminal: vi.fn(),
  };
}

function renderReconcile(): ReturnType<typeof useProjectTerminalReconcile> {
  let out: ReturnType<typeof useProjectTerminalReconcile> | null = null;
  function Probe() {
    out = useProjectTerminalReconcile(ROOT);
    return null;
  }
  renderToStaticMarkup(createElement(Probe));
  expect(out).not.toBeNull();
  return out!;
}

describe("useProjectTerminalReconcile moveThem (the hook's own entry)", () => {
  beforeEach(() => {
    probeMock.mockReset();
    probeMock.mockResolvedValue(pwshNoChildren);
  });

  it("moveThem keeps a pwsh pane with no descendant whose shell last reported a command start", async () => {
    const fx = effects();
    session.current = {
      tabs: [PANE],
      ...fx,
      shellIntegration: {
        lastPromptMark: (): ShellPromptMark => "command_execute",
        announcesCommandStarts: () => true,
      },
    };
    const hook = renderReconcile();
    expect(hook.count).toBe(1);

    await hook.moveThem();

    expect(probeMock).toHaveBeenCalledWith("pwsh-1");
    expect(fx.createTerminal).not.toHaveBeenCalled();
    expect(fx.closeTerminal).not.toHaveBeenCalled();
  });
});

/**
 * The stale-mark case: the pane's shell drew its prompt (and announced command
 * marks) while the pane was live; then a chunk was dropped unparsed while it
 * was hidden. The page's handler must forget the prompt, so the move keeps it.
 */
describe("useProjectTerminalReconcile moveThem over the page's real shell integration", () => {
  beforeEach(() => {
    probeMock.mockReset();
    probeMock.mockResolvedValue(pwshNoChildren);
  });

  function renderWithShellIntegration() {
    const fx = effects();
    let hook: ReturnType<typeof useProjectTerminalReconcile> | null = null;
    let shellIntegration: ReturnType<typeof useShellIntegration> | null = null;
    function Probe() {
      shellIntegration = useShellIntegration({
        tabs: [PANE],
        updateTab: vi.fn(),
        renameTab: vi.fn(),
        createTerminal: fx.createTerminal as never,
        setSessionStates: vi.fn() as never,
        terminalRefs: { current: new Map() },
        setRightPanelMode: vi.fn() as never,
        setSelectedTranscriptSessionId: vi.fn() as never,
        pageId: "default",
      });
      session.current = { tabs: [PANE], ...fx, shellIntegration };
      hook = useProjectTerminalReconcile(ROOT);
      return null;
    }
    renderToStaticMarkup(createElement(Probe));
    const si = shellIntegration!;
    const marks = shellMarksSink(() => (event) => si.handleShellIntegration(PANE.id, event));
    marks.osc633("P;CommandMarks=1");
    marks.osc633("A");
    return { hook: hook!, marks, fx };
  }

  it("moveThem keeps a pane whose output was dropped unparsed after its last prompt", async () => {
    const { hook, marks, fx } = renderWithShellIntegration();
    // Hidden, and something printed: the drop route reports the missed chunk.
    marks.outputMissed();

    await hook.moveThem();

    expect(fx.createTerminal).not.toHaveBeenCalled();
    expect(fx.closeTerminal).not.toHaveBeenCalled();
  });

  it("moveThem replaces the same pane while its prompt mark is current", async () => {
    const { hook, fx } = renderWithShellIntegration();

    await hook.moveThem();

    expect(fx.createTerminal).toHaveBeenCalledTimes(1);
    expect(fx.closeTerminal).toHaveBeenCalledWith("pwsh-1");
  });
});
