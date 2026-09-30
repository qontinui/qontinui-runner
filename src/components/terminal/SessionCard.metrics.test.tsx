/**
 * Session Manager card: context %, session cost and 5-hour headroom (plan
 * `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
 * Phase 7).
 *
 * The runner's vitest env is `node` (no jsdom), so the card renders through
 * `react-dom/server` as `StatusStrip.multiZone.test.tsx` does. The metrics
 * store is seeded directly; `useSyncExternalStore`'s server snapshot reads it.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@qontinui/ui-bridge", () => ({ useUIElement: () => ({ ref: () => {} }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: () => Promise.resolve([]) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

import { SessionCard } from "./SessionCard";
import { AgentMetricsStrip } from "./AgentMetricsStrip";
import {
  CONTEXT_STALE_AFTER_MS,
  applyAgentMetrics,
  resetAgentMetricsStore,
  type SessionMetrics,
} from "./agentMetrics";
import type { UnifiedSession } from "./useSessionManager";

const NOW = 1_780_000_000_000;

function metrics(over: Partial<SessionMetrics> = {}): SessionMetrics {
  return {
    contextUsedPct: 42,
    contextTokens: 84_000,
    contextWindow: 200_000,
    contextSource: "transcript",
    contextObservedAtMs: NOW - 30_000,
    costUsd: null,
    fiveHourPct: 63,
    fiveHourResetsAt: null,
    sevenDayPct: null,
    headroomSource: "oauth_probe",
    headroomObservedAtMs: NOW - 60_000,
    ...over,
  };
}

function session(over: Partial<UnifiedSession> = {}): UnifiedSession {
  return {
    sessionId: "b770ae37-1ffa-4888-a5d1-89d058307adf",
    accountLabel: "default",
    configDir: "",
    projectPath: "/work/repo",
    projectLabel: "repo",
    displayName: "a session",
    resumeName: null,
    registryName: null,
    firstMessagePreview: null,
    messageCount: 3,
    lastModified: "",
    hasPlans: false,
    liveStatus: "active-in-zone",
    zoneTabId: "term-1",
    zoneIndex: 0,
    startedAt: null,
    durationMs: null,
    lastMessageType: "",
    lastMessageTimestamp: "",
    lastMessagePreview: "",
    workSummaryHint: "",
    likelyFrozen: false,
    isOrphaned: false,
    ...over,
  } as UnifiedSession;
}

const noop = () => {};

function renderCard(s: UnifiedSession): string {
  return renderToStaticMarkup(
    <SessionCard
      session={s}
      isSelected={false}
      isChecked={false}
      selectionMode={false}
      isPinned={false}
      sessionLabel={null}
      onResume={noop}
      onOpen={noop}
      onViewTranscript={noop}
      onCopyId={noop}
      onToggleSelect={noop}
      onTogglePin={noop}
      onSetLabel={noop}
    />,
  );
}

/** The `<span data-metric="<name>" …>…</span>` cell, attributes included. */
function cell(html: string, name: string): string {
  const start = html.indexOf(`data-metric="${name}"`);
  expect(start, `cell ${name} rendered`).toBeGreaterThan(-1);
  const open = html.lastIndexOf("<span", start);
  let depth = 0;
  const re = /<span\b|<\/span>/g;
  re.lastIndex = open;
  for (let m = re.exec(html); m; m = re.exec(html)) {
    depth += m[0] === "</span>" ? -1 : 1;
    if (depth === 0) return html.slice(open, m.index + m[0].length);
  }
  throw new Error("unbalanced");
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(NOW);
  resetAgentMetricsStore();
});

afterEach(() => {
  resetAgentMetricsStore();
  vi.useRealTimers();
});

describe("SessionCard metrics row", () => {
  it("renders present readings with their source on hover", () => {
    applyAgentMetrics({ terminalId: "term-1", metrics: metrics() });
    const html = renderCard(session());
    const ctx = cell(html, "context");
    expect(ctx).toContain("42%");
    expect(ctx).toContain('data-metric-source="transcript"');
    expect(ctx).toContain("from transcript, 30s ago");
    expect(ctx).not.toContain("data-metric-stale");
    const five = cell(html, "headroom");
    expect(five).toContain("63%");
    expect(five).toContain("from OAuth usage probe, 1m ago");
  });

  it("renders '—' (never 0) for a terminal the runner reported nothing for", () => {
    const html = renderCard(session());
    for (const name of ["context", "cost", "headroom"]) {
      const c = cell(html, name);
      expect(c).toContain('data-metric-absent="true"');
      expect(c).toContain("—");
      expect(c).not.toMatch(/>0%<|\$0\.00/);
    }
  });

  it("session cost renders '—' — it has no source since the statusline was dropped", () => {
    applyAgentMetrics({ terminalId: "term-1", metrics: metrics() });
    const c = cell(renderCard(session()), "cost");
    expect(c).toContain("—");
    expect(c).toContain('data-metric-absent="true"');
  });

  it("greys a stale reading and shows its age", () => {
    applyAgentMetrics({
      terminalId: "term-1",
      metrics: metrics({ contextObservedAtMs: NOW - CONTEXT_STALE_AFTER_MS - 5 * 60_000 }),
    });
    const ctx = cell(renderCard(session()), "context");
    expect(ctx).toContain('data-metric-stale="true"');
    expect(ctx).toContain("opacity-60");
    expect(ctx).toContain("42%");
    expect(ctx).toContain("· 15m");
    expect(ctx).toContain("(stale)");
  });

  it("omits the row for a session that is not open in a runner terminal", () => {
    const html = renderCard(session({ zoneTabId: null, liveStatus: "dormant" }));
    expect(html).not.toContain("data-agent-metrics");
  });
});

describe("AgentMetricsStrip source labels", () => {
  it("names the grid fallback and the cached usage snapshot", () => {
    const html = renderToStaticMarkup(
      <AgentMetricsStrip
        metrics={metrics({ contextSource: "grid", headroomSource: "cached_usage" })}
        nowMs={NOW}
      />,
    );
    expect(cell(html, "context")).toContain('data-metric-source="screen scan"');
    expect(cell(html, "headroom")).toContain('data-metric-source="cached usage snapshot"');
  });

  it("an unknown age is stale, labelled '· age ?'", () => {
    const html = renderToStaticMarkup(
      <AgentMetricsStrip metrics={metrics({ headroomObservedAtMs: null })} nowMs={NOW} />,
    );
    const five = cell(html, "headroom");
    expect(five).toContain('data-metric-stale="true"');
    expect(five).toContain("· age ?");
  });
});
