import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { probeAgentPanes } from "./agentPanes";
import { useTerminalSession } from "./contexts";
import { isPruneActionable, planPrune, type PrunePlan } from "./pruneTerminals";
import { useNow1Hz } from "./useNow1Hz";
import { resolveLayout } from "./useZoneLayout";

/** Pause between one probe of the close candidates finishing and the next. */
const AGENT_PROBE_INTERVAL_MS = 5_000;

export type PrunePlanState = PrunePlan & {
  targetZones: number;
  actionable: boolean;
  /** Merge a probe answer taken elsewhere (the button's fresh re-probe). */
  applyAgentProbe: (panes: Record<string, string>) => void;
};

/**
 * The "Keep AI" plan for the current page, plus whether applying it would
 * change anything. Shared by `PruneTerminalsButton` (which renders and runs it)
 * and `StatusStrip` (which mounts the button only when this is actionable).
 *
 * `now` comes from the shared 1Hz tick, so a just-launched tab drops out of
 * the launch grace window (and into the close count) on its own.
 *
 * The tabs the tab-level signals would close are also probed in the runner's
 * process table (`terminal_agent_panes`), so a pane running a non-Claude agent
 * CLI — `pi` on DeepSeek, `codex`, `gemini` — counts as an AI session. Only
 * those candidates are probed, and only while there are any, so a page of
 * Claude sessions costs no probes at all. Probes are chained (the next starts
 * {@link AGENT_PROBE_INTERVAL_MS} after the last one answered), so a slow
 * process-table read never stacks calls or lets an older answer land last.
 */
export function usePrunePlan(): PrunePlanState {
  const { tabs, zoneLayout, sessionStates } = useTerminalSession();
  const { assignments, layout } = zoneLayout;
  const zoneCount = layout.zones.length;
  const now = useNow1Hz();

  // Candidates: what the tab-level signals alone would close.
  const candidateKey = useMemo(
    () => planPrune(tabs, assignments, { sessionStates, now }).closeIds.join("\n"),
    [tabs, assignments, sessionStates, now],
  );
  const [agentPanes, setAgentPanes] = useState<Record<string, string>>({});
  // Answers are applied in issue order: a stale answer never overwrites one
  // taken after it (the fresh re-probe included).
  const seqRef = useRef(0);
  const appliedRef = useRef(0);
  const apply = useCallback((seq: number, panes: Record<string, string>) => {
    if (seq < appliedRef.current) return;
    appliedRef.current = seq;
    setAgentPanes((prev) => ({ ...prev, ...panes }));
  }, []);
  const applyAgentProbe = useCallback(
    (panes: Record<string, string>) => apply(++seqRef.current, panes),
    [apply],
  );

  useEffect(() => {
    const ids = candidateKey ? candidateKey.split("\n") : [];
    if (ids.length === 0) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const probe = () => {
      const seq = ++seqRef.current;
      void probeAgentPanes(ids).then((r) => {
        if (cancelled) return;
        if (r) apply(seq, r);
        timer = setTimeout(probe, AGENT_PROBE_INTERVAL_MS);
      });
    };
    probe();
    return () => {
      cancelled = true;
      if (timer !== undefined) clearTimeout(timer);
    };
  }, [candidateKey, apply]);

  return useMemo(() => {
    const plan = planPrune(tabs, assignments, { sessionStates, now, agentPanes });
    const targetZones = resolveLayout(plan.layoutId, plan.keepIds.length).zones.length;
    const actionable = isPruneActionable(plan, zoneCount, targetZones, assignments);
    return { ...plan, targetZones, actionable, applyAgentProbe };
  }, [tabs, assignments, zoneCount, sessionStates, now, agentPanes, applyAgentProbe]);
}
