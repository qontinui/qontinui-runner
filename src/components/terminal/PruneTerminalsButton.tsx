import { useCallback, useEffect, useState, type Ref } from "react";
import { Sparkles } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";

import { useTerminalSession } from "./contexts";
import { applyPrunePlan } from "./pruneTerminals";
import { usePrunePlan } from "./usePrunePlan";

const CONFIRM_TIMEOUT_MS = 4000;

/**
 * "Keep AI" — close every window on this page that is not hosting an AI
 * session, then compact the grid to the smallest layout that fits the
 * sessions that are left (see `pruneTerminals.ts` for the rules).
 *
 * Closing a plain shell kills whatever runs in it (a dev server, a watch), so
 * when there is anything to close the first click only arms an inline confirm
 * naming the count — the same one-shot pattern as `BatchActions`' Approve all.
 * A pure re-layout (nothing to close) applies on the first click.
 *
 * Renders nothing when pruning would change nothing.
 */
export function PruneTerminalsButton() {
  const { zoneLayout, closeTerminal } = useTerminalSession();
  const { requestCompaction } = zoneLayout;
  const [armed, setArmed] = useState(false);

  const plan = usePrunePlan();
  const { targetZones, actionable } = plan;
  const closeCount = plan.closeIds.length;
  const keepCount = plan.keepIds.length;

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), CONFIRM_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [armed]);

  const { ref } = useUIElement({
    id: "terminal-prune-non-ai",
    type: "button",
    label: armed ? `Confirm closing ${closeCount} non-AI windows` : "Keep only AI sessions",
  });

  const onClick = useCallback(() => {
    if (closeCount > 0 && !armed) {
      setArmed(true);
      return;
    }
    setArmed(false);
    applyPrunePlan(plan, { requestCompaction, closeTerminal });
  }, [closeCount, armed, plan, requestCompaction, closeTerminal]);

  if (!actionable) return null;

  const noun = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
  // Name every window the click will close: the AI test errs toward keeping,
  // but the operator is the last check on a shell they still need.
  const closing = plan.closeTitles.length > 0 ? `\n\nCloses: ${plan.closeTitles.join(", ")}` : "";
  const title =
    `Close ${noun(closeCount, "window")} without an AI session (shells, plan viewers) and fit ` +
    `${noun(keepCount, "AI session")} into a ${noun(targetZones, "zone")} grid${closing}`;

  return (
    <button
      ref={ref as Ref<HTMLButtonElement>}
      type="button"
      onClick={onClick}
      title={title}
      className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] leading-none whitespace-nowrap transition-colors ${
        armed
          ? "text-[#f7768e] bg-[#f7768e]/15 hover:bg-[#f7768e]/25"
          : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
      }`}
    >
      <Sparkles className="w-2.5 h-2.5" />
      {armed ? `Close ${noun(closeCount, "window")}?` : "Keep AI"}
    </button>
  );
}
