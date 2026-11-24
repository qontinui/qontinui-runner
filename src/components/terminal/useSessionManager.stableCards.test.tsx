// @vitest-environment jsdom
/**
 * Regression: a terminal title tick must not re-render every Session Manager
 * card (coord finding 8719a700).
 *
 * With ~27 live sessions the open panel pinned the runner window's main thread
 * at 100% and typed input took ~30 s to echo. Measured in the live window with
 * a MutationObserver: a 5 s timer fired after 21.5 s; in one later sample the
 * ~100 bulk-select checkboxes took 18,360 `type` rewrites (≈180 each, old value
 * == new value) — every card re-rendering back to back. Two inputs defeated
 * `memo(SessionCard)`:
 *
 *  1. the card actions (`openSession`, `viewTranscript`, `closeSession`,
 *     `resumeSession`, `refresh`) were rebuilt whenever the provider passed new
 *     inline callbacks or a new `tabs` array;
 *  2. every row OBJECT was rebuilt whenever `tabs` changed — and `tabs` changes
 *     on every OSC 0/2 title a working Claude session reports (and the
 *     transcript list, refetched every 30 s, hands every row a new
 *     `_transcript` object even when nothing changed).
 *
 * These tests drive the REAL `useSessionManager` (and, in the last case, the
 * real `SessionManagerPanel`) with exactly that shape of update.
 */

import { act, createElement, memo } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("@/lib/runner-api", () => ({ resolvePort: () => 0 }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (_key: string, fallback: unknown) => fallback,
    setJSON: () => {},
    getItem: () => null,
    setItem: () => {},
  },
}));
// One Map for every render, as the real hook returns its state between polls —
// a fresh Map per render would recompute the row memos on every render.
vi.mock("./useLiveClaudeSessionNames", () => {
  const names = new Map<string, string>();
  return { useLiveClaudeSessionNames: () => names };
});
vi.mock("./StewardControl", () => ({ StewardControl: () => null }));
vi.mock("../worktrees/WorktreesPanel", () => ({ WorktreesPanel: () => null }));
vi.mock("./FleetSessionPicker", () => ({ FleetSessionPicker: () => null }));
vi.mock("./PastSessionsView", () => ({ PastSessionsView: () => null }));

// A card that only counts its renders, memoized exactly as the real one is
// (`memo(SessionCardInner)`, default shallow compare) — so a render here means
// the panel handed the card at least one changed prop.
const cardRenders = new Map<string, number>();
vi.mock("./SessionCard", () => ({
  SessionCard: memo(function CountingCard(props: { session: { sessionId: string } }) {
    const id = props.session.sessionId;
    cardRenders.set(id, (cardRenders.get(id) ?? 0) + 1);
    return null;
  }),
}));

import { SessionManagerPanel } from "./SessionManagerPanel";
import type { TerminalTab } from "./useTerminalManager";
import type { TranscriptSession } from "./useTranscriptSessions";
import {
  useSessionManager,
  type UseSessionManagerParams,
  type UseSessionManagerReturn,
} from "./useSessionManager";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new Error("no runner in a unit test"))),
  );
  cardRenders.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

/** A Claude session with a transcript, live in tab `tab-claude`. */
const transcript: TranscriptSession = {
  session_id: "sess-1",
  project_path: "D:/work/project",
  config_dir: "C:/claude/.claude-gmail",
  message_count: 12,
  last_modified: "2026-11-05T10:00:00.000Z",
  started_at: "2026-11-05T09:00:00.000Z",
  first_message_preview: "fix the bug",
  has_plans: false,
  display_name: "fix the bug",
};

function claudeTab(title: string): TerminalTab {
  return {
    id: "tab-claude",
    title,
    pid: 1,
    isAlive: true,
    exitCode: null,
    claudeSessionId: "sess-1",
  };
}

function shellTab(title: string): TerminalTab {
  return { id: "tab-shell", title, pid: 2, isAlive: true, exitCode: null };
}

const sessionStates = { "tab-claude": "working", "tab-shell": "idle" } as const;
const staleTabs = new Set<string>();
const transcriptSessions = [transcript];

/**
 * The params the provider builds on ONE render: a fresh `tabs` array and fresh
 * inline arrows every time, as `TerminalSessionContext` does.
 */
function params(
  titles: { claude: string; shell: string },
  spies: Partial<UseSessionManagerParams> = {},
): UseSessionManagerParams {
  return {
    tabs: [claudeTab(titles.claude), shellTab(titles.shell)],
    sessionStates,
    staleTabs,
    transcriptSessions,
    sessionsLoading: false,
    desktopNotify: false,
    onRefreshSessions: () => {},
    onResumeSession: () => {},
    onSelectSession: () => {},
    onFocusSessionTab: () => {},
    onCloseSessionTab: () => {},
    ...spies,
  };
}

/** Mount the real hook; each `render` is one provider render with `p`. */
function mountHook() {
  let current: UseSessionManagerReturn | null = null;
  function HookOnly(props: { p: UseSessionManagerParams }) {
    current = useSessionManager(props.p);
    return null;
  }
  return {
    render(p: UseSessionManagerParams) {
      act(() => root.render(createElement(HookOnly, { p })));
    },
    get manager(): UseSessionManagerReturn {
      if (!current) throw new Error("hook not mounted");
      return current;
    },
  };
}

describe("useSessionManager — a title tick keeps the cards' props stable", () => {
  it("keeps every card action's identity across provider renders", () => {
    const hook = mountHook();
    hook.render(params({ claude: "✳ a", shell: "bash" }));
    const first = hook.manager;

    hook.render(params({ claude: "✶ a", shell: "bash" }));
    const second = hook.manager;

    expect(second.openSession).toBe(first.openSession);
    expect(second.viewTranscript).toBe(first.viewTranscript);
    expect(second.closeSession).toBe(first.closeSession);
    expect(second.resumeSession).toBe(first.resumeSession);
    expect(second.refresh).toBe(first.refresh);
  });

  it("reuses a row whose fields did not change, and rebuilds one whose did", () => {
    const hook = mountHook();
    hook.render(params({ claude: "✳ a", shell: "bash" }));
    const before = new Map(hook.manager.sessions.map((s) => [s.sessionId, s]));

    // The Claude tab's title ticks (its transcript row does not show the tab
    // title), and the shell tab's title — which IS its row's display name —
    // changes too.
    hook.render(params({ claude: "✶ a", shell: "vim notes" }));
    const after = new Map(hook.manager.sessions.map((s) => [s.sessionId, s]));

    expect(after.get("sess-1")).toBe(before.get("sess-1"));
    expect(after.get("tab-shell")).not.toBe(before.get("tab-shell"));
    expect(after.get("tab-shell")!.displayName).toBe("vim notes");
  });

  it("runs the LATEST provider callbacks, not the ones from the first render", () => {
    const firstFocus = vi.fn();
    const firstSelect = vi.fn();
    const firstClose = vi.fn();
    const hook = mountHook();
    hook.render(
      params(
        { claude: "a", shell: "bash" },
        {
          onFocusSessionTab: firstFocus,
          onSelectSession: firstSelect,
          onCloseSessionTab: firstClose,
        },
      ),
    );
    const latestFocus = vi.fn();
    const latestSelect = vi.fn();
    const latestClose = vi.fn();
    hook.render(
      params(
        { claude: "b", shell: "bash" },
        {
          onFocusSessionTab: latestFocus,
          onSelectSession: latestSelect,
          onCloseSessionTab: latestClose,
        },
      ),
    );

    const m = hook.manager;
    const claudeRow = m.sessions.find((s) => s.sessionId === "sess-1")!;
    act(() => m.openSession(claudeRow));
    act(() => m.viewTranscript(claudeRow));
    act(() => m.closeSession(claudeRow));

    expect(latestFocus).toHaveBeenCalledWith("sess-1", "tab-claude", "transcript");
    expect(latestSelect).toHaveBeenCalledWith("sess-1");
    expect(latestClose).toHaveBeenCalledWith("tab-claude");
    expect(firstFocus).not.toHaveBeenCalled();
    expect(firstSelect).not.toHaveBeenCalled();
    expect(firstClose).not.toHaveBeenCalled();
  });
});

describe("SessionManagerPanel — a title tick re-renders only the card it changes", () => {
  function Page(props: { p: UseSessionManagerParams }) {
    const m = useSessionManager(props.p);
    return createElement(SessionManagerPanel, {
      manager: m,
      selectedSessionId: null,
      view: "live",
      onViewChange: () => {},
    });
  }

  it("does not re-render the card of a session whose row did not change", () => {
    act(() => root.render(createElement(Page, { p: params({ claude: "✳ a", shell: "bash" }) })));
    const claudeBefore = cardRenders.get("sess-1") ?? 0;
    const shellBefore = cardRenders.get("tab-shell") ?? 0;
    expect(claudeBefore).toBeGreaterThan(0);
    expect(shellBefore).toBeGreaterThan(0);

    // Ten spinner ticks of the working Claude session's title.
    for (const glyph of ["✶", "✻", "✽", "✻", "✶", "✳", "✢", "·", "✢", "✳"]) {
      act(() =>
        root.render(createElement(Page, { p: params({ claude: `${glyph} a`, shell: "bash" }) })),
      );
    }

    expect(cardRenders.get("sess-1")).toBe(claudeBefore);
    expect(cardRenders.get("tab-shell")).toBe(shellBefore);

    // A change the shell's card DOES show still reaches it.
    act(() =>
      root.render(createElement(Page, { p: params({ claude: "✳ a", shell: "vim notes" }) })),
    );
    expect(cardRenders.get("tab-shell")).toBe(shellBefore + 1);
    expect(cardRenders.get("sess-1")).toBe(claudeBefore);
  });

  it("does not re-render on a quiet transcript poll, but does on a real change", () => {
    const titles = { claude: "✳ a", shell: "bash" };
    act(() => root.render(createElement(Page, { p: params(titles) })));
    const claudeBefore = cardRenders.get("sess-1") ?? 0;

    // The 30 s refetch: a NEW list of NEW objects, equal in value.
    act(() =>
      root.render(
        createElement(Page, { p: params(titles, { transcriptSessions: [{ ...transcript }] }) }),
      ),
    );
    expect(cardRenders.get("sess-1")).toBe(claudeBefore);

    // A refetch that carries a new message count is a change the card shows.
    act(() =>
      root.render(
        createElement(Page, {
          p: params(titles, { transcriptSessions: [{ ...transcript, message_count: 13 }] }),
        }),
      ),
    );
    expect(cardRenders.get("sess-1")).toBe(claudeBefore + 1);
  });
});
