// @vitest-environment jsdom
/**
 * A resume-live dialog cannot outlive its page (review r2 finding 2). The
 * click gate holds the conversation's in-flight claim while its dialog waits,
 * so a dialog left open when the page's scope unmounts (the page was closed)
 * would lock the id until the window reloads. The hook answers it "cancel" on
 * unmount: the click resolves and the claim is released.
 *
 * Mounted with a real React renderer, because the node-env suite
 * (`useShellIntegration.test.ts`) replaces React's hooks, and with them every
 * effect.
 */

import { act, createElement, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
    getJSON: <T,>(_k: string, fallback: T) => fallback,
    setJSON: () => {},
  },
}));

import type { LiveClaudeSession } from "./liveClaudeSessions";
import { clearResumesInFlight, isResumeInFlight, type ResumeOutcome } from "./resumeGate";
import { useShellIntegration, type UseShellIntegrationResult } from "./useShellIntegration";
import type { TranscriptSession } from "./useTranscriptSessions";

const ID = "9b40e22d-deec-415c-9379-5f85ae994ad8";

const holder: LiveClaudeSession = {
  sessionId: ID,
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
};

const transcript = {
  session_id: ID,
  project_path: "/repo",
  config_dir: "/home/u/.claude-work",
  resume_name: "maxim",
} as TranscriptSession;

let hook: UseShellIntegrationResult | null = null;
const reportHook = (h: UseShellIntegrationResult) => {
  hook = h;
};

/** One page's scope, reduced to the hook; it reports each committed value. */
function PageScope({ onHook }: { onHook: (h: UseShellIntegrationResult) => void }) {
  const h = useShellIntegration({
    tabs: [],
    updateTab: () => {},
    renameTab: () => {},
    createTerminal: async () => null,
    setSessionStates: () => {},
    terminalRefs: { current: new Map() },
    setRightPanelMode: () => {},
    setSelectedTranscriptSessionId: () => {},
    pageId: "page-1",
    // A live holder, so the click opens the dialog and waits on it.
    readLiveSessions: async () => [holder],
  });
  useEffect(() => onHook(h));
  return null;
}

/** `p`'s value, or "still waiting" when it has not settled within `ms`. */
function within<T>(p: Promise<T>, ms: number): Promise<T | "still waiting"> {
  return Promise.race([
    p,
    new Promise<"still waiting">((r) => setTimeout(() => r("still waiting"), ms)),
  ]);
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  clearResumesInFlight();
  hook = null;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

/**
 * Click Resume on the live conversation and let the gate open its dialog. The
 * click's outcome is wrapped: an async function returning it bare would adopt
 * it, and wait for the very answer the test has not given yet.
 */
async function clickIntoDialog(): Promise<{ outcome: Promise<ResumeOutcome> }> {
  let outcome!: Promise<ResumeOutcome>;
  await act(async () => {
    outcome = hook!.handleResumeSession(transcript);
    await new Promise((r) => setTimeout(r, 0));
  });
  return { outcome };
}

describe("useShellIntegration — a dialog left open when its page unmounts", () => {
  it("is answered cancel: the click resolves and the id's claim is released", async () => {
    act(() => root.render(createElement(PageScope, { onHook: reportHook })));
    const { outcome } = await clickIntoDialog();
    expect(hook!.resumeGatePrompt?.claudeSessionId).toBe(ID);
    expect(isResumeInFlight(ID), "the click holds the id while its dialog waits").toBe(true);

    act(() => root.unmount());
    await expect(within(outcome, 1_000)).resolves.toBe("cancelled");
    expect(isResumeInFlight(ID)).toBe(false);
  });

  it("while the page stays mounted, the dialog keeps waiting and keeps the claim", async () => {
    act(() => root.render(createElement(PageScope, { onHook: reportHook })));
    const { outcome } = await clickIntoDialog();
    await expect(within(outcome, 50)).resolves.toBe("still waiting");
    expect(isResumeInFlight(ID)).toBe(true);

    act(() => hook!.answerResumeGate("cancel"));
    await expect(outcome).resolves.toBe("cancelled");
    expect(isResumeInFlight(ID)).toBe(false);
  });
});
