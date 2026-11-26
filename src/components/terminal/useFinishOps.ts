import { describeThrown } from "@/lib/utils";
import { useCallback, useState } from "react";
import { setSessionFinished } from "@/lib/session-ledger";
import type { FinishOp } from "./sinceRestart";

/**
 * Per-session Finish/Unfinish through `terminal_session_set_finished` — the
 * work axis, never the process (plan `2026-09-01-session-finished-marker-and-
 * unfinished-resume`, gap 4, delivered by plan
 * `2026-10-04-runner-session-roster-restore-picker` Phase 4).
 *
 * Tracks each session's op (`pending`, or the last `failed` one) for
 * {@link finishButtonModel}. The roster's own `finished` flag stays the truth:
 * `onSettled` re-reads it after every attempt, which is also how a backend
 * no-op (already in that state, unknown id) shows as what it is.
 */
export function useFinishOps(onSettled: () => void): {
  ops: ReadonlyMap<string, FinishOp>;
  setFinished: (claudeSessionId: string, finished: boolean) => void;
} {
  const [ops, setOps] = useState<Map<string, FinishOp>>(new Map());
  const setFinished = useCallback(
    (id: string, finished: boolean) => {
      setOps((prev) => new Map(prev).set(id, { phase: "pending", to: finished }));
      setSessionFinished(id, finished)
        .then(() =>
          setOps((prev) => {
            const next = new Map(prev);
            next.delete(id);
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
  return { ops, setFinished };
}
