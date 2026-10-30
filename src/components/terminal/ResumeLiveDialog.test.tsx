// @vitest-environment jsdom
/**
 * The resume-live dialog (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 3), mounted for real: each form renders its options, the default
 * control holds focus, and every button calls its own handler and no other.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  RESUME_ANYWAY_LIVE_TEXT,
  RESUME_ANYWAY_UNKNOWN_TEXT,
  RESUME_LIVE_CANCEL_ID,
  RESUME_LIVE_CLOSE_ID,
  RESUME_LIVE_DIALOG_ID,
  RESUME_LIVE_FORK_ID,
  RESUME_LIVE_GO_TO_PANE_ID,
  RESUME_LIVE_RESUME_ANYWAY_ID,
  ResumeLiveDialog,
  resumeLiveHolderId,
} from "./ResumeLiveDialog";
import type { LiveClaudeSession } from "./liveClaudeSessions";
import type { ResumeGatePrompt } from "./resumeGate";

const ID = "9b40e22d-deec-415c-9379-5f85ae994ad8";

function row(over: Partial<LiveClaudeSession> = {}): LiveClaudeSession {
  return {
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
    ...over,
  };
}

const inPane = row({ terminalId: "09ce90d9-pane", pageId: "e7810bcf-page" });
const outside = row({ pid: 54516 });

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function mount(prompt: ResumeGatePrompt) {
  const handlers = {
    onGoToPane: vi.fn(),
    onFork: vi.fn(),
    onResumeAnyway: vi.fn(),
    onCancel: vi.fn(),
  };
  act(() => root.render(createElement(ResumeLiveDialog, { prompt, ...handlers })));
  return handlers;
}

const el = (id: string) => container.querySelector<HTMLElement>(`[data-ui-bridge-id="${id}"]`);
const click = (id: string) => act(() => el(id)!.click());

function livePrompt(holders: LiveClaudeSession[], canFork = true): ResumeGatePrompt {
  return { kind: "live", claudeSessionId: ID, conversationName: "maxim", holders, canFork };
}

describe("ResumeLiveDialog — live, held in a runner pane", () => {
  it("names the conversation and its pane, and offers all four options", () => {
    mount(livePrompt([inPane]));
    const dialog = el(RESUME_LIVE_DIALOG_ID)!;
    expect(dialog.getAttribute("data-gate-kind")).toBe("live");
    expect(dialog.textContent).toContain("Conversation already open");
    expect(dialog.textContent).toContain('"maxim · 9b40e22d" is already live');
    expect(el(resumeLiveHolderId(inPane))!.textContent).toBe(
      "runner pane 09ce90d9 on page e7810bcf (pid 49448)",
    );
    for (const id of [
      RESUME_LIVE_GO_TO_PANE_ID,
      RESUME_LIVE_FORK_ID,
      RESUME_LIVE_RESUME_ANYWAY_ID,
      RESUME_LIVE_CANCEL_ID,
    ]) {
      expect(el(id), id).not.toBeNull();
      expect(el(id)!.getAttribute("data-ui-id")).toBe(id);
    }
    expect(el(RESUME_LIVE_RESUME_ANYWAY_ID)!.textContent).toBe(RESUME_ANYWAY_LIVE_TEXT);
    expect(RESUME_ANYWAY_LIVE_TEXT).toMatch(
      /two processes will write one transcript and one worktree/,
    );
  });

  it("'Go to the existing pane' is the default (focused)", () => {
    mount(livePrompt([outside, inPane]));
    expect(document.activeElement).toBe(el(RESUME_LIVE_GO_TO_PANE_ID));
  });

  it.each([
    [RESUME_LIVE_GO_TO_PANE_ID, "onGoToPane"],
    [RESUME_LIVE_FORK_ID, "onFork"],
    [RESUME_LIVE_RESUME_ANYWAY_ID, "onResumeAnyway"],
    [RESUME_LIVE_CANCEL_ID, "onCancel"],
    [RESUME_LIVE_CLOSE_ID, "onCancel"],
  ] as const)("%s calls %s and nothing else", (id, handler) => {
    const h = mount(livePrompt([inPane]));
    click(id);
    for (const [name, fn] of Object.entries(h)) {
      expect(fn, name).toHaveBeenCalledTimes(name === handler ? 1 : 0);
    }
    if (handler === "onGoToPane") expect(h.onGoToPane).toHaveBeenCalledWith(inPane);
  });

  it("Escape is Cancel", () => {
    const h = mount(livePrompt([inPane]));
    act(() => {
      el(RESUME_LIVE_DIALOG_ID)!.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      );
    });
    expect(h.onCancel).toHaveBeenCalledTimes(1);
  });
});

describe("ResumeLiveDialog — live, held outside every runner pane", () => {
  it("names the holder's pid, account and directory, and offers Fork / Resume anyway / Cancel", () => {
    const h = mount(livePrompt([outside]));
    expect(el(resumeLiveHolderId(outside))!.textContent).toBe(
      "pid 54516 · account hotmail · D:/qontinui-root (outside any runner pane)",
    );
    expect(el(RESUME_LIVE_GO_TO_PANE_ID)).toBeNull();
    expect(el(RESUME_LIVE_FORK_ID)).not.toBeNull();
    expect(el(RESUME_LIVE_RESUME_ANYWAY_ID)).not.toBeNull();
    expect(document.activeElement).toBe(el(RESUME_LIVE_CANCEL_ID));
    click(RESUME_LIVE_FORK_ID);
    expect(h.onFork).toHaveBeenCalledTimes(1);
    expect(h.onGoToPane).not.toHaveBeenCalled();
  });

  it("offers no Fork when the prompt does not allow one (the Retry, a non-Claude session)", () => {
    mount(livePrompt([inPane], false));
    expect(el(RESUME_LIVE_FORK_ID)).toBeNull();
    expect(el(RESUME_LIVE_GO_TO_PANE_ID)).not.toBeNull();
  });
});

describe("ResumeLiveDialog — UNKNOWN (D2)", () => {
  const unknown: ResumeGatePrompt = {
    kind: "unknown",
    claudeSessionId: ID,
    conversationName: null,
    holders: [],
    canFork: true,
  };

  it("says liveness could not be read, defaults to Cancel, and labels Resume anyway as a possible second writer", () => {
    const h = mount(unknown);
    const dialog = el(RESUME_LIVE_DIALOG_ID)!;
    expect(dialog.getAttribute("data-gate-kind")).toBe("unknown");
    expect(dialog.textContent).toContain("Could not read which conversations are live");
    expect(dialog.textContent).toContain("(unnamed) · 9b40e22d");
    expect(el(RESUME_LIVE_GO_TO_PANE_ID)).toBeNull();
    expect(document.activeElement).toBe(el(RESUME_LIVE_CANCEL_ID));
    expect(el(RESUME_LIVE_RESUME_ANYWAY_ID)!.textContent).toBe(RESUME_ANYWAY_UNKNOWN_TEXT);
    expect(RESUME_ANYWAY_UNKNOWN_TEXT).toMatch(/may create a second writer/);

    click(RESUME_LIVE_CANCEL_ID);
    expect(h.onCancel).toHaveBeenCalledTimes(1);
    click(RESUME_LIVE_RESUME_ANYWAY_ID);
    expect(h.onResumeAnyway).toHaveBeenCalledTimes(1);
    click(RESUME_LIVE_FORK_ID);
    expect(h.onFork).toHaveBeenCalledTimes(1);
  });

  // Review W7: the explanation names only options the dialog offers.
  it("mentions forking only when Fork is offered", () => {
    mount(unknown);
    expect(el(RESUME_LIVE_DIALOG_ID)!.textContent).toContain("fork it into a new conversation");
    expect(el(RESUME_LIVE_FORK_ID)).not.toBeNull();

    mount({ ...unknown, canFork: false });
    const text = el(RESUME_LIVE_DIALOG_ID)!.textContent ?? "";
    expect(el(RESUME_LIVE_FORK_ID)).toBeNull();
    expect(text).toContain("Cancel and look for it first.");
    expect(text).not.toMatch(/fork/i);
  });
});
