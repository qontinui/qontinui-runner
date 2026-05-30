/**
 * Tests for the result-card builders.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom, no React
 * Testing Library) — see ResultCardContext.test.ts / useNow1Hz.test.tsx. We
 * therefore test the PURE builder functions only: assert the returned spec's
 * `title` / `subtitle` / `sections` / footer fields. The `/history` card's
 * `body` is a React node — we only assert it is truthy, never render it.
 */

import { describe, it, expect } from "vitest";

import { buildMetricsCardSpec, buildHistoryCardSpec } from "./builders";
import type { BuildMetricsCardInput, HistoryCardEntry } from "./builders";
import type { SessionState } from "../useZoneLayout";
import type { TerminalTab } from "../useTerminalManager";

function tab(id: string, title = id): TerminalTab {
  return { id, title, pid: null } as TerminalTab;
}

const baseMetrics = {
  totalApprovals: 5,
  totalRejections: 2,
  totalBroadcasts: 1,
  sessionsCreated: 7,
};

function baseInput(overrides: Partial<BuildMetricsCardInput> = {}): BuildMetricsCardInput {
  return {
    metrics: baseMetrics,
    sessionStates: {},
    tabs: [],
    autoApproveCount: 0,
    autoRestartCount: 0,
    ...overrides,
  };
}

describe("buildMetricsCardSpec", () => {
  it("titles the card 'Session Metrics'", () => {
    expect(buildMetricsCardSpec(baseInput()).title).toBe("Session Metrics");
  });

  it("first counters row is Sessions created matching metrics.sessionsCreated", () => {
    const spec = buildMetricsCardSpec(baseInput());
    const row = spec.sections![0].rows[0];
    expect(row.label).toBe("Sessions created");
    expect(row.value).toBe("7");
    expect(row.labelColor).toBe("#a9b1d6");
    expect(row.valueColor).toBeUndefined();
  });

  it("emits the approvals/rejections/broadcasts counters in order", () => {
    const rows = buildMetricsCardSpec(baseInput()).sections![0].rows;
    expect(rows.map((r) => [r.label, r.value])).toEqual([
      ["Sessions created", "7"],
      ["Approvals sent", "5"],
      ["Rejections sent", "2"],
      ["Broadcasts sent", "1"],
    ]);
  });

  it("omits Auto-approved / Auto-restarted when counts are 0", () => {
    const rows = buildMetricsCardSpec(baseInput()).sections![0].rows;
    expect(rows.find((r) => r.label === "Auto-approved")).toBeUndefined();
    expect(rows.find((r) => r.label === "Auto-restarted")).toBeUndefined();
  });

  it("includes Auto-approved / Auto-restarted when counts > 0", () => {
    const rows = buildMetricsCardSpec(
      baseInput({ autoApproveCount: 3, autoRestartCount: 4 }),
    ).sections![0].rows;
    const auto = rows.filter((r) => r.label.startsWith("Auto-"));
    expect(auto).toEqual([
      { label: "Auto-approved", labelColor: "#9ece6a", value: "3" },
      { label: "Auto-restarted", labelColor: "#7dcfff", value: "4" },
    ]);
  });

  it("derives Current state counts from sessionStates + tabs (2 working, 1 error)", () => {
    const sessionStates: Record<string, SessionState> = {
      a: "working",
      b: "working",
      c: "error",
      // d has no entry -> defaults to idle (omitted from the section)
    };
    const spec = buildMetricsCardSpec(
      baseInput({
        sessionStates,
        tabs: [tab("a"), tab("b"), tab("c"), tab("d")],
      }),
    );
    const current = spec.sections!.find((s) => s.heading === "Current state")!;
    const byLabel = Object.fromEntries(current.rows.map((r) => [r.label, r.value]));
    expect(byLabel["Working"]).toBe("2");
    expect(byLabel["Errored"]).toBe("1");
    expect(byLabel["Waiting for input"]).toBe("0");
    expect(byLabel["Idle (done responding)"]).toBe("0");
    // The standalone "idle" row is intentionally OMITTED (matches ZSB) —
    // only the four working/needs-input/completed/error rows are present.
    expect(current.rows).toHaveLength(4);
    expect(current.rows.find((r) => r.label === "Idle")).toBeUndefined();
  });

  it("omits Time-in-state when stateTimeAccum is all-zero", () => {
    const stateTimeAccum: Record<SessionState, number> = {
      idle: 0,
      working: 0,
      "needs-input": 0,
      completed: 0,
      error: 0,
    };
    const spec = buildMetricsCardSpec(baseInput({ stateTimeAccum }));
    expect(spec.sections!.find((s) => s.heading === "Time in state")).toBeUndefined();
  });

  it("includes + formats Time-in-state when working > 1000ms", () => {
    const stateTimeAccum: Record<SessionState, number> = {
      idle: 0,
      working: 65_000, // 1m5s
      "needs-input": 0,
      completed: 0,
      error: 0,
    };
    const spec = buildMetricsCardSpec(baseInput({ stateTimeAccum }));
    const section = spec.sections!.find((s) => s.heading === "Time in state")!;
    expect(section).toBeDefined();
    // STATE_LABELS.working === "working"; 65000ms -> "1m5s"
    expect(section.rows).toEqual([{ label: "working", labelColor: "#7aa2f7", value: "1m5s" }]);
  });

  it("footer is 'Copy summary as Markdown' when tabs + assignments present", () => {
    const spec = buildMetricsCardSpec(
      baseInput({ tabs: [tab("a")], assignments: { 0: "a" } }),
    );
    expect(spec.footer?.label).toBe("Copy summary as Markdown");
  });

  it("footer is undefined when assignments absent", () => {
    const spec = buildMetricsCardSpec(baseInput({ tabs: [tab("a")] }));
    expect(spec.footer).toBeUndefined();
  });

  it("footer is undefined when tabs absent", () => {
    // tabs defaults to [] (truthy), so explicitly clear assignments to prove
    // the AND guard; an empty-tabs+assignments still produces a footer.
    const spec = buildMetricsCardSpec(baseInput({ tabs: [], assignments: undefined }));
    expect(spec.footer).toBeUndefined();
  });

  it("Top Keywords section is omitted when there is no output", () => {
    const spec = buildMetricsCardSpec(baseInput());
    expect(spec.sections!.find((s) => s.heading === "Top Keywords")).toBeUndefined();
  });

  it("Top Keywords section is present + sorted when output has repeated words", () => {
    const lastOutputLines = {
      a: ["deploy deploy build", "deploy verify"],
    };
    const spec = buildMetricsCardSpec(baseInput({ lastOutputLines }));
    const section = spec.sections!.find((s) => s.heading === "Top Keywords")!;
    expect(section).toBeDefined();
    expect(section.rows[0]).toEqual({ label: "deploy", labelColor: "#a9b1d6", value: "3" });
  });
});

describe("buildHistoryCardSpec", () => {
  const entries: HistoryCardEntry[] = [
    { time: 1_700_000_000_000, type: "spawn", session: "alpha", zone: 0, color: "#7aa2f7" },
    { time: 1_700_000_060_000, type: "error", session: "beta", color: "#f7768e" },
  ];

  it("titles the card 'Event History' with the count as subtitle", () => {
    const spec = buildHistoryCardSpec(entries);
    expect(spec.title).toBe("Event History");
    expect(spec.subtitle).toBe("(2)");
  });

  it("builds a truthy React body node", () => {
    expect(buildHistoryCardSpec(entries).body).toBeTruthy();
  });

  it("renders subtitle '(0)' for an empty list and still has a body", () => {
    const spec = buildHistoryCardSpec([]);
    expect(spec.subtitle).toBe("(0)");
    expect(spec.body).toBeTruthy();
  });
});
