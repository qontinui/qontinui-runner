import { useState, useCallback, useMemo } from "react";
import { CheckCheck, X, Send, AlertTriangle, MousePointerClick, HelpCircle } from "lucide-react";
import type { SessionState } from "./useZoneLayout";
import {
  useTerminalCore,
  useSessionState,
  useZoneMetadata,
  useTransitionEffects,
  useUIStateCx,
} from "./contexts";

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

export function BatchOperationsBar() {
  // Read from contexts
  const { tabs, zoneLayout, terminalRefs } = useTerminalCore();
  const assignments = zoneLayout.assignments;
  const { sessionStates } = useSessionState();
  const { labelsAndTags, incrementMetric, addHistoryEvent } = useZoneMetadata();
  const zoneLabels = labelsAndTags.zoneLabels;
  const transitionEffects = useTransitionEffects();
  const { state: uiState, dispatch } = useUIStateCx();
  const selectedZones = uiState.selectedZones;
  const onDismiss = useCallback(() => transitionEffects.setBatchBarDismissed(true), [transitionEffects]);
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
  const onMetrics = useCallback((type: "approve" | "reject" | "broadcast", count: number) => {
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
  }, [incrementMetric, addHistoryEvent]);
  const [broadcastInput, setBroadcastInput] = useState("");
  const [showBroadcast, setShowBroadcast] = useState(false);
  const [lastAction, setLastAction] = useState<string | null>(null);

  const hasSelection = selectedZones && selectedZones.size > 0;

  // When zones are selected, only target those zones' tabs
  const targetTabs = useMemo(() => {
    if (hasSelection && assignments) {
      const selectedTabIds = new Set([...selectedZones].map((z) => assignments[z]).filter(Boolean));
      return tabs.filter((t) => selectedTabIds.has(t.id));
    }
    return tabs;
  }, [tabs, hasSelection, selectedZones, assignments]);

  const needsInputTabs = targetTabs.filter((t) => sessionStates[t.id] === "needs-input");
  const errorTabs = targetTabs.filter((t) => sessionStates[t.id] === "error");

  const writeToTab = useCallback(
    (tabId: string, text: string) => {
      const ref = terminalRefs.get(tabId);
      ref?.current?.writeToTerminal(text);
    },
    [terminalRefs],
  );

  const handleApproveAll = useCallback(() => {
    let count = 0;
    for (const tab of needsInputTabs) {
      writeToTab(tab.id, "y\r");
      count++;
    }
    onMetrics?.("approve", count);
    setLastAction(`Approved ${count} session${count !== 1 ? "s" : ""}`);
    setTimeout(() => setLastAction(null), 3000);
  }, [needsInputTabs, writeToTab, onMetrics]);

  const handleRejectAll = useCallback(() => {
    let count = 0;
    for (const tab of needsInputTabs) {
      writeToTab(tab.id, "n\r");
      count++;
    }
    onMetrics?.("reject", count);
    setLastAction(`Rejected ${count} session${count !== 1 ? "s" : ""}`);
    setTimeout(() => setLastAction(null), 3000);
  }, [needsInputTabs, writeToTab, onMetrics]);

  const hasTemplateVars = /\{(zone|n|title|tag)\}/.test(broadcastInput);

  const handleBroadcast = useCallback(() => {
    if (!broadcastInput.trim()) return;
    let count = 0;

    if (assignments && hasTemplateVars) {
      // Template mode: expand per-zone with contextual variables
      let n = 0;
      for (const [zoneStr, tabId] of Object.entries(assignments)) {
        const state = sessionStates[tabId] ?? "idle";
        if (state !== "needs-input") continue;
        // Skip if zone selection is active and this zone isn't selected
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
      // Plain text mode: send identical text to all targets
      for (const tab of needsInputTabs) {
        writeToTab(tab.id, broadcastInput + "\r");
        count++;
      }
    }

    onMetrics?.("broadcast", count);
    setLastAction(`Sent to ${count} session${count !== 1 ? "s" : ""}`);
    setBroadcastInput("");
    setShowBroadcast(false);
    setTimeout(() => setLastAction(null), 3000);
  }, [
    broadcastInput,
    needsInputTabs,
    writeToTab,
    onMetrics,
    assignments,
    hasTemplateVars,
    sessionStates,
    hasSelection,
    selectedZones,
    tabs,
    zoneLabels,
  ]);

  if (needsInputTabs.length === 0 && errorTabs.length === 0) {
    return null;
  }

  return (
    <div className="absolute bottom-4 left-1/2 -translate-x-1/2 z-30 flex flex-col items-center gap-2">
      {/* Broadcast input row */}
      {showBroadcast && (
        <div className="flex items-center gap-2 px-3 py-2 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl">
          <input
            autoFocus
            value={broadcastInput}
            onChange={(e) => setBroadcastInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleBroadcast();
              if (e.key === "Escape") setShowBroadcast(false);
              e.stopPropagation();
            }}
            placeholder={`Send to ${needsInputTabs.length} waiting session${needsInputTabs.length !== 1 ? "s" : ""}...`}
            className="bg-[#13141f] border border-[#2a2d3d] rounded px-2 py-1 text-xs text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] w-64"
          />
          <button
            onClick={handleBroadcast}
            disabled={!broadcastInput.trim()}
            className="flex items-center gap-1 px-2 py-1 rounded text-xs bg-[#7aa2f7]/20 text-[#7aa2f7] hover:bg-[#7aa2f7]/30 disabled:opacity-40 transition-colors"
          >
            <Send className="w-3 h-3" />
            Send
          </button>
          <div className="relative group">
            <HelpCircle className="w-3 h-3 text-[#565f89] cursor-help" />
            <div className="hidden group-hover:block absolute bottom-full left-0 mb-1 w-48 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 p-2 text-[9px] text-[#a9b1d6]">
              <div className="font-medium text-[#c0caf5] mb-1">Template variables:</div>
              <div>
                <code className="text-[#7aa2f7]">{"{zone}"}</code> — zone number
              </div>
              <div>
                <code className="text-[#7aa2f7]">{"{n}"}</code> — iteration (1, 2, 3...)
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
            onClick={() => setShowBroadcast(false)}
            className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
          >
            <X className="w-3 h-3" />
          </button>
        </div>
      )}

      {/* Main bar */}
      <div className="flex items-center gap-2 px-3 py-2 bg-[#1a1b26]/95 backdrop-blur-sm border border-[#2a2d3d] rounded-lg shadow-xl">
        {/* Last action feedback */}
        {lastAction && (
          <span className="text-[10px] text-[#9ece6a] px-2 animate-pulse">{lastAction}</span>
        )}

        {/* Selection indicator */}
        {hasSelection && !lastAction && (
          <>
            <div className="flex items-center gap-1 text-[10px] text-[#bb9af7]">
              <MousePointerClick className="w-3 h-3" />
              <span className="font-medium">{selectedZones.size} selected</span>
            </div>
            {onClearSelection && (
              <button
                onClick={onClearSelection}
                className="text-[9px] text-[#565f89] hover:text-[#a9b1d6] transition-colors"
              >
                clear
              </button>
            )}
            <div className="w-px h-4 bg-[#2a2d3d]" />
          </>
        )}

        {/* Needs input section */}
        {needsInputTabs.length > 0 && !lastAction && (
          <>
            <div className="flex items-center gap-1.5 text-[10px] text-[#e0af68]">
              <span className="w-2 h-2 rounded-full bg-[#e0af68] animate-pulse" />
              <span className="font-medium">{needsInputTabs.length} waiting</span>
            </div>

            <div className="w-px h-4 bg-[#2a2d3d]" />

            {/* Approve */}
            <button
              onClick={handleApproveAll}
              className="flex items-center gap-1 px-2 py-1 rounded text-[10px] font-medium bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/25 transition-colors"
              title={
                hasSelection
                  ? "Approve selected zones"
                  : "Approve all waiting sessions (Ctrl+Shift+Enter)"
              }
            >
              <CheckCheck className="w-3 h-3" />
              {hasSelection ? "Approve" : "Approve All"}
            </button>

            {/* Reject */}
            <button
              onClick={handleRejectAll}
              className="flex items-center gap-1 px-2 py-1 rounded text-[10px] font-medium bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25 transition-colors"
              title={hasSelection ? "Reject selected zones" : "Reject all waiting sessions"}
            >
              <X className="w-3 h-3" />
              {hasSelection ? "Reject" : "Reject All"}
            </button>

            {/* Broadcast */}
            <button
              onClick={() => setShowBroadcast(!showBroadcast)}
              className={`flex items-center gap-1 px-2 py-1 rounded text-[10px] font-medium transition-colors ${
                showBroadcast
                  ? "bg-[#7aa2f7]/20 text-[#7aa2f7]"
                  : "bg-[#2a2d3d] text-[#a9b1d6] hover:bg-[#2a2d3d]/80"
              }`}
              title={
                hasSelection ? "Broadcast to selected zones" : "Broadcast to all waiting sessions"
              }
            >
              <Send className="w-3 h-3" />
              Broadcast
            </button>

            {/* Select all waiting (only when no selection) */}
            {!hasSelection && onSelectAllWaiting && (
              <button
                onClick={onSelectAllWaiting}
                className="flex items-center gap-1 px-2 py-1 rounded text-[10px] font-medium bg-[#bb9af7]/10 text-[#bb9af7] hover:bg-[#bb9af7]/20 transition-colors"
                title="Select all waiting zones for targeted actions"
              >
                <MousePointerClick className="w-3 h-3" />
                Select
              </button>
            )}
          </>
        )}

        {/* Error section */}
        {errorTabs.length > 0 && !lastAction && (
          <>
            {needsInputTabs.length > 0 && <div className="w-px h-4 bg-[#2a2d3d]" />}
            <div className="flex items-center gap-1.5 text-[10px] text-[#f7768e]">
              <AlertTriangle className="w-3 h-3" />
              <span>
                {errorTabs.length} error{errorTabs.length !== 1 ? "s" : ""}
              </span>
            </div>
          </>
        )}

        {/* Dismiss */}
        <button
          onClick={onDismiss}
          className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors ml-1"
          title="Dismiss"
        >
          <X className="w-3 h-3" />
        </button>
      </div>
    </div>
  );
}
