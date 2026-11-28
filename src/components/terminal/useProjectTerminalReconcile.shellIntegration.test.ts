/**
 * "Move them" over the page's REAL shell integration: `useProjectTerminalReconcile`'s
 * own `moveThem()`, rendered beside the page's real `useShellIntegration` and
 * fed through the same `shellMarksSink` a `TerminalInstance` uses. The session
 * context and the process probe are mocked; the marks are not.
 *
 * One-render harness (`renderToStaticMarkup`, as `useTerminalPages.test.ts`):
 * the hooks render once and `moveThem` is called afterwards.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import type { PaneClaudeProbe } from "./resumeVerification";

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

/** pwsh at zero descendants: idle, or `Get-Content -Wait` (in-process). */
const pwshNoChildren: PaneClaudeProbe = {
  state: "absent",
  sessionIds: [],
  root: { image: "pwsh.exe", isShell: true, descendants: 0 },
};

/**
 * The stale-mark case: the pane's shell drew a covered prompt while the pane
 * was live; then a chunk was dropped unparsed while it was hidden. The page's
 * handler must forget the prompt, so the move keeps it. A remount re-derives
 * the marks from its ring replay.
 */
describe("useProjectTerminalReconcile moveThem over the page's real shell integration", () => {
  beforeEach(() => {
    probeMock.mockReset();
    probeMock.mockResolvedValue(pwshNoChildren);
  });

  function renderWithShellIntegration() {
    const fx = {
      createTerminal: vi.fn(async () => "replacement-1"),
      closeTerminal: vi.fn(),
    };
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
    const marks = shellMarksSink(
      () => (event) => si.handleShellIntegration(PANE.id, event),
      (cb) => cb(),
    );
    marks.osc633("P;CommandMarks=1");
    marks.osc633("A");
    return { hook: hook!, marks, fx };
  }

  it("moveThem keeps a pane whose output was dropped unparsed after its last prompt", async () => {
    const { hook, marks, fx } = renderWithShellIntegration();
    // Hidden, and something printed: the drop route reports the lost chunk.
    marks.outputDropped();

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

  it("moveThem replaces a remounted pane whose ring replay ends at a covered prompt", async () => {
    const { hook, marks, fx } = renderWithShellIntegration();
    marks.unmounted(); // a page switch unmounts every pane…
    marks.beginReplay(); // …and the remount replays the ring
    for (const m of ["C", "D;0", "P;CommandMarks=1", "A"]) marks.osc633(m);
    marks.endReplay();

    await hook.moveThem();

    expect(fx.closeTerminal).toHaveBeenCalledWith("pwsh-1");
  });

  it("moveThem keeps a remounted pane whose ring replay ends mid-command", async () => {
    const { hook, marks, fx } = renderWithShellIntegration();
    marks.unmounted();
    marks.beginReplay();
    for (const m of ["P;CommandMarks=1", "A", "E;Get-Content -Wait log", "C"]) marks.osc633(m);
    marks.endReplay();

    await hook.moveThem();

    expect(fx.createTerminal).not.toHaveBeenCalled();
    expect(fx.closeTerminal).not.toHaveBeenCalled();
  });
});
