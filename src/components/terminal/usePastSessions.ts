import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "./types";

/**
 * The account a past session belonged to — the `/rename`-wrapper pair the
 * operator launched it under (e.g. `{ label: "gmail", wrapper: "clg" }`).
 */
export interface PastSessionAccount {
  label: string;
  wrapper: string;
}

/**
 * A previously-seen Claude session read from the durable session registry +
 * transcript snapshot history (backend `terminal_session_list_history`). Unlike
 * the live `UnifiedSession`, this is a HISTORICAL record — the session may be
 * closed, and its transcript may already be pruned (`transcriptExists`).
 *
 * The headline field is `resumeName` — the real `/rename` name the operator
 * gave the session — and `resumeCommand` is a ready-to-paste
 * `clg --resume <id>`-style command.
 *
 * Keys are camelCase, matching the Rust command's serde output exactly.
 */
export interface PastSession {
  /** Stable Claude Code session id — the resume key. */
  claudeSessionId: string;
  /** The real `/rename` name (the headline — show prominently). */
  resumeName: string;
  /** Ready-to-copy resume command, e.g. `"clg --resume <id>"`. */
  resumeCommand: string;
  /** Which account/wrapper the session ran under. */
  account: PastSessionAccount;
  /** Terminal page the session belonged to. */
  pageId: string;
  /** Zone index within the page (-1 when unassigned). */
  zoneIndex: number;
  /** Lifecycle state. */
  state: "open" | "closed" | string;
  /** Why the session closed (present when `state === "closed"`). */
  closeReason?: string;
  /** Ephemeral terminal/tab id at last record. */
  terminalId?: string;
  /** Claude config dir for the session (for `claude --resume` env). */
  configDir?: string;
  /** Working directory the terminal was opened in. */
  workingDir?: string;
  /** AI-CLI provider that owned the session (`"claude"`, `"gemini"`). */
  provider: string;
  /** Epoch ms the record was last refreshed (sort key, newest-first). */
  lastSeenAt: number;
  /** Epoch ms the session was first recorded open. */
  openedAt: number;
  /** Epoch ms the session was recorded closed. */
  closedAt?: number;
  /** Epoch ms a provider hook confirmed a real session started. */
  confirmedAt?: number;
  /** Whether a provider hook confirmed the session was real. */
  confirmed: boolean;
  /** Whether the on-disk transcript still exists (false ⇒ pruned/gone). */
  transcriptExists: boolean;
  /** Whether this session can actually be resumed (transcript present). */
  restorable: boolean;
  /**
   * Sessions sharing a dense `lastSeenAt` band get the same `cohortId` — a
   * crash cohort (many sessions stranded at once) renders as one group.
   */
  cohortId: number;
}

/** Optional filters for the history query (camelCase, matches the command). */
export interface PastSessionsQuery {
  sinceMs?: number;
  pageId?: string;
  account?: string;
  includeShells?: boolean;
  limit?: number;
}

export interface UsePastSessionsResult {
  sessions: PastSession[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Normalize the command's `data` payload into a `PastSession[]`.
 *
 * `terminal_session_list_history` returns `data: { sessions: [...] }` — the
 * same envelope as `GET /sessions/history` — while the sibling
 * `transcript_list_sessions` returns a bare array. Accept either, and return
 * `[]` for anything unexpected: consumers iterate this value during render, so
 * a non-array here would throw and take the whole panel down with it.
 */
function extractSessions(data: unknown): PastSession[] {
  if (Array.isArray(data)) return data as PastSession[];
  if (data && typeof data === "object") {
    const inner = (data as { sessions?: unknown }).sessions;
    if (Array.isArray(inner)) return inner as PastSession[];
  }
  return [];
}

/**
 * Load the runner's previous-sessions history (durable registry + transcript
 * snapshots) via the `terminal_session_list_history` Tauri command. Mirrors
 * {@link useTranscriptSessions}: loads once on mount (guarded by a ref so a
 * re-render doesn't refetch) and exposes an imperative `refresh`.
 */
export function usePastSessions(opts?: PastSessionsQuery): UsePastSessionsResult {
  const [sessions, setSessions] = useState<PastSession[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const didLoad = useRef(false);

  const fetchSessions = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<CommandResponse>("terminal_session_list_history", {
        opts: opts ?? null,
      });
      if (result.success) {
        // The command wraps its payload: `data: { sessions: [...] }` (the same
        // envelope `GET /sessions/history` returns). Accept a bare array too,
        // and fall back to `[]` for anything else — a shape surprise must
        // degrade to "no sessions", never throw into the render tree (an
        // earlier revision assigned the envelope object straight to state,
        // and `groupByCohort`'s `for...of` then crashed the whole panel).
        setSessions(extractSessions(result.data));
      } else {
        setError(result.message || "Failed to load previous sessions");
      }
    } catch (err) {
      setError(`Failed to load previous sessions: ${err}`);
    } finally {
      setLoading(false);
    }
  }, [opts]);

  useEffect(() => {
    if (!didLoad.current) {
      didLoad.current = true;
      fetchSessions();
    }
  }, [fetchSessions]);

  return {
    sessions,
    loading,
    error,
    refresh: fetchSessions,
  };
}
