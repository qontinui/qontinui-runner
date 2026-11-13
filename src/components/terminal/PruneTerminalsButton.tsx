import { useCallback, useEffect, useRef, useState, type Ref } from "react";
import { Sparkles } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";

import { getKnownClaudePanes, probeClaudeInPanes, rememberClaudePanes } from "./claudePresence";
import { useTerminalSession } from "./contexts";
import { isPlanTab, planPrune, type PruneSignals } from "./pruneTerminals";
import { usePrunePlan } from "./usePrunePlan";

const CONFIRM_TIMEOUT_MS = 4000;

type ClaudePresence = NonNullable<PruneSignals["claudePresence"]>;

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
 * Before arming, the first click asks the process table about every terminal
 * candidate in one snapshot (`probeClaudeInPanes`), and the armed plan closes
 * only the windows it reads `absent`: the tab-side signals can all miss an idle
 * Claude whose session id never bound. A pane read `live` is remembered
 * (`rememberClaudePanes`), so it is kept and compacted like any other AI
 * session and stops being offered for closing. The same snapshot re-checks the
 * page's remembered panes, and one whose Claude has exited is forgotten and
 * becomes a candidate again. When no candidate could be read at all, the
 * button says so rather than silently doing nothing.
 *
 * Renders nothing when pruning would change nothing.
 */
export function PruneTerminalsButton() {
  const { tabs, sessionStates, zoneLayout, closeTerminal } = useTerminalSession();
  const { assignments, requestCompaction } = zoneLayout;
  // Non-null = armed: the probe answers the confirm click will act on.
  const [presence, setPresence] = useState<ClaudePresence | null>(null);
  const [probing, setProbing] = useState(false);
  // Set when a probe could read none of the candidates; cleared on a timer.
  const [unchecked, setUnchecked] = useState(0);
  const armed = presence !== null;
  // Bumped per probe and on unmount, so a probe that outlives its click (a
  // remount, a second instance) never arms or re-lays out on its own.
  const probeSeq = useRef(0);
  useEffect(
    () => () => {
      probeSeq.current += 1;
    },
    [],
  );

  const plan = usePrunePlan(presence ?? undefined);
  const { targetZones, actionable } = plan;
  const closeCount = plan.closeIds.length;
  const keepCount = plan.keepIds.length;

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setPresence(null), CONFIRM_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [armed]);

  useEffect(() => {
    if (unchecked === 0) return;
    const timer = setTimeout(() => setUnchecked(0), CONFIRM_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [unchecked]);

  const confirming = armed && closeCount > 0;
  const { ref } = useUIElement({
    id: "terminal-prune-non-ai",
    type: "button",
    label: probing
      ? "Checking windows for running AI sessions"
      : unchecked > 0
        ? `Could not check ${unchecked} windows; kept them`
        : confirming
          ? `Confirm closing ${closeCount} non-AI windows`
          : "Keep only AI sessions",
  });

  const apply = useCallback(
    (keepIds: string[], closeIds: string[]) => {
      // Request first: the compaction waits for these ids to leave the tab list.
      requestCompaction(keepIds, closeIds);
      for (const id of closeIds) closeTerminal(id);
    },
    [requestCompaction, closeTerminal],
  );

  const onClick = useCallback(async () => {
    if (probing) return;
    if (armed || closeCount === 0) {
      setPresence(null);
      apply(plan.keepIds, plan.closeIds);
      return;
    }
    const byId = new Map(tabs.map((t) => [t.id, t]));
    const candidateIds = plan.closeIds.filter((id) => {
      const tab = byId.get(id);
      return tab !== undefined && !isPlanTab(tab);
    });
    const known = getKnownClaudePanes();
    const recheckIds = tabs.filter((t) => known.has(t.id)).map((t) => t.id);
    const seq = ++probeSeq.current;
    setUnchecked(0);
    setProbing(true);
    const probed = await probeClaudeInPanes([...candidateIds, ...recheckIds]);
    rememberClaudePanes(probed);
    if (seq !== probeSeq.current) return;
    setProbing(false);
    const unreadable = candidateIds.filter(
      (id) => probed[id] !== "live" && probed[id] !== "absent",
    ).length;
    const vetted = planPrune(tabs, assignments, {
      sessionStates,
      now: Date.now(),
      claudePresence: probed,
    });
    if (vetted.closeIds.length === 0) {
      // Every candidate may host Claude: nothing to confirm, only re-layout.
      apply(vetted.keepIds, []);
      if (unreadable > 0 && unreadable === candidateIds.length) setUnchecked(unreadable);
      return;
    }
    setPresence(probed);
  }, [probing, armed, closeCount, plan, apply, tabs, assignments, sessionStates]);

  if (!actionable) return null;

  const noun = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
  // Name every window the click will close: the AI test errs toward keeping,
  // but the operator is the last check on a shell they still need. Before the
  // probe the list is only candidates — the probe may rescue any of them.
  const names = plan.closeTitles.join(", ");
  const closing =
    names.length === 0
      ? ""
      : armed
        ? `\n\nCloses: ${names}`
        : `\n\nChecks, then may close: ${names}`;
  const title =
    unchecked > 0
      ? `Could not read the process table for ${noun(unchecked, "window")}, so ` +
        "nothing was closed: a window is closed only when no claude is seen running in it."
      : `Close ${noun(closeCount, "window")} without an AI session (shells, plan viewers) and fit ` +
        `${noun(keepCount, "AI session")} into a ${noun(targetZones, "zone")} grid${closing}`;

  return (
    <button
      ref={ref as Ref<HTMLButtonElement>}
      type="button"
      onClick={() => void onClick()}
      disabled={probing}
      title={title}
      className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] leading-none whitespace-nowrap transition-colors ${
        confirming
          ? "text-[#f7768e] bg-[#f7768e]/15 hover:bg-[#f7768e]/25"
          : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
      }`}
    >
      <Sparkles className="w-2.5 h-2.5" />
      {probing
        ? "Checking…"
        : unchecked > 0
          ? "Couldn't check"
          : confirming
            ? `Close ${noun(closeCount, "window")}?`
            : "Keep AI"}
    </button>
  );
}
