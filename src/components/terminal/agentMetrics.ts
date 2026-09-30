/**
 * Per-terminal session metrics — context fill, session cost and the account's
 * 5-hour headroom — as the runner reports them (plan
 * `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
 * Phase 7).
 *
 * The runner owns the readings; this module is the TypeScript mirror of their
 * wire shape (the `terminal-agent-metrics` Tauri event and the
 * `get_terminal_agent_metrics` command), one small module-level store the
 * cards subscribe to, and the pure projection the cards render.
 *
 * Two honesty rules the projection enforces, because a number with no
 * provenance is worse than no number:
 *
 * - An ABSENT reading renders "—", never `0` / `0%` / `$0.00`. Session cost
 *   has no source at all since the D2 re-decision (the statusline is not
 *   registered on this CLI), so it is always "—" today.
 * - A STALE reading (older than its threshold, or of unknown age) still shows
 *   its value, but greyed and with its age; the source and age are always in
 *   the tooltip.
 */

import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ── Wire types (serde shape of the runner's `SessionMetrics`) ────────────────

export type ContextSource = "transcript" | "grid";
export type HeadroomSource = "oauth_probe" | "cached_usage";

export interface SessionMetrics {
  contextUsedPct: number | null;
  contextTokens: number | null;
  contextWindow: number | null;
  contextSource: ContextSource | null;
  contextObservedAtMs: number | null;
  /** Always null today: nothing reports session cost (D2 re-decision). */
  costUsd: number | null;
  fiveHourPct: number | null;
  fiveHourResetsAt: string | null;
  sevenDayPct: number | null;
  headroomSource: HeadroomSource | null;
  headroomObservedAtMs: number | null;
}

/** Payload of the `terminal-agent-metrics` event, and one row of the command. */
export interface TerminalAgentMetricsEvent {
  terminalId: string;
  metrics: SessionMetrics;
}

export const TERMINAL_AGENT_METRICS_EVENT = "terminal-agent-metrics";

/**
 * A context reading older than this is stale. The transcript and the grid
 * both update on every turn, so ten quiet minutes means the session is idle
 * or the source stopped reporting — either way the number may no longer hold.
 */
export const CONTEXT_STALE_AFTER_MS = 10 * 60 * 1000;

/**
 * A headroom reading older than this is stale. The runner's OAuth usage probe
 * samples every ~10 minutes, so a reading up to one full cycle old is the
 * normal state; twice the cycle means a sample was missed.
 */
export const HEADROOM_STALE_AFTER_MS = 20 * 60 * 1000;

export const ABSENT = "—";

// ── Mapping (wire → typed) ───────────────────────────────────────────────────

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}

function oneOf<T extends string>(v: unknown, allowed: readonly T[]): T | null {
  return typeof v === "string" && (allowed as readonly string[]).includes(v) ? (v as T) : null;
}

/**
 * Coerce whatever arrived over IPC into a `SessionMetrics`. Every field that
 * is missing, mistyped or non-finite becomes `null` — i.e. absent, rendered
 * "—" — rather than a default that would read as a measurement.
 */
export function normalizeSessionMetrics(raw: unknown): SessionMetrics {
  const r = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  return {
    contextUsedPct: num(r.contextUsedPct),
    contextTokens: num(r.contextTokens),
    contextWindow: num(r.contextWindow),
    contextSource: oneOf(r.contextSource, ["transcript", "grid"] as const),
    contextObservedAtMs: num(r.contextObservedAtMs),
    costUsd: num(r.costUsd),
    fiveHourPct: num(r.fiveHourPct),
    fiveHourResetsAt: str(r.fiveHourResetsAt),
    sevenDayPct: num(r.sevenDayPct),
    headroomSource: oneOf(r.headroomSource, ["oauth_probe", "cached_usage"] as const),
    headroomObservedAtMs: num(r.headroomObservedAtMs),
  };
}

/** Shape guard + normalization for one event payload / command row. */
export function parseAgentMetricsEvent(value: unknown): TerminalAgentMetricsEvent | null {
  if (!value || typeof value !== "object") return null;
  const v = value as { terminalId?: unknown; metrics?: unknown };
  if (typeof v.terminalId !== "string" || v.terminalId.length === 0) return null;
  if (!v.metrics || typeof v.metrics !== "object") return null;
  return { terminalId: v.terminalId, metrics: normalizeSessionMetrics(v.metrics) };
}

// ── Store ────────────────────────────────────────────────────────────────────

let metricsByTerminal: Readonly<Record<string, SessionMetrics>> = {};
const listeners = new Set<() => void>();
let bridgeStop: (() => void) | null = null;

function emit(): void {
  for (const l of listeners) l();
}

/** Apply one event payload / command row. Returns false when it was malformed. */
export function applyAgentMetrics(value: unknown): boolean {
  const parsed = parseAgentMetricsEvent(value);
  if (!parsed) return false;
  metricsByTerminal = { ...metricsByTerminal, [parsed.terminalId]: parsed.metrics };
  emit();
  return true;
}

export function getAgentMetrics(terminalId: string | null | undefined): SessionMetrics | null {
  if (!terminalId) return null;
  return metricsByTerminal[terminalId] ?? null;
}

/** Initial load of every terminal's metrics. `[]` on any failure. */
export async function fetchTerminalAgentMetrics(): Promise<unknown[]> {
  try {
    const rows = await invoke<unknown[]>("get_terminal_agent_metrics");
    return Array.isArray(rows) ? rows : [];
  } catch {
    return [];
  }
}

/**
 * Subscribe to the runner: the event plus one initial load. A runner build
 * without either simply never reports, so every card renders "—". Every
 * failure is swallowed — metrics are display-only.
 */
function startBridge(): () => void {
  let stopped = false;
  let unlisten: (() => void) | undefined;
  try {
    listen<unknown>(TERMINAL_AGENT_METRICS_EVENT, (event) => {
      if (!stopped) applyAgentMetrics(event.payload);
    })
      .then((fn) => {
        if (stopped) fn();
        else unlisten = fn;
      })
      .catch(() => {});
  } catch {
    // No Tauri event bridge (tests, plain browser).
  }
  void fetchTerminalAgentMetrics().then((rows) => {
    if (stopped) return;
    for (const row of rows) applyAgentMetrics(row);
  });
  return () => {
    stopped = true;
    unlisten?.();
  };
}

export function subscribeAgentMetrics(listener: () => void): () => void {
  listeners.add(listener);
  if (!bridgeStop) bridgeStop = startBridge();
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && bridgeStop) {
      bridgeStop();
      bridgeStop = null;
    }
  };
}

/** Test seam: forget every reading and drop the bridge. */
export function resetAgentMetricsStore(): void {
  metricsByTerminal = {};
  bridgeStop?.();
  bridgeStop = null;
  emit();
}

/** The runner's metrics for one terminal, or null when it has reported none. */
export function useAgentMetrics(terminalId: string | null | undefined): SessionMetrics | null {
  const read = () => getAgentMetrics(terminalId);
  return useSyncExternalStore(subscribeAgentMetrics, read, read);
}

// ── Projection (what a card renders) ─────────────────────────────────────────

export interface MetricCell {
  /** Short value text; {@link ABSENT} when there is no reading. */
  text: string;
  absent: boolean;
  /** Older than its threshold, or of unknown age. Rendered greyed with its age. */
  stale: boolean;
  /** Short age ("42s", "5m", "2h") or null when unknown / absent. */
  age: string | null;
  /** Human-readable source, or null when absent. */
  source: string | null;
  /** Full tooltip: value, source and age. */
  title: string;
}

export interface MetricCells {
  context: MetricCell;
  cost: MetricCell;
  headroom: MetricCell;
}

const SOURCE_LABELS: Record<ContextSource | HeadroomSource, string> = {
  transcript: "transcript",
  grid: "screen scan",
  oauth_probe: "OAuth usage probe",
  cached_usage: "cached usage snapshot",
};

export function formatMetricAge(ageMs: number): string {
  const s = Math.max(0, Math.floor(ageMs / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

function pct(v: number): string {
  return `${Math.round(v)}%`;
}

function provenance(
  source: string | null,
  observedAtMs: number | null,
  nowMs: number,
  staleAfterMs: number,
): { source: string | null; age: string | null; stale: boolean; line: string } {
  const src = source ?? "unknown source";
  if (observedAtMs == null) {
    return { source, age: null, stale: true, line: `from ${src}, age unknown` };
  }
  const ageMs = nowMs - observedAtMs;
  const age = formatMetricAge(ageMs);
  const stale = ageMs > staleAfterMs;
  return { source, age, stale, line: `from ${src}, ${age} ago${stale ? " (stale)" : ""}` };
}

function absentCell(title: string): MetricCell {
  return { text: ABSENT, absent: true, stale: false, age: null, source: null, title };
}

function contextCell(m: SessionMetrics | null, nowMs: number): MetricCell {
  let used = m?.contextUsedPct ?? null;
  if (used == null && m?.contextTokens != null && m.contextWindow != null && m.contextWindow > 0) {
    used = (m.contextTokens / m.contextWindow) * 100;
  }
  if (!m || used == null) return absentCell("Context used — no reading");
  const p = provenance(
    m.contextSource ? SOURCE_LABELS[m.contextSource] : null,
    m.contextObservedAtMs,
    nowMs,
    CONTEXT_STALE_AFTER_MS,
  );
  const tokens =
    m.contextTokens != null && m.contextWindow != null
      ? ` (${m.contextTokens.toLocaleString("en-US")} / ${m.contextWindow.toLocaleString("en-US")} tokens)`
      : "";
  return {
    text: pct(used),
    absent: false,
    stale: p.stale,
    age: p.age,
    source: p.source,
    title: `Context ${pct(used)} used${tokens}\n${p.line}`,
  };
}

function costCell(m: SessionMetrics | null): MetricCell {
  if (!m || m.costUsd == null) {
    return absentCell("Session cost — no source reports it for this session");
  }
  const text = `$${m.costUsd.toFixed(2)}`;
  // Cost carries no observation time of its own on the wire.
  return { text, absent: false, stale: false, age: null, source: null, title: `Session cost ${text}` };
}

function headroomCell(m: SessionMetrics | null, nowMs: number): MetricCell {
  if (!m || m.fiveHourPct == null) {
    return absentCell("Account 5-hour usage — no reading");
  }
  const p = provenance(
    m.headroomSource ? SOURCE_LABELS[m.headroomSource] : null,
    m.headroomObservedAtMs,
    nowMs,
    HEADROOM_STALE_AFTER_MS,
  );
  const left = Math.max(0, 100 - m.fiveHourPct);
  const lines = [`Account 5-hour window ${pct(m.fiveHourPct)} used (${pct(left)} headroom)`];
  if (m.fiveHourResetsAt) lines.push(`resets ${m.fiveHourResetsAt}`);
  if (m.sevenDayPct != null) lines.push(`7-day window ${pct(m.sevenDayPct)} used`);
  lines.push(p.line);
  return {
    text: pct(m.fiveHourPct),
    absent: false,
    stale: p.stale,
    age: p.age,
    source: p.source,
    title: lines.join("\n"),
  };
}

/** Pure projection of one terminal's metrics onto the three card cells. */
export function metricCells(metrics: SessionMetrics | null, nowMs: number): MetricCells {
  return {
    context: contextCell(metrics, nowMs),
    cost: costCell(metrics),
    headroom: headroomCell(metrics, nowMs),
  };
}
