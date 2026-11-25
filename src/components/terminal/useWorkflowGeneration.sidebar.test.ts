/**
 * The Session Manager sidebar's open state as the page reads it: through
 * `useWorkflowGeneration`, the hook `TerminalSessionContext` takes
 * `workflowGen.showSidebar` from. In tmux work mode the sidebar is the only
 * navigator to the off-screen sessions (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 4), so a reload — a fresh
 * mount of the page's hooks — must bring it back open if it was left open, and
 * keep each page's and each window's state its own.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return hooksHarness.reactMock;
});

const store = vi.hoisted(() => new Map<string, unknown>());
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (key: string, fallback: unknown) => (store.has(key) ? store.get(key) : fallback),
    setJSON: (key: string, value: unknown) => {
      store.set(key, value);
    },
    getItem: () => null,
    setItem: () => {},
  },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import { useWorkflowGeneration } from "./useWorkflowGeneration";

/**
 * A fresh mount of the page's workflow state — what a reload amounts to. The
 * returned function re-renders that same mount (committing a state change).
 */
function mountPage(pageId: string, windowLabel?: string) {
  harness.reset();
  const render = () =>
    harness.renderSettled(() => {
      harness.beginRender();
      return useWorkflowGeneration({
        activeId: null,
        tabs: [],
        loadMessages: async () => [],
        pageId,
        windowLabel,
      });
    });
  render();
  return render;
}

beforeEach(() => {
  store.clear();
  harness.reset();
});

describe("useWorkflowGeneration — the session manager sidebar", () => {
  it("reloads open after it was opened, per page and per window", () => {
    const page = mountPage("p1");
    expect(page().showSidebar).toBe(false);
    page().setShowSidebar(true);
    expect(page().showSidebar).toBe(true);

    expect(mountPage("p1")().showSidebar).toBe(true);
    expect(mountPage("p2")().showSidebar).toBe(false);
    expect(mountPage("p1", "term-2")().showSidebar).toBe(false);
  });
});
