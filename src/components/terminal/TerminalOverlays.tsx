import { useCallback } from "react";
import { deliverApprovals } from "./approveAll";
import { KeyboardShortcutsOverlay } from "./KeyboardShortcutsOverlay";
import { CommandPalette } from "./CommandPalette";
import { ZoneDiffOverlay } from "./ZoneDiffOverlay";
import {
  useTerminalSession,
  useZoneMetadata,
  useTransitionEffects,
  useUIStateCx,
} from "./contexts";
import { useHotField } from "./useTerminalHotStore";

/**
 * Live two-zone output diff.
 *
 * Split into its own component so the `lastOutputLines` subscription is only
 * mounted while the operator actually has the compare overlay open —
 * subscribing in `TerminalOverlays` itself would re-render the overlay layer
 * on every terminal output frame for nothing (plan Phase 1).
 */
function ZoneCompareOverlay({
  pageId,
  leftLabel,
  rightLabel,
  leftTabId,
  rightTabId,
  onClose,
}: {
  pageId: string;
  leftLabel: string;
  rightLabel: string;
  leftTabId: string | undefined;
  rightTabId: string | undefined;
  onClose: () => void;
}) {
  const lastOutputLines = useHotField(pageId, "lastOutputLines");
  return (
    <ZoneDiffOverlay
      leftLabel={leftLabel}
      rightLabel={rightLabel}
      leftLines={leftTabId ? (lastOutputLines[leftTabId] ?? []) : []}
      rightLines={rightTabId ? (lastOutputLines[rightTabId] ?? []) : []}
      onClose={onClose}
    />
  );
}

interface TerminalOverlaysProps {
  /** Callbacks from useZoneActions — kept as props until that hook moves to context */
  onSortZones?: () => void;
  onExport?: () => void;
}

const noop = () => {};
export function TerminalOverlays({ onSortZones = noop, onExport = noop }: TerminalOverlaysProps) {
  const session = useTerminalSession();
  const { tabs, zoneLayout, terminalRefs } = session;
  const { sessionStates, snapshots, pageId } = session;
  const { labelsAndTags, incrementMetric, addHistoryEvent } = useZoneMetadata();
  const transitionEffects = useTransitionEffects();
  const { state: uiState, dispatch, toggleFocusMode } = useUIStateCx();

  /**
   * The three approve/reject paths, counting DELIVERIES.
   *
   * All three used to be
   * `terminalRefs.current.get(id)?.current?.writeToTerminal("y\r")` followed
   * by an unconditional `incrementMetric` — the same silent optional chain
   * `/approve-all` had, with a worse consequence: the metric and the history
   * event recorded approvals that reached no process, so the page's OWN
   * metrics card and event log (what `/metrics` and `/history` render) carried
   * numbers nothing had made true. Routing through `deliverApprovals` gives an
   * unmounted pane a real path (`terminal_write` by id) and makes the counter
   * a count of what landed.
   */
  const approveTab = useCallback(
    async (tabId: string) => {
      const report = await deliverApprovals([tabId], terminalRefs.current, "y\r");
      if (report.delivered > 0) incrementMetric("totalApprovals", report.delivered);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- terminalRefs is a stable ref object
    [incrementMetric],
  );

  const rejectTab = useCallback(
    async (tabId: string) => {
      const report = await deliverApprovals([tabId], terminalRefs.current, "n\r");
      if (report.delivered > 0) incrementMetric("totalRejections", report.delivered);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- terminalRefs is a stable ref object
    [incrementMetric],
  );

  const approveAll = useCallback(async () => {
    const ni = tabs.filter((t) => sessionStates[t.id] === "needs-input");
    const report = await deliverApprovals(
      ni.map((t) => t.id),
      terminalRefs.current,
      "y\r",
    );
    if (report.delivered > 0) incrementMetric("totalApprovals", report.delivered);
    // The history entry names both numbers when they differ, so a partial
    // approve-all is legible afterwards rather than only in the moment.
    addHistoryEvent(
      "Approve all",
      report.delivered === report.targeted
        ? `${report.delivered} sessions`
        : `${report.delivered} of ${report.targeted} sessions`,
      undefined,
      report.delivered === report.targeted ? "#9ece6a" : "#e0af68",
    );
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
            <ZoneCompareOverlay
              pageId={pageId}
              leftLabel={`Zone ${z1 + 1}: ${tab1?.title ?? "empty"}`}
              rightLabel={`Zone ${z2 + 1}: ${tab2?.title ?? "empty"}`}
              leftTabId={tab1?.id}
              rightTabId={tab2?.id}
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
