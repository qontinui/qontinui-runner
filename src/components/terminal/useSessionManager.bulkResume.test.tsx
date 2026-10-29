// @vitest-environment jsdom
/**
 * The Session Manager's bulk "resume selected", entered where the panel enters
 * it: the REAL `useSessionManager`, mounted in React. Each selected row goes
 * through the click funnel NON-interactively (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 3), so a live or unknown id is skipped rather than forked, and the run
 * ends with ONE acknowledged summary naming what was resumed and skipped.
 */

import { act, createElement } from "react";
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
vi.mock("./useLiveClaudeSessionNames", () => ({ useLiveClaudeSessionNames: () => new Map() }));

import type { ResumeOutcome } from "./resumeGate";
import { useSessionManager, type UseSessionManagerReturn } from "./useSessionManager";
import type { TranscriptSession } from "./useTranscriptSessions";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new Error("no runner in a unit test"))),
  );
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

function transcript(id: string, name: string): TranscriptSession {
  return {
    session_id: id,
    project_path: "/repo",
    config_dir: "/home/u/.claude-work",
    message_count: 4,
    last_modified: "2026-10-27T10:00:00Z",
    started_at: null,
    first_message_preview: "hello",
    has_plans: false,
    display_name: "hello",
    resume_name: name,
  };
}

const SESSIONS = [
  transcript("aaaaaaaa-0000-0000-0000-000000000001", "alpha"),
  transcript("bbbbbbbb-0000-0000-0000-000000000002", "bravo"),
  transcript("cccccccc-0000-0000-0000-000000000003", "charlie"),
];

function mount(outcomeFor: (id: string) => ResumeOutcome) {
  const onResumeSession = vi.fn(async (s: TranscriptSession, _opts?: { interactive?: boolean }) =>
    outcomeFor(s.session_id),
  );
  const onNotify = vi.fn();
  let manager: UseSessionManagerReturn | null = null;
  function Page() {
    manager = useSessionManager({
      tabs: [],
      sessionStates: {},
      staleTabs: new Set<string>(),
      transcriptSessions: SESSIONS,
      sessionsLoading: false,
      desktopNotify: false,
      onRefreshSessions: () => {},
      onResumeSession,
      onSelectSession: () => {},
      onFocusSessionTab: () => {},
      onNotify,
    });
    return null;
  }
  act(() => root.render(createElement(Page)));
  return { manager: () => manager!, onResumeSession, onNotify };
}

async function bulk(m: ReturnType<typeof mount>, ids: string[]) {
  act(() => {
    for (const id of ids) m.manager().toggleSelect(id);
  });
  let tally: Awaited<ReturnType<UseSessionManagerReturn["bulkResume"]>> | null = null;
  await act(async () => {
    tally = await m.manager().bulkResume();
  });
  return tally!;
}

describe("useSessionManager bulkResume — through the click gate, non-interactively", () => {
  it("passes { interactive: false } for every selected row", async () => {
    const m = mount(() => "skipped-live");
    await bulk(m, [SESSIONS[0].session_id, SESSIONS[1].session_id]);
    expect(m.onResumeSession).toHaveBeenCalledTimes(2);
    for (const call of m.onResumeSession.mock.calls) {
      expect(call[1]).toEqual({ interactive: false });
    }
  });

  it("0 live ids: everything resumed, one success summary", async () => {
    const m = mount(() => "resumed");
    const tally = await bulk(m, [SESSIONS[0].session_id]);
    expect(tally.resumed).toEqual(["alpha · aaaaaaaa"]);
    expect(m.onNotify).toHaveBeenCalledTimes(1);
    expect(m.onNotify.mock.calls[0][0]).toEqual({
      message: "Resume selected — 1 resumed",
      type: "success",
    });
  });

  it("1 live id: skipped and named; the others still resume", async () => {
    const m = mount((id) => (id === SESSIONS[0].session_id ? "skipped-live" : "resumed"));
    const tally = await bulk(
      m,
      SESSIONS.map((s) => s.session_id),
    );
    expect(tally.live).toEqual(["alpha · aaaaaaaa"]);
    expect(tally.resumed).toHaveLength(2);
    expect(m.onNotify.mock.calls[0][0].message).toMatch(
      /^Resume selected — 2 resumed, 1 already live: alpha · aaaaaaaa$/,
    );
  });

  it("many live ids: skipped, counted and named in the summary", async () => {
    const live = new Set([SESSIONS[1].session_id, SESSIONS[2].session_id]);
    const m = mount((id) => (live.has(id) ? "skipped-live" : "resumed"));
    const tally = await bulk(
      m,
      SESSIONS.map((s) => s.session_id),
    );
    expect(tally.resumed).toEqual(["alpha · aaaaaaaa"]);
    expect([...tally.live].sort()).toEqual(["bravo · bbbbbbbb", "charlie · cccccccc"]);
    const { message, type } = m.onNotify.mock.calls[0][0];
    expect(type).toBe("error");
    expect(message).toMatch(/^Resume selected — 1 resumed, 2 already live: /);
    expect(message).toContain("bravo · bbbbbbbb");
    expect(message).toContain("charlie · cccccccc");
  });

  it("an unreadable registry: every row is liveness-unknown, none resumed", async () => {
    const m = mount(() => "skipped-unknown");
    const tally = await bulk(
      m,
      SESSIONS.map((s) => s.session_id),
    );
    expect(tally.resumed).toEqual([]);
    expect(tally.unknown).toHaveLength(3);
    expect(m.onNotify.mock.calls[0][0].message).toMatch(
      /^Resume selected — 0 resumed, 3 liveness unknown: /,
    );
  });

  it("clears the selection, and posts nothing when nothing was selected", async () => {
    const m = mount(() => "resumed");
    await bulk(m, []);
    expect(m.onResumeSession).not.toHaveBeenCalled();
    expect(m.onNotify).not.toHaveBeenCalled();
    await bulk(m, [SESSIONS[0].session_id]);
    expect(m.manager().selectedIds.size).toBe(0);
  });
});
