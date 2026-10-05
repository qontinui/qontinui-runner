import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { CommandResponse } from "./types";

/**
 * Whether a session is marked FINISHED — the WORK axis ("is there anything left
 * to do"), never liveness. A finished session keeps running until it exits.
 *
 * - `finished` — the runner-local marker is set, or coord's work axis reads
 *   `finished` (the two doors `/finish-session` writes through);
 * - `not_finished` — not marked, or coord has since moved off the mark;
 * - `unknown` — nothing local decides it, and coord could not say.
 */
export type FinishedVerdict = "finished" | "not_finished" | "unknown";

/** One session's state, as `terminal_session_finished_states` serves it. */
export interface SessionFinishedState {
  verdict: FinishedVerdict;
  /** Which door(s) carried a `finished` verdict. */
  source?: "local" | "coord" | "both";
  /** Unix millis of the earliest mark. */
  finishedAt?: number;
  /**
   * Set by this hook, never by the backend: the CURRENT read is `unknown`, and
   * the last read that could answer said `finished`. The pane keeps a dimmed
   * band for it rather than dropping the band on every coord blip — dropping it
   * would resize the pane's live PTY twice per blip — and says so on its
   * `data-session-finished-held` attribute.
   */
  held?: boolean;
}

/** Claude session id → its finished state. A missing id is UNKNOWN. */
export type FinishedStates = Readonly<Record<string, SessionFinishedState>>;

/**
 * How often the page re-reads the markers. Each read is one bulk coord request
 * for every session on the page. A runner-local mark (the `/finish-session`
 * first rung) arrives at once by event; a coord-only mark within this window.
 */
export const FINISHED_POLL_MS = 30_000;

/**
 * Emitted by the runner when a session's local finished marker changes. Must
 * equal `FINISHED_CHANGED_EVENT` in `src-tauri/src/commands/terminal_finished.rs`
 * (pinned by `useFinishedSessions.test.ts`).
 */
export const FINISHED_CHANGED_EVENT = "terminal-session-finished-changed";

/**
 * Extra reads after a local-change event. An UNMARK reaches coord through the
 * runner's outbox, so the read the event triggers can still find coord saying
 * `finished`; these converge the band once the outbox has drained.
 */
export const FINISHED_EVENT_FOLLOWUP_MS: readonly number[] = [3_000, 12_000];

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

/** `p` as a held entry: the current verdict is unknown, the band is kept. */
function asHeld(p: SessionFinishedState): SessionFinishedState {
  return {
    verdict: "unknown",
    held: true,
    ...(p.source ? { source: p.source } : {}),
    ...(p.finishedAt !== undefined ? { finishedAt: p.finishedAt } : {}),
  };
}

/**
 * Carry a `finished` across reads that cannot confirm it.
 *
 * For each of `ids` (the sessions just asked about):
 *
 * - **the read failed outright** (no entry): a previous `finished`, or held,
 *   entry is held;
 * - **the backend answered `unknown`**: it is held only when coord carried it.
 *   `unknown` means no local mark and coord unread, so a band that only the
 *   LOCAL mark ever backed has lost its sole evidence — it was unmarked, and
 *   holding it would keep a band nothing supports;
 * - **any other answer** replaces it: `not_finished` drops the band, `finished`
 *   confirms it.
 *
 * Sessions NOT in `ids` (another page's) keep their last state, as held when it
 * was finished. Their panes then come back with the band they left with — the
 * same border width, so no PTY resize — dimmed until the first read confirms
 * or drops it.
 */
export function holdFinished(
  prev: FinishedStates,
  next: FinishedStates,
  ids: readonly string[],
): FinishedStates {
  const out: Record<string, SessionFinishedState> = {};
  const asked = new Set(ids);
  for (const [id, p] of Object.entries(prev)) {
    if (!asked.has(id) && (p.verdict === "finished" || p.held === true)) out[id] = asHeld(p);
  }
  for (const id of ids) {
    const n = next[id];
    const p = prev[id];
    const wasFinished = p !== undefined && (p.verdict === "finished" || p.held === true);
    const coordBacked = p?.source === "coord" || p?.source === "both";
    if (wasFinished && (n === undefined || (n.verdict === "unknown" && coordBacked))) {
      out[id] = asHeld(p);
    } else if (n !== undefined) {
      out[id] = n;
    }
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
      x.finishedAt === y.finishedAt &&
      x.held === y.held
    );
  });
}

/**
 * The finished state of each of `claudeSessionIds`: read on mount, every
 * {@link FINISHED_POLL_MS} while the window is visible, when it becomes visible
 * again, and at once when the runner reports a local mark changing.
 *
 * The returned object keeps its identity across reads that changed nothing, so
 * memoized consumers re-render only on a real change.
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
    // A trigger that lands while a read is in flight must not be dropped: that
    // read may have been answered before the change it announces.
    let rerun = false;
    let disposed = false;

    const read = async (): Promise<void> => {
      if (disposed || document.visibilityState === "hidden") return;
      if (inFlight) {
        rerun = true;
        return;
      }
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
      if (disposed || latestKey.current !== idsKey) return;
      setStates((prev) => {
        const merged = holdFinished(prev, next, ids);
        return sameFinishedStates(prev, merged) ? prev : merged;
      });
      if (rerun) {
        rerun = false;
        void read();
      }
    };

    void read();
    const timer = window.setInterval(() => void read(), FINISHED_POLL_MS);
    const followups = new Set<number>();
    const onVisibility = () => {
      if (document.visibilityState === "visible") void read();
    };
    document.addEventListener("visibilitychange", onVisibility);
    let unlisten: (() => void) | undefined;
    listen<{ claudeSessionId?: string }>(FINISHED_CHANGED_EVENT, (event) => {
      if (!event.payload?.claudeSessionId || ids.includes(event.payload.claudeSessionId)) {
        void read();
        for (const ms of FINISHED_EVENT_FOLLOWUP_MS) {
          const t = window.setTimeout(() => {
            followups.delete(t);
            void read();
          }, ms);
          followups.add(t);
        }
      }
    })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch((err) => console.warn("[useFinishedSessions] listen failed:", err));

    return () => {
      disposed = true;
      window.clearInterval(timer);
      for (const t of followups) window.clearTimeout(t);
      document.removeEventListener("visibilitychange", onVisibility);
      unlisten?.();
    };
  }, [idsKey]);

  // A page with no Claude sessions has nothing to report; never surface a
  // previous id set's answers for it.
  return idsKey ? states : EMPTY;
}
