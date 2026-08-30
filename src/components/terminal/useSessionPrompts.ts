import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { normalizePromptText, promptsUnchanged, type UserPrompt } from "./sessionPrompts";

interface CommandResponse {
  success: boolean;
  message?: string;
  data?: unknown;
}

/** Wire shape of `transcript_read_user_prompts`. */
interface UserPromptsResult {
  mtime_ms: number;
  unchanged: boolean;
  prompts: { uuid: string; timestamp: string; text: string }[];
}

export type SessionPromptsStatus = "loading" | "ready" | "unavailable";

export interface SessionPromptsState {
  status: SessionPromptsStatus;
  prompts: UserPrompt[];
  /** Set only when `status` is `"unavailable"`. */
  reason: string | null;
  /** A read is in flight — drives the refresh button's busy state. */
  refreshing: boolean;
}

const LOADING: SessionPromptsState = {
  status: "loading",
  prompts: [],
  reason: null,
  refreshing: false,
};

/** How often an OPEN panel re-checks its session's transcript. */
export const DEFAULT_PROMPTS_POLL_MS = 5_000;

/**
 * The operator's own prompts for one Claude Code session.
 *
 * `enabled` is the panel's open/closed state: a closed panel does no IPC at
 * all, so N background zones cost nothing.
 *
 * The backend does the reading, the machine-record filtering and the
 * mtime short-circuit (`transcript::read_user_prompts`) — an unchanged
 * transcript costs a stat, not a parse of a file that reaches 10 MB. What is
 * left for this side is envelope normalization, which is text-shaped and
 * therefore cheap and unit-testable without a DOM.
 *
 * A failed read is reported as `unavailable`, never as an empty list: "this
 * session has no prompts" and "we could not read them" are different
 * statements and the panel renders them differently.
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
  // Last mtime seen FOR THE CURRENT SESSION. Reset whenever the session
  // changes, or a zone reassignment would hand the new session the old file's
  // mtime and get a bogus `unchanged`.
  const lastMtime = useRef<{ sid: string; mtimeMs: number } | null>(null);

  // `manual` distinguishes the refresh BUTTON from the poll. Only the button
  // shows a busy state — a poll that flipped `refreshing` on and off would
  // write state (and re-render an open panel) every interval, which is exactly
  // what the mtime short-circuit and `promptsUnchanged` exist to avoid.
  const fetchPrompts = useCallback(
    async (manual = false) => {
      if (!claudeSessionId || inFlight.current) return;
      inFlight.current = true;
      if (manual) {
        setCached((prev) =>
          prev?.sid === claudeSessionId
            ? { ...prev, data: { ...prev.data, refreshing: true } }
            : prev,
        );
      }
      try {
        const since =
          lastMtime.current?.sid === claudeSessionId ? lastMtime.current.mtimeMs : undefined;
        const result = await invoke<CommandResponse>("transcript_read_user_prompts", {
          sessionId: claudeSessionId,
          configDir: configDir ?? null,
          projectPath: projectPath ?? null,
          sinceMtimeMs: since ?? null,
        });
        if (!result.success || !result.data) {
          lastMtime.current = null;
          setCached({
            sid: claudeSessionId,
            data: {
              status: "unavailable",
              prompts: [],
              reason: result.message ?? "transcript not found",
              refreshing: false,
            },
          });
          return;
        }
        const payload = result.data as UserPromptsResult;
        lastMtime.current = { sid: claudeSessionId, mtimeMs: payload.mtime_ms };
        if (payload.unchanged) {
          if (manual) {
            setCached((prev) =>
              prev?.sid === claudeSessionId
                ? { ...prev, data: { ...prev.data, refreshing: false } }
                : prev,
            );
          }
          return;
        }
        const next: UserPrompt[] = [];
        for (const p of payload.prompts) {
          const text = normalizePromptText(p.text ?? "");
          if (text === null) continue;
          next.push({ uuid: p.uuid, timestamp: p.timestamp, text });
        }
        setCached((prev) => {
          // Skip the state write on a no-new-prompt tick so an open panel does
          // not re-render (and fight the scroll anchor) every poll interval.
          if (
            prev?.sid === claudeSessionId &&
            prev.data.status === "ready" &&
            !prev.data.refreshing &&
            promptsUnchanged(prev.data.prompts, next)
          ) {
            return prev;
          }
          return {
            sid: claudeSessionId,
            data: { status: "ready", prompts: next, reason: null, refreshing: false },
          };
        });
      } catch (err) {
        // A hard Err is an IPC fault, not a data condition.
        console.error("transcript_read_user_prompts failed:", err);
        lastMtime.current = null;
        setCached({
          sid: claudeSessionId,
          data: { status: "unavailable", prompts: [], reason: "ipc_error", refreshing: false },
        });
      } finally {
        inFlight.current = false;
      }
    },
    [claudeSessionId, configDir, projectPath],
  );

  useEffect(() => {
    if (!enabled || !claudeSessionId) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- fetchPrompts is async; setCached fires after the IPC await, never synchronously in the effect body
    void fetchPrompts();
    const timer = setInterval(() => void fetchPrompts(), pollMs);
    return () => clearInterval(timer);
  }, [enabled, claudeSessionId, fetchPrompts, pollMs]);

  const state = claudeSessionId && cached?.sid === claudeSessionId ? cached.data : LOADING;
  return { ...state, refresh: () => void fetchPrompts(true) };
}
