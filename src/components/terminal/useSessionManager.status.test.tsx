// @vitest-environment jsdom
/**
 * Wiring: the REAL `useSessionManager`, mounted in React, classifies every row
 * through `sessionStatusModel` from the sources the runner serves — the live
 * registry (`terminal_claude_session_list_live`, with `runnerManaged`), the
 * external scan, the session history's finished marker, and digests requested
 * BY ID (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 1). Only the Tauri boundary is faked.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@/lib/runner-api", () => ({ resolvePort: () => 0 }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (_key: string, fallback: unknown) => fallback,
    setJSON: () => {},
    getItem: () => null,
    setItem: () => {},
  },
}));

import type { TerminalTab } from "./useTerminalManager";
import type { TranscriptSession } from "./useTranscriptSessions";
import { useSessionManager, type UseSessionManagerReturn } from "./useSessionManager";
import { __resetLiveClaudeSessionsForTest } from "./useLiveClaudeSessions";

interface RegistryRow {
  sessionId: string;
  pid: number;
  status: string;
  runnerManaged: boolean;
}

interface Backend {
  registry: RegistryRow[] | "fail";
  externalPids: number[] | "degraded";
  finished?: Record<string, boolean>;
}

let container: HTMLDivElement;
let root: Root;

function transcript(id: string, i = 0): TranscriptSession {
  return {
    session_id: id,
    project_path: "/work/repo",
    config_dir: "/home/u/.claude-gmail",
    message_count: 3,
    last_modified: new Date(Date.UTC(2026, 9, 13, 11, 0, 0) - i * 1000).toISOString(),
    first_message_preview: "hello",
    has_plans: false,
    display_name: id,
  } as TranscriptSession;
}

function serve(backend: Backend) {
  invokeMock.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "terminal_claude_session_list_live":
        if (backend.registry === "fail") return Promise.reject(new Error("snapshot failed"));
        return Promise.resolve({
          success: true,
          data: {
            sessions: backend.registry.map((r) => ({
              ...r,
              name: "",
              account: { label: "gmail", wrapper: "clg" },
              workingDir: "/work/repo",
              kind: "interactive",
              startedAt: 1,
              updatedAt: 1,
              resumeCommand: "",
            })),
          },
        });
      case "transcript_find_external_processes":
        if (backend.externalPids === "degraded") return Promise.resolve({ success: false });
        return Promise.resolve({
          success: true,
          data: {
            processes: backend.externalPids.map((pid) => ({ pid, working_directory: null })),
            stale: false,
            observed_at_ms: 0,
          },
        });
      case "session_info_list": {
        // The finished marker rides each row's lifecycle; a session with no
        // marker recorded carries `finishedAt: null`.
        const ids = (args?.ids as string[]) ?? [];
        return Promise.resolve({
          status: "ok",
          sessions: ids.map((claudeSessionId) => ({
            available: true,
            identity: { claudeSessionId },
            name: { value: null, source: null },
            account: {},
            placement: {},
            lifecycle: { finishedAt: backend.finished?.[claudeSessionId] ? 1 : null },
          })),
          notFound: [],
          branches: [],
        });
      }
      case "transcript_session_digests": {
        const ids = (args?.sessionIds as string[]) ?? [];
        return Promise.resolve({
          success: true,
          data: ids.map((id) => ({
            session_id: id,
            config_dir: "",
            project_path: "",
            last_message_type: "assistant",
            last_message_timestamp: new Date().toISOString(),
            last_message_preview: `digest of ${id}`,
            last_assistant_had_tool_use: false,
            likely_frozen: false,
            work_summary_hint: "",
          })),
        });
      }
      default:
        return Promise.resolve(null);
    }
  });
}

async function flush() {
  await act(async () => {
    for (let i = 0; i < 20; i++) await Promise.resolve();
  });
}

async function mount(
  transcripts: TranscriptSession[],
  opts: { tabs?: TerminalTab[]; visible?: string[] } = {},
): Promise<() => UseSessionManagerReturn> {
  let manager: UseSessionManagerReturn | null = null;
  const visible = new Set(opts.visible ?? []);
  function Page() {
    manager = useSessionManager({
      tabs: opts.tabs ?? [],
      sessionStates: {},
      staleTabs: new Set<string>(),
      transcriptSessions: transcripts,
      sessionsLoading: false,
      desktopNotify: false,
      onRefreshSessions: () => {},
      onResumeSession: () => {},
      onSelectSession: () => {},
      onFocusSessionTab: () => {},
      visibleTabIds: visible,
    });
    return null;
  }
  act(() => root.render(createElement(Page)));
  await flush();
  await flush();
  return () => manager!;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new Error("no runner in a unit test"))),
  );
  invokeMock.mockReset();
  __resetLiveClaudeSessionsForTest();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

describe("useSessionManager → sessionStatusModel wiring", () => {
  it("a session with no tab and no live registry pid reads Not running while an unrelated unmanaged claude runs", async () => {
    // pid 4242 is another session's registered process; 999 has no registry
    // row at all. Under the old count heuristic either one flipped every
    // recently-touched transcript to "Active (External)".
    serve({
      registry: [{ sessionId: "s-other", pid: 4242, status: "busy", runnerManaged: false }],
      externalPids: [999, 4242],
    });
    const get = await mount([transcript("s-1"), transcript("s-other", 1)]);
    const s1 = get().sessions.find((s) => s.sessionId === "s-1")!;
    expect(s1.status.location).toBe("not-running");
    const other = get().sessions.find((s) => s.sessionId === "s-other")!;
    expect(other.status.location).toBe("outside-runner");
    // 999 is counted, never attributed.
    expect(get().unidentifiedCount).toBe(1);
  });

  it("an Outside-runner row takes its activity from registry status", async () => {
    serve({
      registry: [{ sessionId: "s-1", pid: 7, status: "waiting", runnerManaged: false }],
      externalPids: [7],
    });
    const get = await mount([transcript("s-1")]);
    expect(get().sessions[0].status).toMatchObject({
      location: "outside-runner",
      activity: "needs-you",
    });
    expect(get().needsInputCount).toBe(1);
  });

  it("a runner-owned process with no tab on this page is Hidden here, never Outside runner", async () => {
    serve({
      registry: [{ sessionId: "s-1", pid: 7, status: "busy", runnerManaged: true }],
      externalPids: [],
    });
    const get = await mount([transcript("s-1")]);
    expect(get().sessions[0].status).toMatchObject({
      location: "hidden-here",
      activity: "working",
    });
    expect(get().outsideWorkingCount).toBe(0);
    expect(get().workingCount).toBe(1);
  });

  it("a failed registry read places a tab-less row Unknown, never Not running", async () => {
    serve({ registry: "fail", externalPids: [] });
    const get = await mount([transcript("s-1")]);
    expect(get().sessions[0].status.location).toBe("unknown");
    expect(get().unidentifiedCount).toBeNull();
  });

  it("a degraded external scan places an unmatched row Unknown", async () => {
    serve({ registry: [], externalPids: "degraded" });
    const get = await mount([transcript("s-1")]);
    expect(get().sessions[0].status.location).toBe("unknown");
  });

  it("a tab here is On screen when visible, Hidden here when not", async () => {
    serve({ registry: [], externalPids: [] });
    const tabs: TerminalTab[] = [
      { id: "t-a", title: "a", pid: 1, isAlive: true, exitCode: null, claudeSessionId: "s-a" },
      { id: "t-b", title: "b", pid: 2, isAlive: true, exitCode: null, claudeSessionId: "s-b" },
    ];
    const get = await mount([transcript("s-a"), transcript("s-b", 1)], { tabs, visible: ["t-a"] });
    const loc = (id: string) => get().sessions.find((s) => s.sessionId === id)!.status.location;
    expect(loc("s-a")).toBe("on-screen");
    expect(loc("s-b")).toBe("hidden-here");
  });

  it("reads the finished marker from the session history", async () => {
    serve({ registry: [], externalPids: [], finished: { "s-1": true } });
    const get = await mount([transcript("s-1"), transcript("s-2", 1)]);
    const act_ = (id: string) => get().sessions.find((s) => s.sessionId === id)!.status.activity;
    expect(act_("s-1")).toBe("finished");
    // No marker recorded ⇒ not a claim of "not finished": the process is gone.
    expect(act_("s-2")).toBe("exited");
  });

  it("the 51st and the 200th live session still get a digest — by id, in chunks of ≤100", async () => {
    const ids = Array.from({ length: 200 }, (_, i) => `s-${i}`);
    serve({
      registry: ids.map((sessionId, i) => ({
        sessionId,
        pid: 1000 + i,
        status: "",
        runnerManaged: false,
      })),
      externalPids: ids.map((_, i) => 1000 + i),
    });
    const get = await mount(ids.map((id, i) => transcript(id, i)));
    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "transcript_session_digests");
    const requested = new Set<string>();
    for (const [, args] of calls) {
      const chunk = (args as { sessionIds: string[] }).sessionIds;
      expect(chunk.length).toBeLessThanOrEqual(100);
      for (const id of chunk) requested.add(id);
      expect(args).not.toHaveProperty("maxSessions");
    }
    expect(requested.has("s-50")).toBe(true);
    expect(requested.has("s-199")).toBe(true);
    const row = (id: string) => get().sessions.find((s) => s.sessionId === id)!;
    expect(row("s-50").lastMessagePreview).toBe("digest of s-50");
    expect(row("s-199").lastMessagePreview).toBe("digest of s-199");
  });
});
