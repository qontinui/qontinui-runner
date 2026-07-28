import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { extractLiveSessions, registryNamesBySessionId } from "./liveClaudeSessions";
import type { CommandResponse } from "./types";

/**
 * How often to re-read Claude Code's live-session registry.
 *
 * The name changes when the operator runs `/rename`, so the cadence sets how
 * long a card can show a stale name. 30s matches the sibling
 * `transcript_find_external_processes` poll in `useSessionManager`, and the
 * read itself is ~20 sub-1KB JSON files (measured on the fleet's heaviest box,
 * 2026-07-28) — far too cheap to justify a cache or a push channel.
 */
const REGISTRY_POLL_MS = 30_000;

const EMPTY: ReadonlyMap<string, string> = new Map();

/**
 * `sessionId → the name shown in that session's Claude Code window`, for every
 * LIVE session that carries an operator-chosen name.
 *
 * This is the single frontend entry point for the window-name override that
 * `SessionCard`, `useSessionManager` and `PastSessionsView` apply. It reuses the
 * existing `terminal_claude_session_list_live` command — the runner has exactly
 * one registry reader (`session::claude_session_registry`) and must keep it that
 * way.
 *
 * Two properties the callers rely on:
 *
 * - **A miss is meaningful.** Rows whose `nameSource` is `"derived"` are
 *   filtered out by {@link registryNamesBySessionId}, so a miss means "nothing
 *   here beats what you already show" — callers keep their existing fallback
 *   chain untouched.
 * - **Closed sessions are always a miss.** Claude Code deletes the backing file
 *   when a process exits and the reader re-checks liveness against the process
 *   table, so this map never describes a dead session. That is what makes the
 *   live/closed asymmetry in `PastSessionsView` fall out for free rather than
 *   needing a second branch.
 *
 * Fails soft to an empty map: a card showing its previous name is a far better
 * outcome than a panel that throws mid-render.
 */
export function useLiveClaudeSessionNames(): ReadonlyMap<string, string> {
  const [names, setNames] = useState<ReadonlyMap<string, string>>(EMPTY);
  const cancelled = useRef(false);

  const refresh = useCallback(async () => {
    try {
      const res = await invoke<CommandResponse>("terminal_claude_session_list_live");
      if (cancelled.current) return;
      const next = res.success ? registryNamesBySessionId(extractLiveSessions(res.data)) : EMPTY;
      // Replace only on a real change so consumers memoized on this map (the
      // `allSessions` build in `useSessionManager`) do not rebuild every poll.
      setNames((prev) => (sameNames(prev, next) ? prev : next));
    } catch {
      // Command unavailable (older binary) or registry unreadable — keep the
      // last good map rather than blanking every card.
    }
  }, []);

  useEffect(() => {
    cancelled.current = false;
    // Defer the first read past the effect body so it cannot setState
    // synchronously during mount.
    void Promise.resolve().then(() => {
      if (!cancelled.current) void refresh();
    });
    const interval = setInterval(() => void refresh(), REGISTRY_POLL_MS);
    return () => {
      cancelled.current = true;
      clearInterval(interval);
    };
  }, [refresh]);

  return names;
}

/** Shallow map equality — used to keep the state identity stable across polls. */
function sameNames(a: ReadonlyMap<string, string>, b: ReadonlyMap<string, string>): boolean {
  if (a.size !== b.size) return false;
  for (const [id, name] of a) {
    if (b.get(id) !== name) return false;
  }
  return true;
}
