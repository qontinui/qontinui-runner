/**
 * "Move them", entered where the chip enters it: `useProjectTerminalReconcile`'s
 * own `moveThem()`, with the hook's real wiring from the page's terminal session
 * (its tabs, create/close, and the shell-integration readings) and the process
 * probe.
 *
 * One-render harness (`renderToStaticMarkup`, as `useTerminalPages.test.ts`):
 * the hook renders once and its `moveThem` is called afterwards. The session
 * context and the probe are mocked. The cases over the page's REAL
 * `useShellIntegration` live in `useProjectTerminalReconcile.shellIntegration.test.ts`.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import type { PaneClaudeProbe } from "./resumeVerification";
import type { ShellPromptMark } from "./shellIntegrationEvents";

const session = vi.hoisted(() => ({ current: null as unknown }));
const probeMock = vi.hoisted(() => vi.fn());
vi.mock("./contexts/TerminalSessionContext", () => ({
  useTerminalSession: () => session.current,
}));
vi.mock("./resumeVerification", () => ({
  probeClaudeInPane: (tabId: string) => probeMock(tabId),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));

import { useProjectTerminalReconcile } from "./useProjectTerminalReconcile";

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
