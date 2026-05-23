import { useCallback } from "react";
import { KeyboardShortcutsOverlay } from "./KeyboardShortcutsOverlay";
import { CommandPalette } from "./CommandPalette";
import { ZoneDiffOverlay } from "./ZoneDiffOverlay";
import {
  useTerminalSession,
  useZoneMetadata,
  useTransitionEffects,
  useUIStateCx,
} from "./contexts";

interface TerminalOverlaysProps {
  /** Callbacks from useZoneActions — kept as props until that hook moves to context */
  onSortZones?: () => void;
  onExport?: () => void;
}

const noop = () => {};
export function TerminalOverlays({ onSortZones = noop, onExport = noop }: TerminalOverlaysProps) {
  const session = useTerminalSession();
  const { tabs, zoneLayout, terminalRefs } = session;
  const { sessionStates, lastOutputLines, snapshots } = session;
  const { labelsAndTags, incrementMetric, addHistoryEvent } = useZoneMetadata();
  const transitionEffects = useTransitionEffects();
  const { state: uiState, dispatch, toggleFocusMode } = useUIStateCx();

  const approveTab = useCallback(
    (tabId: string) => {
      terminalRefs.current.get(tabId)?.current?.writeToTerminal("y\r");
      incrementMetric("totalApprovals");
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- terminalRefs is a stable ref object
    [incrementMetric],
  );

  const rejectTab = useCallback(
    (tabId: string) => {
      terminalRefs.current.get(tabId)?.current?.writeToTerminal("n\r");
      incrementMetric("totalRejections");
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- terminalRefs is a stable ref object
    [incrementMetric],
  );

  const approveAll = useCallback(() => {
    const ni = tabs.filter((t) => sessionStates[t.id] === "needs-input");
    incrementMetric("totalApprovals", ni.length);
    addHistoryEvent("Approve all", `${ni.length} sessions`, undefined, "#9ece6a");
    for (const tab of ni) {
      terminalRefs.current.get(tab.id)?.current?.writeToTerminal("y\r");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- terminalRefs is a stable ref object
  }, [tabs, sessionStates, incrementMetric, addHistoryEvent]);

  return (
    <>
      {uiState.showShortcutsOverlay && (
        <KeyboardShortcutsOverlay
          onClose={() => dispatch({ type: "SET_SHOW_SHORTCUTS", payload: false })}
        />
      )}

      {snapshots.diffZones &&
        (() => {
          const [z1, z2] = snapshots.diffZones;
          const tab1 = tabs.find((t) => t.id === zoneLayout.assignments[z1]);
          const tab2 = tabs.find((t) => t.id === zoneLayout.assignments[z2]);
          return (
            <ZoneDiffOverlay
              leftLabel={`Zone ${z1 + 1}: ${tab1?.title ?? "empty"}`}
              rightLabel={`Zone ${z2 + 1}: ${tab2?.title ?? "empty"}`}
              leftLines={tab1 ? (lastOutputLines[tab1.id] ?? []) : []}
              rightLines={tab2 ? (lastOutputLines[tab2.id] ?? []) : []}
              onClose={() => snapshots.setDiffZones(null)}
            />
          );
        })()}

      {snapshots.snapshotDiff && (
        <ZoneDiffOverlay
          leftLabel="Snapshot"
          rightLabel="Current"
          leftLines={snapshots.snapshotDiff.snapshot}
          rightLines={snapshots.snapshotDiff.current}
          onClose={snapshots.clearSnapshotDiff}
        />
      )}

      {uiState.showCommandPalette && (
        <CommandPalette
          onClose={() => dispatch({ type: "SET_SHOW_COMMAND_PALETTE", payload: false })}
          tabs={tabs}
          assignments={zoneLayout.assignments}
          sessionStates={sessionStates}
          focusedZone={zoneLayout.focusedZone}
          onFocusZone={zoneLayout.setFocusedZone}
          onApproveTab={approveTab}
          onRejectTab={rejectTab}
          onRestartZone={transitionEffects.handleRestartInZone}
          onTogglePin={labelsAndTags.togglePin}
          pinnedZones={labelsAndTags.pinnedZones}
          onApproveAll={approveAll}
          onSortZones={onSortZones}
          onExport={onExport}
          onToggleFocusMode={toggleFocusMode}
          focusMode={uiState.focusMode}
          onToggleAutoFocus={transitionEffects.toggleAutoFocus}
          autoFocus={transitionEffects.autoFocusNeedsInput}
          onToggleSound={transitionEffects.toggleSound}
          soundEnabled={transitionEffects.soundEnabled}
          zoneLabels={labelsAndTags.zoneLabels}
          onSetZoneLabel={labelsAndTags.setZoneLabel}
          zoneCount={zoneLayout.layout.zones.length}
          onCompareZones={(z1, z2) => {
            dispatch({ type: "SET_SHOW_COMMAND_PALETTE", payload: false });
            snapshots.setDiffZones([z1, z2]);
          }}
          onSnapshotZone={snapshots.snapshotZone}
          onCompareSnapshot={(tabId) => {
            snapshots.compareSnapshot(tabId);
            dispatch({ type: "SET_SHOW_COMMAND_PALETTE", payload: false });
          }}
          snapshotZones={snapshots.snapshotZones}
        />
      )}
    </>
  );
}
