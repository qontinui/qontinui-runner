// @vitest-environment jsdom
/**
 * `ResumeGateHost` shows only the ACTIVE page's resume-live dialog. A dialog
 * it stops showing — the operator switched pages, or the host unmounted — is
 * answered "cancel" (review r2 finding 2): its click holds the conversation's
 * in-flight claim while it waits, and nothing else would ever answer it.
 *
 * The page contexts are replaced by plain values: what is pinned is which
 * page's `answerResumeGate` the host calls, and when.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ctx = vi.hoisted(() => ({
  session: null as unknown,
  pages: new Map<string, unknown>(),
  flashTab: (_tabId: string) => {},
}));
vi.mock("./contexts", () => ({
  useTerminalSession: () => ctx.session,
  usePageSessionGetter: () => (pageId: string) => ctx.pages.get(pageId) ?? null,
  useTransitionEffects: () => ({ flashTab: ctx.flashTab }),
}));
vi.mock("./commands/orchestrateCommand", () => ({
  TERMINAL_OPEN_PAGE_EVENT: "terminal-open-page",
}));

import type { LiveClaudeSession } from "./liveClaudeSessions";
import { ResumeGateHost } from "./ResumeGateHost";
import { RESUME_LIVE_DIALOG_ID, RESUME_LIVE_GO_TO_PANE_ID } from "./ResumeLiveDialog";
import type { ResumeGateChoice, ResumeGatePrompt } from "./resumeGate";
import type { RevealableZoneLayout } from "./tabReveal";

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
  pageId: "page-b",
};

const livePrompt: ResumeGatePrompt = {
  kind: "live",
  claudeSessionId: ID,
  conversationName: "maxim",
  holders: [holder],
  canFork: true,
};

/** One page's session value, as `useTerminalSession()` serves it. */
function page(pageId: string, prompt: ResumeGatePrompt | null) {
  const answer = vi.fn((_choice: ResumeGateChoice) => {});
  const setNotification = vi.fn();
  const value = {
    pageId,
    shellIntegration: { resumeGatePrompt: prompt, answerResumeGate: answer },
    workflowGen: { setNotification },
  };
  return { answer, setNotification, value };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  ctx.pages.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function show(value: unknown) {
  ctx.session = value;
  act(() => root.render(createElement(ResumeGateHost)));
}

const dialog = () => container.querySelector(`[data-ui-bridge-id="${RESUME_LIVE_DIALOG_ID}"]`);

describe("ResumeGateHost — a dialog it stops showing is answered cancel", () => {
  it("a page switch cancels the old page's open dialog, and only that one", () => {
    const a = page("page-a", livePrompt);
    const b = page("page-b", null);
    show(a.value);
    expect(dialog()).not.toBeNull();
    expect(a.answer).not.toHaveBeenCalled();

    show(b.value);
    expect(dialog()).toBeNull();
    expect(a.answer.mock.calls).toEqual([["cancel"]]);
    expect(b.answer).not.toHaveBeenCalled();
  });

  it("a re-render of the same page cancels nothing", () => {
    const a = page("page-a", livePrompt);
    show(a.value);
    show({ ...a.value });
    expect(a.answer).not.toHaveBeenCalled();
    expect(dialog()).not.toBeNull();
  });

  it("unmounting the host cancels the dialog it was showing", () => {
    const a = page("page-a", livePrompt);
    show(a.value);
    act(() => root.unmount());
    expect(a.answer.mock.calls).toEqual([["cancel"]]);
  });

  it("'Go to the existing pane' answers before it switches pages", () => {
    const order: string[] = [];
    const a = page("page-a", livePrompt);
    a.answer.mockImplementation((choice) => {
      order.push(`answer ${choice}`);
    });
    const zoneLayout: RevealableZoneLayout = {
      assignments: { 0: holder.terminalId! },
      layout: { zones: [{}] },
      focusedZone: 0,
      maximizedZone: null,
      getReservedZones: () => new Set(),
      assignTabToZone: () => {},
      setMaximizedZone: () => {},
      setFocusedZone: () => {},
      requestReveal: () => {},
    };
    ctx.pages.set("page-b", { tabs: [{ id: holder.terminalId! }], zoneLayout });
    const onOpenPage = () => order.push("open page-b");
    window.addEventListener("terminal-open-page", onOpenPage);
    try {
      show(a.value);
      act(() =>
        container
          .querySelector<HTMLElement>(`[data-ui-bridge-id="${RESUME_LIVE_GO_TO_PANE_ID}"]`)!
          .click(),
      );
    } finally {
      window.removeEventListener("terminal-open-page", onOpenPage);
    }
    expect(order).toEqual(["answer go-to-pane", "open page-b"]);
  });
});
