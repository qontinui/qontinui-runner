/**
 * BatchActions — inline bulk controls for sessions awaiting input.
 *
 * Replaces the old floating `BatchOperationsBar` (which sat
 * `absolute bottom-4` over the zone grid and occluded the bottom-center
 * terminal). These controls now live INLINE in the top `StatusStrip`,
 * directly after its "N need input" pill — co-located with the signal
 * they act on, and out of the grid entirely.
 *
 * The strip already renders the waiting COUNT, so this component does not
 * repeat it; it only renders the actions (Approve all / Reject all /
 * Broadcast / Select). The strip auto-hides when nothing needs attention,
 * so there is no dismiss affordance.
 *
 * Approve-all blind-sends `y` to every waiting session, so when the batch
 * is large (>= APPROVE_CONFIRM_THRESHOLD) the first click arms a one-shot
 * inline confirm rather than firing immediately — a reflexive click at the
 * top of the page can't rubber-stamp many sessions unseen.
 */

import { useCallback, useMemo, useState } from "react";
import { CheckCheck, X, Send, MousePointerClick, HelpCircle } from "lucide-react";

import type { SessionState } from "./useZoneLayout";
import { useTerminalSession, useZoneMetadata, useUIStateCx } from "./contexts";

/** Above this many waiting sessions, Approve-all requires a confirm click. */
const APPROVE_CONFIRM_THRESHOLD = 3;
const CONFIRM_TIMEOUT_MS = 4000;
const LAST_ACTION_TIMEOUT_MS = 3000;

function expandTemplate(
  template: string,
  vars: { zone: number; n: number; title: string; tag: string },
): string {
  return template
    .replace(/\{zone\}/g, String(vars.zone))
    .replace(/\{n\}/g, String(vars.n))
    .replace(/\{title\}/g, vars.title)
    .replace(/\{tag\}/g, vars.tag);
}

export function BatchActions() {
  const session = useTerminalSession();
  const { tabs, zoneLayout, terminalRefs, sessionStates } = session;
  const assignments = zoneLayout.assignments;
  const { labelsAndTags, incrementMetric, addHistoryEvent } = useZoneMetadata();
  const zoneLabels = labelsAndTags.zoneLabels;
  const { state: uiState, dispatch } = useUIStateCx();
  const selectedZones = uiState.selectedZones;

  const [broadcastInput, setBroadcastInput] = useState("");
  const [showBroadcast, setShowBroadcast] = useState(false);
  const [lastAction, setLastAction] = useState<string | null>(null);
  const [confirmApprove, setConfirmApprove] = useState(false);

  const hasSelection = selectedZones && selectedZones.size > 0;

  // When zones are selected, actions target only those zones' tabs.
  const targetTabs = useMemo(() => {
    if (hasSelection && assignments) {
      const selectedTabIds = new Set([...selectedZones].map((z) => assignments[z]).filter(Boolean));
      return tabs.filter((t) => selectedTabIds.has(t.id));
    }
    return tabs;
  }, [tabs, hasSelection, selectedZones, assignments]);

  const needsInputTabs = targetTabs.filter((t) => sessionStates[t.id] === "needs-input");

  // Total waiting regardless of selection — decides whether the per-zone
  // Select affordance is worth showing (pointless with a single waiter).
  const totalWaiting = useMemo(
    () => tabs.filter((t) => sessionStates[t.id] === "needs-input").length,
    [tabs, sessionStates],
  );

  const writeToTab = useCallback(
    (tabId: string, text: string) => {
      const ref = terminalRefs.current.get(tabId);
      ref?.current?.writeToTerminal(text);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- terminalRefs is a stable ref object
    [],
  );

  const onMetrics = useCallback(
    (type: "approve" | "reject" | "broadcast", count: number) => {
      if (type === "approve") {
        incrementMetric("totalApprovals", count);
        addHistoryEvent("Batch approve", `${count} sessions`, undefined, "#9ece6a");
      } else if (type === "reject") {
        incrementMetric("totalRejections", count);
        addHistoryEvent("Batch reject", `${count} sessions`, undefined, "#f7768e");
      } else if (type === "broadcast") {
        incrementMetric("totalBroadcasts", count);
        addHistoryEvent("Broadcast", `${count} sessions`, undefined, "#7aa2f7");
      }
    },
    [incrementMetric, addHistoryEvent],
  );

  const flashAction = useCallback((text: string) => {
    setLastAction(text);
    setTimeout(() => setLastAction(null), LAST_ACTION_TIMEOUT_MS);
  }, []);

  const onSelectAllWaiting = useCallback(() => {
    const waiting = new Set<number>();
    for (const [zoneStr, tabId] of Object.entries(assignments)) {
      if ((sessionStates[tabId] as SessionState) === "needs-input") {
        waiting.add(Number(zoneStr));
      }
    }
    dispatch({ type: "SET_SELECTED_ZONES", payload: waiting });
  }, [assignments, sessionStates, dispatch]);

  const onClearSelection = useCallback(() => dispatch({ type: "CLEAR_SELECTION" }), [dispatch]);

  const runApprove = useCallback(() => {
    let count = 0;
    for (const tab of needsInputTabs) {
      writeToTab(tab.id, "y\r");
      count++;
    }
    onMetrics("approve", count);
    flashAction(`Approved ${count} session${count !== 1 ? "s" : ""}`);
  }, [needsInputTabs, writeToTab, onMetrics, flashAction]);

  const handleApproveAll = useCallback(() => {
    // Large batch → arm a one-shot confirm instead of firing blind.
    if (needsInputTabs.length >= APPROVE_CONFIRM_THRESHOLD && !confirmApprove) {
      setConfirmApprove(true);
      setTimeout(() => setConfirmApprove(false), CONFIRM_TIMEOUT_MS);
      return;
    }
    setConfirmApprove(false);
    runApprove();
  }, [needsInputTabs.length, confirmApprove, runApprove]);

  const handleRejectAll = useCallback(() => {
    setConfirmApprove(false);
    let count = 0;
    for (const tab of needsInputTabs) {
      writeToTab(tab.id, "n\r");
      count++;
    }
    onMetrics("reject", count);
    flashAction(`Rejected ${count} session${count !== 1 ? "s" : ""}`);
  }, [needsInputTabs, writeToTab, onMetrics, flashAction]);

  const hasTemplateVars = /\{(zone|n|title|tag)\}/.test(broadcastInput);

  const handleBroadcast = useCallback(() => {
    if (!broadcastInput.trim()) return;
    let count = 0;

    if (assignments && hasTemplateVars) {
      // Template mode: expand per-zone with contextual variables.
      let n = 0;
      for (const [zoneStr, tabId] of Object.entries(assignments)) {
        const state = sessionStates[tabId] ?? "idle";
        if (state !== "needs-input") continue;
        if (hasSelection && selectedZones && !selectedZones.has(Number(zoneStr))) continue;
        n++;
        const zoneIdx = Number(zoneStr);
        const tab = tabs.find((t) => t.id === tabId);
        const tags =
          zoneLabels?.[zoneIdx]
            ?.split(",")
            .map((t) => t.trim())
            .filter(Boolean) ?? [];

        const expanded = expandTemplate(broadcastInput, {
          zone: zoneIdx + 1,
          n,
          title: tab?.title ?? "",
          tag: tags[0] ?? "",
        });

        writeToTab(tabId, expanded + "\r");
        count++;
      }
    } else {
      // Plain text mode: send identical text to all targets.
      for (const tab of needsInputTabs) {
        writeToTab(tab.id, broadcastInput + "\r");
        count++;
      }
    }

    onMetrics("broadcast", count);
    flashAction(`Sent to ${count} session${count !== 1 ? "s" : ""}`);
    setBroadcastInput("");
    setShowBroadcast(false);
  }, [
    broadcastInput,
    needsInputTabs,
    writeToTab,
    onMetrics,
    flashAction,
    assignments,
    hasTemplateVars,
    sessionStates,
    hasSelection,
    selectedZones,
    tabs,
    zoneLabels,
  ]);

  // Nothing waiting (e.g. a selection that excludes every waiter) → render
  // nothing; the strip's own pill already conveys the count.
  if (needsInputTabs.length === 0 && !hasSelection) return null;

  const btn =
    "flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium leading-none whitespace-nowrap transition-colors";

  return (
    <div className="relative flex items-center gap-1">
      {lastAction ? (
        <span className="text-[10px] text-[#9ece6a] px-1 animate-pulse">{lastAction}</span>
      ) : (
        <>
          {hasSelection && (
            <span className="flex items-center gap-1 text-[10px] text-[#bb9af7]">
              <MousePointerClick className="w-2.5 h-2.5" />
              <span className="font-medium">{selectedZones.size} selected</span>
              <button
                type="button"
                onClick={onClearSelection}
                className="text-[9px] text-[#565f89] hover:text-[#a9b1d6] transition-colors"
              >
                clear
              </button>
            </span>
          )}

          <button
            type="button"
            onClick={handleApproveAll}
            className={`${btn} bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/25`}
            title={
              hasSelection
                ? "Approve the selected zones (sends y)"
                : "Approve every waiting session (sends y) — Ctrl+Shift+Enter"
            }
          >
            <CheckCheck className="w-2.5 h-2.5" />
            {confirmApprove ? `Approve ${needsInputTabs.length}?` : "Approve all"}
          </button>

          <button
            type="button"
            onClick={handleRejectAll}
            className={`${btn} bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25`}
            title={
              hasSelection
                ? "Reject the selected zones (sends n)"
                : "Reject every waiting session (sends n)"
            }
          >
            <X className="w-2.5 h-2.5" />
            Reject all
          </button>

          <button
            type="button"
            onClick={() => setShowBroadcast((v) => !v)}
            className={`${btn} ${
              showBroadcast
                ? "bg-[#7aa2f7]/20 text-[#7aa2f7]"
                : "bg-[#7aa2f7]/10 text-[#7aa2f7] hover:bg-[#7aa2f7]/20"
            }`}
            title={
              hasSelection
                ? "Broadcast text to the selected zones"
                : "Broadcast text to every waiting session"
            }
          >
            <Send className="w-2.5 h-2.5" />
            Broadcast
          </button>

          {totalWaiting > 1 && !hasSelection && (
            <button
              type="button"
              onClick={onSelectAllWaiting}
              className={`${btn} bg-[#bb9af7]/10 text-[#bb9af7] hover:bg-[#bb9af7]/20`}
              title="Select all waiting zones, then approve/reject/broadcast only those"
            >
              <MousePointerClick className="w-2.5 h-2.5" />
              Select
            </button>
          )}
        </>
      )}

      {/* Broadcast input — drops DOWN below the strip (the strip is top-
          pinned), so it overlays the grid only while open and on demand. */}
      {showBroadcast && (
        <div className="absolute left-0 top-full mt-1 z-50 flex items-center gap-2 px-3 py-2 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl">
          <input
            autoFocus
            value={broadcastInput}
            onChange={(e) => setBroadcastInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleBroadcast();
              if (e.key === "Escape") setShowBroadcast(false);
              e.stopPropagation();
            }}
            placeholder={`Send to ${needsInputTabs.length} waiting session${
              needsInputTabs.length !== 1 ? "s" : ""
            }…`}
            className="bg-[#13141f] border border-[#2a2d3d] rounded px-2 py-1 text-xs text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] w-64"
          />
          <button
            type="button"
            onClick={handleBroadcast}
            disabled={!broadcastInput.trim()}
            className="flex items-center gap-1 px-2 py-1 rounded text-xs bg-[#7aa2f7]/20 text-[#7aa2f7] hover:bg-[#7aa2f7]/30 disabled:opacity-40 transition-colors"
          >
            <Send className="w-3 h-3" />
            Send
          </button>
          <div className="relative group">
            <HelpCircle className="w-3 h-3 text-[#565f89] cursor-help" />
            <div className="hidden group-hover:block absolute top-full right-0 mt-1 w-48 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 p-2 text-[9px] text-[#a9b1d6]">
              <div className="font-medium text-[#c0caf5] mb-1">Template variables:</div>
              <div>
                <code className="text-[#7aa2f7]">{"{zone}"}</code> — zone number
              </div>
              <div>
                <code className="text-[#7aa2f7]">{"{n}"}</code> — iteration (1, 2, 3…)
              </div>
              <div>
                <code className="text-[#7aa2f7]">{"{title}"}</code> — session title
              </div>
              <div>
                <code className="text-[#7aa2f7]">{"{tag}"}</code> — first tag
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={() => setShowBroadcast(false)}
            className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
          >
            <X className="w-3 h-3" />
          </button>
        </div>
      )}
    </div>
  );
}
