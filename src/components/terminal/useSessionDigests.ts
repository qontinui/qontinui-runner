/**
 * Transcript tail digests for the Session Manager, requested BY SESSION ID.
 *
 * The panel used to ask `transcript_session_digests` for "the most recent 50"
 * (the command clamped to 100 and truncated with no offset), so on a box with
 * more sessions every one past the cut silently lost its digest-derived status.
 * It now names the sessions it needs — every one whose process may be running,
 * plus the newest rows at the top of the list — in chunks, so the cost scales
 * with live sessions rather than with every transcript on disk, and the 51st
 * or 200th live session gets a digest like the first (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 1). A row that still has no digest renders activity `Unknown`.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { CommandResponse } from "./types";
import { isPossiblyLive, type SessionLocation } from "./sessionStatusModel";

export interface SessionDigest {
  session_id: string;
  config_dir: string;
  project_path: string;
  last_message_type: string;
  last_message_timestamp: string;
  last_message_preview: string;
  last_assistant_had_tool_use: boolean;
  likely_frozen: boolean;
  work_summary_hint: string;
}

/** Ids per `transcript_session_digests` call — one file tail per id. */
export const DIGEST_CHUNK_SIZE = 100;

/**
 * Newest not-running rows also digested: their status does not need one (a
 * gone process is `Exited` / `Finished`), but the top of the list keeps the
 * preview and work-summary text it showed before the cap was removed.
 */
export const RECENT_ROWS_DIGESTED = 50;

const DIGEST_POLL_MS = 30_000;

/** Split `ids` into request-sized chunks. */
export function chunkIds(ids: readonly string[], size = DIGEST_CHUNK_SIZE): string[][] {
  const chunks: string[][] = [];
  for (let i = 0; i < ids.length; i += size) chunks.push(ids.slice(i, i + size));
  return chunks;
}

/**
 * The transcript sessions to digest: every one whose location is not
 * `Not running` (so every live session, however many), plus the
 * {@link RECENT_ROWS_DIGESTED} most recently modified. Sorted, so the request
 * key is stable across renders.
 */
export function digestRequestIds(
  rows: readonly { sessionId: string; location: SessionLocation; lastModified: string }[],
): string[] {
  const ids = new Set<string>();
  for (const r of rows) if (isPossiblyLive(r.location)) ids.add(r.sessionId);
  const recent = [...rows]
    .sort((a, b) => b.lastModified.localeCompare(a.lastModified))
    .slice(0, RECENT_ROWS_DIGESTED);
  for (const r of recent) ids.add(r.sessionId);
  return [...ids].sort();
}

export interface UseSessionDigestsResult {
  digests: Readonly<Record<string, SessionDigest>>;
  loading: boolean;
  refresh: () => void;
}

/**
 * Digests for `ids`, re-fetched when the id set changes and every 30 s (the
 * activity a digest decides — idle vs. mid-tool — moves while a session runs).
 * A failed round keeps the previous digests rather than blanking every row.
 */
export function useSessionDigests(ids: readonly string[]): UseSessionDigestsResult {
  const [digests, setDigests] = useState<Record<string, SessionDigest>>({});
  const [loading, setLoading] = useState(false);
  const key = ids.join("|");
  const idsRef = useRef<readonly string[]>(ids);
  const generation = useRef(0);
  // Declared before the fetch effect so the ref is current when it runs.
  useEffect(() => {
    idsRef.current = ids;
  }, [ids]);

  const fetchDigests = useCallback(async () => {
    const requested = idsRef.current;
    const gen = ++generation.current;
    if (requested.length === 0) {
      setDigests({});
      return;
    }
    setLoading(true);
    try {
      const map: Record<string, SessionDigest> = {};
      for (const chunk of chunkIds(requested)) {
        const result = await invoke<CommandResponse>("transcript_session_digests", {
          sessionIds: chunk,
        });
        if (!result?.success || !Array.isArray(result.data)) return;
        for (const d of result.data as SessionDigest[]) map[d.session_id] = d;
      }
      // A newer request superseded this one while it ran.
      if (gen === generation.current) setDigests(map);
    } catch {
      // Keep the previous round: a transport failure says nothing new.
    } finally {
      if (gen === generation.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchDigests();
    });
    const interval = setInterval(() => void fetchDigests(), DIGEST_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
    // `key` is the id set's identity; `ids` itself is read through the ref.
  }, [key, fetchDigests]);

  return { digests, loading, refresh: fetchDigests };
}
