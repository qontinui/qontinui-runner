/**
 * builders — pure factory functions that turn terminal-page context state
 * into a `ResultCardSpec`. These reproduce the bodies of the ZoneStatusBar
 * `MetricsPopover` and `HistoryPopover` (ZoneStatusBar.tsx:685-1081) so the
 * `/metrics` and `/history` slash actions can pop a result card whose content
 * matches the popover exactly.
 *
 * Test constraint: the runner's vitest runs `environment: "node"` (no jsdom,
 * no @testing-library/react). These builders are PURE — tests assert the
 * returned spec's `title` / `subtitle` / `sections` / footer fields. The
 * `body` React node (used by `/history`) is built but not rendered in tests.
 *
 * `STATE_COLORS` / `STATE_LABELS` are LOCAL copies ported verbatim from
 * ZoneStatusBar.tsx:59-72 — deliberately NOT imported from ZoneStatusBar
 * because PR-C deletes that file.
 */

import type { SessionState, ZoneAssignments } from "../useZoneLayout";
import type { TerminalTab } from "../useTerminalManager";
import type { ResultCardSpec, ResultCardSection } from "./ResultCardContext";

// Ported verbatim from ZoneStatusBar.tsx:59-72.
const STATE_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

const STATE_LABELS: Record<SessionState, string> = {
  idle: "idle",
  working: "working",
  "needs-input": "waiting",
  completed: "done",
  error: "error",
};

export interface BuildMetricsCardInput {
  metrics: {
    totalApprovals: number;
    totalRejections: number;
    totalBroadcasts: number;
    sessionsCreated: number;
  };
  sessionStates: Record<string, SessionState>;
  tabs: TerminalTab[];
  autoApproveCount: number;
  autoRestartCount: number;
  stateTimeAccum?: Record<SessionState, number>;
  lastOutputLines?: Record<string, string[]>;
  assignments?: ZoneAssignments;
  zoneLabels?: Record<number, string>;
  stateDurations?: Record<string, string>;
}

/**
 * Build the `/metrics` result-card spec. Reproduces MetricsPopover
 * (ZoneStatusBar.tsx:685-1001) section-for-section.
 */
export function buildMetricsCardSpec(input: BuildMetricsCardInput): ResultCardSpec {
  const {
    metrics,
    sessionStates,
    tabs,
    autoApproveCount,
    autoRestartCount,
    stateTimeAccum,
    lastOutputLines,
    assignments,
    zoneLabels,
    stateDurations,
  } = input;

  const sections: ResultCardSection[] = [];

  // ── Counters (no heading; first section) ──────────────────────────────
  const counterRows: ResultCardSection["rows"] = [
    { label: "Sessions created", labelColor: "#a9b1d6", value: String(metrics.sessionsCreated) },
    { label: "Approvals sent", labelColor: "#9ece6a", value: String(metrics.totalApprovals) },
    { label: "Rejections sent", labelColor: "#f7768e", value: String(metrics.totalRejections) },
    { label: "Broadcasts sent", labelColor: "#7aa2f7", value: String(metrics.totalBroadcasts) },
  ];
  if (autoApproveCount > 0) {
    counterRows.push({ label: "Auto-approved", labelColor: "#9ece6a", value: String(autoApproveCount) });
  }
  if (autoRestartCount > 0) {
    counterRows.push({ label: "Auto-restarted", labelColor: "#7dcfff", value: String(autoRestartCount) });
  }
  sections.push({ rows: counterRows });

  // ── Current state ─────────────────────────────────────────────────────
  // Replicate the stateCounts useMemo at ZoneStatusBar.tsx:140-154.
  const stateCounts: Record<SessionState, number> = {
    idle: 0,
    working: 0,
    "needs-input": 0,
    completed: 0,
    error: 0,
  };
  for (const tab of tabs) {
    const state = sessionStates[tab.id] ?? "idle";
    stateCounts[state]++;
  }
  // idle row OMITTED, matching ZSB.
  sections.push({
    heading: "Current state",
    rows: [
      { label: "Working", labelColor: "#a9b1d6", valueColor: "#7aa2f7", value: String(stateCounts.working) },
      {
        label: "Waiting for input",
        labelColor: "#a9b1d6",
        valueColor: "#e0af68",
        value: String(stateCounts["needs-input"]),
      },
      {
        label: "Idle (done responding)",
        labelColor: "#a9b1d6",
        valueColor: "#9ece6a",
        value: String(stateCounts.completed),
      },
      { label: "Errored", labelColor: "#a9b1d6", valueColor: "#f7768e", value: String(stateCounts.error) },
    ],
  });

  // ── Time in state ─────────────────────────────────────────────────────
  if (stateTimeAccum && (stateTimeAccum.working > 0 || stateTimeAccum["needs-input"] > 0)) {
    const timeRows: ResultCardSection["rows"] = [];
    for (const s of ["working", "needs-input", "idle", "completed", "error"] as SessionState[]) {
      const ms = stateTimeAccum[s];
      if (ms < 1000) continue;
      const sec = Math.floor(ms / 1000);
      const min = Math.floor(sec / 60);
      const hr = Math.floor(min / 60);
      const label =
        hr > 0 ? `${hr}h${min % 60}m` : min > 0 ? `${min}m${sec % 60}s` : `${sec}s`;
      timeRows.push({ label: STATE_LABELS[s], labelColor: STATE_COLORS[s], value: label });
    }
    if (timeRows.length > 0) {
      sections.push({ heading: "Time in state", rows: timeRows });
    }
  }

  // ── Top Keywords ──────────────────────────────────────────────────────
  // Replicate topKeywords from ZoneStatusBar.tsx:728-843 verbatim.
  const topKeywords = computeTopKeywords(lastOutputLines);
  if (topKeywords.length > 0) {
    sections.push({
      heading: "Top Keywords",
      rows: topKeywords.map(([word, count]) => ({
        label: word,
        labelColor: "#a9b1d6",
        value: String(count),
      })),
    });
  }

  const spec: ResultCardSpec = {
    title: "Session Metrics",
    sections,
  };

  // ── Footer: Copy summary as Markdown ──────────────────────────────────
  // Ports handleCopySummary from ZoneStatusBar.tsx:845-869. Guard: omit the
  // footer entirely if tabs or assignments are absent.
  if (tabs && assignments) {
    spec.footer = {
      label: "Copy summary as Markdown",
      onClick: () => {
        const rows: string[] = [];
        rows.push("| Zone | Title | State | Duration | Lines | Tags |");
        rows.push("|------|-------|-------|----------|-------|------|");

        const sortedEntries = Object.entries(assignments)
          .map(([z, tabId]) => ({ zone: Number(z), tabId }))
          .sort((a, b) => a.zone - b.zone);

        for (const { zone, tabId } of sortedEntries) {
          const tab = tabs.find((t) => t.id === tabId);
          if (!tab) continue;
          const state = (sessionStates as Record<string, string>)?.[tabId] ?? "idle";
          const duration = (stateDurations as Record<string, string>)?.[tabId] ?? "-";
          const lineCount = (lastOutputLines as Record<string, unknown[]>)?.[tabId]?.length ?? 0;
          const tags = zoneLabels?.[zone] ?? "-";
          rows.push(
            `| ${zone + 1} | ${tab.title} | ${state} | ${duration} | ${lineCount} | ${tags} |`,
          );
        }

        return navigator.clipboard.writeText(rows.join("\n"));
      },
    };
  }

  return spec;
}

/**
 * Compute the top-5 keywords across `lastOutputLines`. Replicates the
 * `topKeywords` useMemo at ZoneStatusBar.tsx:728-843 verbatim (same stopword
 * set, the `[^a-z0-9\s]` split, the len>=3 && !stopword && !/^\d+$/ filter,
 * freq sort desc, `.slice(0, 5)`).
 */
function computeTopKeywords(
  lastOutputLines?: Record<string, string[]>,
): [string, number][] {
  if (!lastOutputLines) return [];
  const stopWords = new Set([
    "the",
    "a",
    "an",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "being",
    "have",
    "has",
    "had",
    "do",
    "does",
    "did",
    "will",
    "would",
    "could",
    "should",
    "may",
    "might",
    "shall",
    "can",
    "need",
    "dare",
    "ought",
    "used",
    "to",
    "of",
    "in",
    "for",
    "on",
    "with",
    "at",
    "by",
    "from",
    "as",
    "into",
    "through",
    "during",
    "before",
    "after",
    "above",
    "below",
    "between",
    "out",
    "off",
    "over",
    "under",
    "again",
    "further",
    "then",
    "once",
    "here",
    "there",
    "when",
    "where",
    "why",
    "how",
    "all",
    "each",
    "every",
    "both",
    "few",
    "more",
    "most",
    "other",
    "some",
    "such",
    "no",
    "nor",
    "not",
    "only",
    "own",
    "same",
    "so",
    "than",
    "too",
    "very",
    "just",
    "because",
    "but",
    "and",
    "or",
    "if",
    "while",
    "that",
    "this",
    "these",
    "those",
    "it",
    "its",
  ]);

  const freq: Record<string, number> = {};
  for (const lines of Object.values(lastOutputLines)) {
    for (const line of lines) {
      const words = line
        .toLowerCase()
        .replace(/[^a-z0-9\s]/g, " ")
        .split(/\s+/);
      for (const w of words) {
        if (w.length >= 3 && !stopWords.has(w) && !/^\d+$/.test(w)) {
          freq[w] = (freq[w] ?? 0) + 1;
        }
      }
    }
  }
  return Object.entries(freq)
    .sort((a, b) => b[1] - a[1])
    .slice(0, 5);
}

export interface HistoryCardEntry {
  time: number;
  type: string;
  session: string;
  zone?: number;
  color: string;
}

/**
 * Build the `/history` result-card spec. The list body is lifted verbatim
 * from HistoryPopover (ZoneStatusBar.tsx:1043-1076). The card header already
 * shows the title + count, so the body need NOT repeat the "Event History (N)"
 * header bar — just the list.
 */
export function buildHistoryCardSpec(entries: HistoryCardEntry[]): ResultCardSpec {
  const formatTime = (ts: number) => {
    const d = new Date(ts);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  };

  const body = (
    <div className="max-h-64 overflow-y-auto scrollbar-dark">
      {entries.length === 0 ? (
        <div className="px-3 py-4 text-center text-[10px] text-[#565f89]">No events yet</div>
      ) : (
        [...entries].reverse().map((entry, i) => (
          <div
            key={`event-${entry.type}-${i}`}
            className="flex items-start gap-2 px-3 py-1.5 hover:bg-[#2a2d3d]/30 transition-colors"
          >
            <span className="text-[9px] text-[#565f89] font-mono shrink-0 mt-0.5">
              {formatTime(entry.time)}
            </span>
            <div className="flex-1 min-w-0">
              <span className="text-[10px] font-medium" style={{ color: entry.color }}>
                {entry.type}
              </span>
              <span className="text-[10px] text-[#a9b1d6] ml-1 truncate">{entry.session}</span>
            </div>
            {entry.zone !== undefined && (
              <span className="text-[9px] text-[#565f89] font-mono shrink-0">
                Z{entry.zone + 1}
              </span>
            )}
          </div>
        ))
      )}
    </div>
  );

  return {
    title: "Event History",
    subtitle: `(${entries.length})`,
    body,
  };
}
