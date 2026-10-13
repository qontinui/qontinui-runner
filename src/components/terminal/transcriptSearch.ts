/**
 * Thin client for the `transcript_search` Tauri command — the backend of the
 * query language's `text:"…"` term (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4) — plus the hook that runs one search per `text:` query.
 *
 * The response always states `{scanned, total, truncated}`;
 * {@link textSearchResultFrom} turns it into the evaluator's
 * `TextSearchResult`, under which a session the scan did not reach is
 * UNKNOWN for that term — never "no match".
 */

import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { describeThrown } from "@/lib/utils";

import type { TextSearchResult } from "./sessionQuery";

/** Mirrors Rust `TranscriptSearchHit`. */
export interface TranscriptSearchHit {
  sessionId: string;
  configDir: string;
  path: string;
  hitCount: number;
  snippet: string;
  mtimeMs: number;
}

/** Mirrors Rust `TranscriptSearchResponse`. */
export interface TranscriptSearchResponse {
  terms: string[];
  hits: TranscriptSearchHit[];
  matched: number;
  scanned: number;
  total: number;
  failed: number;
  truncated: boolean;
  /** `time_budget` | `limit` | `unreadable` | `not_run`. */
  truncation: string[];
  notScanned: string[];
  missing: string[];
  elapsedMs: number;
}

/**
 * Search transcripts. `sessionKeys` (Claude session ids) scopes the scan to
 * those sessions; omit it to search every transcript on disk (the palette).
 */
export function searchTranscripts(
  query: string,
  sessionKeys?: readonly string[],
  limit?: number,
): Promise<TranscriptSearchResponse> {
  return invoke<TranscriptSearchResponse>("transcript_search", {
    query,
    sessionKeys: sessionKeys ?? null,
    limit: limit ?? null,
  });
}

/** The evaluator's view of one response. `scoped` = sessionKeys were sent. */
export function textSearchResultFrom(
  resp: TranscriptSearchResponse,
  scoped: boolean,
): TextSearchResult {
  return {
    status: "done",
    matched: new Set(resp.hits.map((h) => h.sessionId)),
    notScanned: new Set(resp.notScanned),
    limitHit: resp.truncation.includes("limit"),
    // A scoped scan lists exactly what it missed in `notScanned`; an unscoped
    // one that stopped early cannot say which sessions it missed.
    incomplete: !scoped && resp.truncated && !resp.truncation.every((t) => t === "limit"),
  };
}

/**
 * Run one scoped search per `text:` query over `sessionIds`, re-running when
 * either changes. Each query reads `pending` until its answer lands.
 */
export function useTranscriptSearches(
  queries: readonly string[],
  sessionIds: readonly string[],
): ReadonlyMap<string, TextSearchResult> {
  const queriesKey = useMemo(() => [...new Set(queries)].sort().join("\u0000"), [queries]);
  const idsKey = useMemo(() => [...new Set(sessionIds)].sort().join("\n"), [sessionIds]);
  // Answers are kept with the session scope they were computed for, so an
  // answer for a previous scope is never shown for the current one.
  const [answers, setAnswers] = useState<{ scope: string; map: Map<string, TextSearchResult> }>({
    scope: "",
    map: new Map(),
  });

  useEffect(() => {
    const qs = queriesKey === "" ? [] : queriesKey.split("\u0000");
    const ids = idsKey === "" ? [] : idsKey.split("\n");
    let live = true;
    for (const q of qs) {
      // `limit` = the scope, so the hit limit can never trim a scoped answer.
      searchTranscripts(q, ids, Math.max(1, ids.length))
        .then((resp) => textSearchResultFrom(resp, true))
        .catch(
          (e: unknown): TextSearchResult => ({
            status: "failed",
            reason: describeThrown(e, "transcript_search failed"),
          }),
        )
        .then((r) => {
          if (!live) return;
          setAnswers((prev) => ({
            scope: idsKey,
            map: new Map(prev.scope === idsKey ? prev.map : []).set(q, r),
          }));
        });
    }
    return () => {
      live = false;
    };
  }, [queriesKey, idsKey]);

  return useMemo(() => {
    const qs = queriesKey === "" ? [] : queriesKey.split("\u0000");
    const pending: TextSearchResult = { status: "pending" };
    return new Map(
      qs.map((q) => [q, (answers.scope === idsKey ? answers.map.get(q) : undefined) ?? pending]),
    );
  }, [answers, queriesKey, idsKey]);
}
