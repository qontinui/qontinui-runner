import { useCallback, useEffect, useState, type Ref } from "react";
import { Sparkles } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";

import { probeAgentPanes } from "./agentPanes";
import { useTerminalSession } from "./contexts";
import { panesSafeToClose } from "./pruneTerminals";
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
 * The confirming click re-reads the process table (`terminal_agent_panes`,
 * fresh) before closing anything, and closes only the windows that read still
 * hosts no agent CLI. A pane where an agent started since the plan was drawn,
 * or whose subtree cannot be read, is kept, and the button says how many it
 * kept ("Kept N — agent or unreadable").
 *
 * Renders nothing when pruning would change nothing.
 */
export function PruneTerminalsButton() {
  const { zoneLayout, closeTerminal } = useTerminalSession();
  const { requestCompaction } = zoneLayout;
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [keptOnCheck, setKeptOnCheck] = useState(0);

  const plan = usePrunePlan();
  const { targetZones, actionable } = plan;
  const closeCount = plan.closeIds.length;
  const keepCount = plan.keepIds.length;

  useEffect(() => {
    if (keptOnCheck === 0) return;
    const timer = setTimeout(() => setKeptOnCheck(0), CONFIRM_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [keptOnCheck]);

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

  const { applyAgentProbe } = plan;
  const onClick = useCallback(async () => {
    if (busy) return;
    if (closeCount > 0 && !armed) {
      setKeptOnCheck(0);
      setArmed(true);
      return;
    }
    setArmed(false);
    let closeIds = plan.closeIds;
    let keepIds = plan.keepIds;
    if (closeIds.length > 0) {
      setBusy(true);
      const fresh = await probeAgentPanes(closeIds, true);
      setBusy(false);
      if (fresh) applyAgentProbe(fresh);
      const safe = panesSafeToClose(closeIds, fresh);
      if (safe.length !== closeIds.length) {
        const safeSet = new Set(safe);
        const refused = closeIds.filter((id) => !safeSet.has(id));
        setKeptOnCheck(refused.length);
        keepIds = [...keepIds, ...refused];
        closeIds = safe;
      }
    }
    // Request first: the compaction waits for these ids to leave the tab list.
    requestCompaction(keepIds, closeIds);
    for (const id of closeIds) closeTerminal(id);
  }, [busy, closeCount, armed, plan, applyAgentProbe, requestCompaction, closeTerminal]);

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
      onClick={() => void onClick()}
      disabled={busy}
      title={title}
      className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] leading-none whitespace-nowrap transition-colors ${
        armed
          ? "text-[#f7768e] bg-[#f7768e]/15 hover:bg-[#f7768e]/25"
          : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
      }`}
    >
      <Sparkles className="w-2.5 h-2.5" />
      {armed
        ? `Close ${noun(closeCount, "window")}?`
        : keptOnCheck > 0
          ? `Kept ${keptOnCheck} — agent or unreadable`
          : "Keep AI"}
    </button>
  );
}
