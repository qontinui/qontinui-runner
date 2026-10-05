import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "./types";

/**
 * Whether a session is marked FINISHED — the WORK axis ("is there anything left
 * to do"), never liveness. A finished session keeps running until it exits.
 *
 * - `finished` — the runner-local marker is set, or coord's work axis reads
 *   `finished` (what `/finish-session` writes);
 * - `not_finished` — neither door has it marked;
 * - `unknown` — no local marker, and coord could not be read. coord is where
 *   `/finish-session` writes, so its silence is not evidence of "unfinished".
 */
export type FinishedVerdict = "finished" | "not_finished" | "unknown";

/** One session's merged state, as `terminal_session_finished_states` serves it. */
export interface SessionFinishedState {
  verdict: FinishedVerdict;
  /** Which door(s) carried a `finished` verdict. */
  source?: "local" | "coord" | "both";
  /** Unix millis of the earliest mark. */
  finishedAt?: number;
}

/** Claude session id → its finished state. A missing id is UNKNOWN. */
export type FinishedStates = Readonly<Record<string, SessionFinishedState>>;

/**
 * How often the page re-reads the markers. Each read is one bulk coord request
 * for every session on the page, so a mark made by `/finish-session` reaches
 * the pane border within this window.
 */
export const FINISHED_POLL_MS = 30_000;

const EMPTY: FinishedStates = Object.freeze({});
const VERDICTS: ReadonlySet<string> = new Set(["finished", "not_finished", "unknown"]);
const SOURCES: ReadonlySet<string> = new Set(["local", "coord", "both"]);

/**
 * Normalize the command's `data` into {@link FinishedStates}. Anything that is
 * not a recognised verdict is dropped, so it reads as UNKNOWN rather than as
 * either positive arm.
 */
export function parseFinishedStates(data: unknown): FinishedStates {
  const sessions =
    data && typeof data === "object" ? (data as { sessions?: unknown }).sessions : undefined;
  if (!sessions || typeof sessions !== "object") return EMPTY;
  const out: Record<string, SessionFinishedState> = {};
  for (const [id, raw] of Object.entries(sessions as Record<string, unknown>)) {
    if (!raw || typeof raw !== "object") continue;
    const { verdict, source, finishedAt } = raw as Record<string, unknown>;
    if (typeof verdict !== "string" || !VERDICTS.has(verdict)) continue;
    out[id] = {
      verdict: verdict as FinishedVerdict,
      ...(typeof source === "string" && SOURCES.has(source)
        ? { source: source as SessionFinishedState["source"] }
        : {}),
      ...(typeof finishedAt === "number" ? { finishedAt } : {}),
    };
  }
  return out;
}

/** Structural equality, so an unchanged poll keeps the previous object identity. */
export function sameFinishedStates(a: FinishedStates, b: FinishedStates): boolean {
  const aKeys = Object.keys(a);
  if (aKeys.length !== Object.keys(b).length) return false;
  return aKeys.every((k) => {
    const x = a[k];
    const y = b[k];
    return (
      y !== undefined &&
      x.verdict === y.verdict &&
      x.source === y.source &&
      x.finishedAt === y.finishedAt
    );
  });
}

/**
 * The finished state of each of `claudeSessionIds`, re-read every
 * {@link FINISHED_POLL_MS} while the window is visible and immediately when it
 * becomes visible again.
 *
 * The returned object keeps its identity across polls that changed nothing, so
 * passing it to memoized zone cells re-renders them only on a real change. A
 * failed read clears every state to UNKNOWN rather than keeping the last
 * answer: a stale `finished` is a claim this page can no longer back.
 */
export function useFinishedSessions(claudeSessionIds: readonly string[]): FinishedStates {
  const idsKey = useMemo(
    () => [...new Set(claudeSessionIds.filter((id) => id))].sort().join(","),
    [claudeSessionIds],
  );
  const [states, setStates] = useState<FinishedStates>(EMPTY);
  // The ids the latest request was made for — a response for an older id set
  // must not overwrite a newer one.
  const latestKey = useRef(idsKey);

  useEffect(() => {
    latestKey.current = idsKey;
    if (!idsKey) return;
    const ids = idsKey.split(",");
    let inFlight = false;

    const read = async () => {
      if (inFlight || document.visibilityState === "hidden") return;
      inFlight = true;
      let next: FinishedStates = EMPTY;
      try {
        const result = await invoke<CommandResponse>("terminal_session_finished_states", {
          claudeSessionIds: ids,
        });
        if (result.success) next = parseFinishedStates(result.data);
      } catch (err) {
        console.warn("[useFinishedSessions] terminal_session_finished_states failed:", err);
      } finally {
        inFlight = false;
      }
      if (latestKey.current !== idsKey) return;
      setStates((prev) => (sameFinishedStates(prev, next) ? prev : next));
    };

    void read();
    const timer = window.setInterval(() => void read(), FINISHED_POLL_MS);
    const onVisibility = () => {
      if (document.visibilityState === "visible") void read();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [idsKey]);

  // A page with no Claude sessions has nothing to report; never surface a
  // previous id set's answers for it.
  return idsKey ? states : EMPTY;
}
