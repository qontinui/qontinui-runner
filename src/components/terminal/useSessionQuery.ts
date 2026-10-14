/**
 * `useSessionQuery` — the Session Manager's one query string, persisted per
 * window, parsed, evaluated over every row's `SessionFacts` and partitioned
 * into matched / unknown / excluded (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D5 / Phase 4).
 *
 * `text:` terms are answered by one scoped `transcript_search` per distinct
 * term over the rows' Claude session ids (`useTranscriptSearches`); until a
 * term's answer lands, it is UNKNOWN for every row, so the rows sit in the
 * Unknown tail rather than vanishing.
 *
 * The clock re-reads every {@link NOW_TICK_MS}, so `idle>30m` / `age<2h` move
 * with time without a refresh.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import { instanceStorage } from "@/lib/instance-storage";

import type { SessionFacts } from "./sessionFacts";
import {
  collectTextQueries,
  filterSessions,
  parseQuery,
  type FilterResult,
  type ParsedQuery,
  type QueryContext,
} from "./sessionQuery";
import { chipCounts, type ChipCount } from "./sessionQueryChips";
import { useTranscriptSearches } from "./transcriptSearch";

/** How often relative-time predicates are re-evaluated. */
export const NOW_TICK_MS = 30_000;

const STORAGE_BASE = "qontinui-session-query";

/** The storage key for one window's query. */
export function sessionQueryStorageKey(windowLabel: string = "main"): string {
  return windowLabel === "main" ? STORAGE_BASE : `${STORAGE_BASE}:window:${windowLabel}`;
}

/** A stored query, or the empty one for anything unreadable. */
export function parseStoredQuery(raw: unknown): string {
  return typeof raw === "string" ? raw : "";
}

export interface UseSessionQueryParams<F extends SessionFacts> {
  /** Every row the panel could show, with its facts. */
  rows: readonly F[];
  /** The Tauri window label; each window keeps its own query. */
  windowLabel?: string;
}

export interface SessionQueryState<F extends SessionFacts> {
  query: string;
  setQuery: (q: string) => void;
  parsed: ParsedQuery;
  /** `{ ok: false, errors }` on a parse error — nothing is filtered then. */
  result: FilterResult<F>;
  /** What the evaluator saw, for a caller that evaluates more (chips, tests). */
  ctx: QueryContext;
  /** Each chip's count over `rows`, keyed by `QueryChip.key`. */
  chipCounts: Record<string, ChipCount>;
}

export function useSessionQuery<F extends SessionFacts>({
  rows,
  windowLabel = "main",
}: UseSessionQueryParams<F>): SessionQueryState<F> {
  const key = sessionQueryStorageKey(windowLabel);
  const [query, setQueryState] = useState<string>(() =>
    parseStoredQuery(instanceStorage.getJSON<unknown>(key, "")),
  );
  useEffect(() => {
    instanceStorage.setJSON(key, query);
  }, [key, query]);
  const setQuery = useCallback((q: string) => setQueryState(q), []);

  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), NOW_TICK_MS);
    return () => clearInterval(t);
  }, []);

  const parsed = useMemo(() => parseQuery(query), [query]);
  const textQueries = useMemo(() => (parsed.ok ? collectTextQueries(parsed.ast) : []), [parsed]);
  const claudeIds = useMemo(
    () => rows.map((r) => r.claudeSessionId).filter((id): id is string => id !== null),
    [rows],
  );
  const textSearch = useTranscriptSearches(textQueries, claudeIds);

  const ctx = useMemo<QueryContext>(() => ({ now, textSearch }), [now, textSearch]);
  const result = useMemo(() => filterSessions(parsed, rows, ctx), [parsed, rows, ctx]);
  const counts = useMemo(() => chipCounts(rows, ctx), [rows, ctx]);

  return { query, setQuery, parsed, result, ctx, chipCounts: counts };
}
