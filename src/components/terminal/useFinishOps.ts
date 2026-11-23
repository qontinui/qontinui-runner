import { describeThrown } from "@/lib/utils";
import { useCallback, useRef, useState } from "react";
import { setSessionFinished } from "@/lib/session-ledger";
import type { FinishOp } from "./sinceRestart";

/**
 * Per-session Finish/Unfinish through `terminal_session_set_finished` — the
 * work axis, never the process (plan `2026-09-01-session-finished-marker-and-
 * unfinished-resume`, gap 4, delivered by plan
 * `2026-10-04-runner-session-roster-restore-picker` Phase 4).
 *
 * Tracks each session's op (`pending`, the last `failed` one, or a `settled`
 * one carrying the finish's nets notice — see `finishNetsNotice`) for
 * {@link finishButtonModel}. The roster's own `finished` flag stays the truth:
 * `onSettled` re-reads it after every attempt, which is also how a backend
 * no-op (already in that state, unknown id) shows as what it is.
 */
export function useFinishOps(onSettled: () => void): {
  ops: ReadonlyMap<string, FinishOp>;
  setFinished: (claudeSessionId: string, finished: boolean) => void;
  /**
   * Drop every `settled` op whose row's finished state no longer matches what
   * that op set (changed through another door), so its notice cannot come
   * back when the row later returns to that state. `finishedOf` answers
   * `undefined` for a row it does not hold, which is left alone.
   */
  reconcile: (finishedOf: (claudeSessionId: string) => boolean | undefined) => void;
} {
  const [ops, setOps] = useState<Map<string, FinishOp>>(new Map());
  // Per session: did its last Finish (through this view, this app run)
  // withdraw an implement→ship net? Only then does an Unfinish owe the
  // not-re-armed line (plan item 4). Unknown after a reload, so no line.
  const withdrewRef = useRef<Map<string, boolean>>(new Map());
  const setFinished = useCallback(
    (id: string, finished: boolean) => {
      setOps((prev) => new Map(prev).set(id, { phase: "pending", to: finished }));
      const earlierFinishWithdrew = !finished && withdrewRef.current.get(id) === true;
      setSessionFinished(id, finished, undefined, earlierFinishWithdrew)
        .then(({ notice, withdrew }) => {
          if (finished) withdrewRef.current.set(id, withdrew);
          else withdrewRef.current.delete(id);
          return notice;
        })
        .then((notice) =>
          setOps((prev) => {
            const next = new Map(prev);
            // A result the operator is owed (an implement→ship net left
            // armed, or not re-armed on Unfinish) stays beside the control.
            if (notice) next.set(id, { phase: "settled", to: finished, notice });
            else next.delete(id);
            return next;
          }),
        )
        .catch((err: unknown) =>
          setOps((prev) =>
            new Map(prev).set(id, {
              phase: "failed",
              to: finished,
              message: describeThrown(err, "update failed"),
            }),
          ),
        )
        .finally(onSettled);
    },
    [onSettled],
  );
  const reconcile = useCallback(
    (finishedOf: (claudeSessionId: string) => boolean | undefined) => {
      setOps((prev) => {
        let next: Map<string, FinishOp> | null = null;
        for (const [id, op] of prev) {
          if (op.phase !== "settled") continue;
          const f = finishedOf(id);
          if (f === undefined || f === op.to) continue;
          next ??= new Map(prev);
          next.delete(id);
          withdrewRef.current.delete(id);
        }
        return next ?? prev;
      });
    },
    [],
  );
  return { ops, setFinished, reconcile };
}
