import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { TranscriptMessage } from "./useTranscriptSessions";
import { extractUserPrompts, promptsUnchanged, type UserPrompt } from "./sessionPrompts";

interface CommandResponse {
  success: boolean;
  message?: string;
  data?: unknown;
}

export type SessionPromptsStatus = "loading" | "ready" | "unavailable";

export interface SessionPromptsState {
  status: SessionPromptsStatus;
  prompts: UserPrompt[];
  /** Set only when `status` is `"unavailable"`. */
  reason: string | null;
}

const LOADING: SessionPromptsState = { status: "loading", prompts: [], reason: null };

/** How often an OPEN panel re-reads its session's transcript. */
export const DEFAULT_PROMPTS_POLL_MS = 5_000;

/**
 * The operator's own prompts for one Claude Code session, read out of that
 * session's JSONL transcript.
 *
 * `enabled` is the panel's open/closed state: a closed panel does no IPC at
 * all, so N background zones cost nothing. The transcript is the source rather
 * than captured keystrokes because Claude Code's TUI edits, wraps and re-draws
 * the input line — what reaches the PTY is not reliably what was submitted.
 *
 * A failed or unsuccessful read is reported as `unavailable`, never as an
 * empty list: "this session has no prompts" and "we could not read them" are
 * different statements and the panel renders them differently.
 */
export function useSessionPrompts(
  claudeSessionId: string | undefined,
  opts: {
    configDir?: string;
    projectPath?: string;
    enabled?: boolean;
    pollMs?: number;
  } = {},
): SessionPromptsState & { refresh: () => void } {
  const { configDir, projectPath, enabled = true, pollMs = DEFAULT_PROMPTS_POLL_MS } = opts;

  // Keyed by session id so a zone reassigned to another session falls back to
  // LOADING rather than briefly showing the previous session's prompts.
  const [cached, setCached] = useState<{ sid: string; data: SessionPromptsState } | null>(null);
  const inFlight = useRef(false);

  const fetchPrompts = useCallback(async () => {
    if (!claudeSessionId || inFlight.current) return;
    inFlight.current = true;
    try {
      const result = await invoke<CommandResponse>("transcript_read_session", {
        sessionId: claudeSessionId,
        configDir: configDir ?? null,
        projectPath: projectPath ?? null,
      });
      if (!result.success || !result.data) {
        setCached({
          sid: claudeSessionId,
          data: {
            status: "unavailable",
            prompts: [],
            reason: result.message ?? "transcript not found",
          },
        });
        return;
      }
      const next = extractUserPrompts(result.data as TranscriptMessage[]);
      setCached((prev) => {
        // Skip the state write on a no-new-prompt tick so an open panel does
        // not re-render (and fight the scroll anchor) every poll interval.
        if (
          prev?.sid === claudeSessionId &&
          prev.data.status === "ready" &&
          promptsUnchanged(prev.data.prompts, next)
        ) {
          return prev;
        }
        return { sid: claudeSessionId, data: { status: "ready", prompts: next, reason: null } };
      });
    } catch (err) {
      // A hard Err is an IPC fault, not a data condition.
      console.error("transcript_read_session failed:", err);
      setCached({
        sid: claudeSessionId,
        data: { status: "unavailable", prompts: [], reason: "ipc_error" },
      });
    } finally {
      inFlight.current = false;
    }
  }, [claudeSessionId, configDir, projectPath]);

  useEffect(() => {
    if (!enabled || !claudeSessionId) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- fetchPrompts is async; setCached fires after the IPC await, never synchronously in the effect body
    void fetchPrompts();
    const timer = setInterval(() => void fetchPrompts(), pollMs);
    return () => clearInterval(timer);
  }, [enabled, claudeSessionId, fetchPrompts, pollMs]);

  const state = claudeSessionId && cached?.sid === claudeSessionId ? cached.data : LOADING;
  return { ...state, refresh: () => void fetchPrompts() };
}
