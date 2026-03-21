import { KeyboardShortcutsOverlay } from "./KeyboardShortcutsOverlay";
import { CommandPalette } from "./CommandPalette";
import { ZoneDiffOverlay } from "./ZoneDiffOverlay";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState } from "./useZoneLayout";

interface TerminalOverlaysProps {
  showShortcutsOverlay: boolean;
  onCloseShortcuts: () => void;
  showCommandPalette: boolean;
  onCloseCommandPalette: () => void;
  tabs: TerminalTab[];
  assignments: Record<number, string>;
  sessionStates: Record<string, SessionState>;
  focusedZone: number;
  onFocusZone: (idx: number) => void;
  onApproveTab: (tabId: string) => void;
  onRejectTab: (tabId: string) => void;
  onRestartZone: (zoneIdx: number) => void;
  onTogglePin: (zoneIdx: number) => void;
  pinnedZones: Set<number>;
  onApproveAll: () => void;
  onSortZones: () => void;
  onExport: () => void;
  onToggleFocusMode: () => void;
  focusMode: boolean;
  onToggleAutoFocus: () => void;
  autoFocus: boolean;
  onToggleSound: () => void;
  soundEnabled: boolean;
  zoneLabels: Record<number, string>;
  onSetZoneLabel: (zoneIdx: number, label: string) => void;
  zoneCount: number;
  onCompareZones: (z1: number, z2: number) => void;
  onSnapshotZone: (tabId: string) => void;
  onCompareSnapshot: (tabId: string) => void;
  snapshotZones: Set<string>;
  diffZones: [number, number] | null;
  lastOutputLines: Record<string, string[]>;
  onCloseDiff: () => void;
  snapshotDiff: { snapshot: string[]; current: string[] } | null;
  onCloseSnapshotDiff: () => void;
}

export function TerminalOverlays({
  showShortcutsOverlay,
  onCloseShortcuts,
  showCommandPalette,
  onCloseCommandPalette,
  tabs,
  assignments,
  sessionStates,
  focusedZone,
  onFocusZone,
  onApproveTab,
  onRejectTab,
  onRestartZone,
  onTogglePin,
  pinnedZones,
  onApproveAll,
  onSortZones,
  onExport,
  onToggleFocusMode,
  focusMode,
  onToggleAutoFocus,
  autoFocus,
  onToggleSound,
  soundEnabled,
  zoneLabels,
  onSetZoneLabel,
  zoneCount,
  onCompareZones,
  onSnapshotZone,
  onCompareSnapshot,
  snapshotZones,
  diffZones,
  lastOutputLines,
  onCloseDiff,
  snapshotDiff,
  onCloseSnapshotDiff,
}: TerminalOverlaysProps) {
  return (
    <>
      {showShortcutsOverlay && <KeyboardShortcutsOverlay onClose={onCloseShortcuts} />}

      {diffZones &&
        (() => {
          const [z1, z2] = diffZones;
          const tab1 = tabs.find((t) => t.id === assignments[z1]);
          const tab2 = tabs.find((t) => t.id === assignments[z2]);
          return (
            <ZoneDiffOverlay
              leftLabel={`Zone ${z1 + 1}: ${tab1?.title ?? "empty"}`}
              rightLabel={`Zone ${z2 + 1}: ${tab2?.title ?? "empty"}`}
              leftLines={tab1 ? (lastOutputLines[tab1.id] ?? []) : []}
              rightLines={tab2 ? (lastOutputLines[tab2.id] ?? []) : []}
              onClose={onCloseDiff}
            />
          );
        })()}

      {snapshotDiff && (
        <ZoneDiffOverlay
          leftLabel="Snapshot"
          rightLabel="Current"
          leftLines={snapshotDiff.snapshot}
          rightLines={snapshotDiff.current}
          onClose={onCloseSnapshotDiff}
        />
      )}

      {showCommandPalette && (
        <CommandPalette
          onClose={onCloseCommandPalette}
          tabs={tabs}
          assignments={assignments}
          sessionStates={sessionStates}
          focusedZone={focusedZone}
          onFocusZone={onFocusZone}
          onApproveTab={onApproveTab}
          onRejectTab={onRejectTab}
          onRestartZone={onRestartZone}
          onTogglePin={onTogglePin}
          pinnedZones={pinnedZones}
          onApproveAll={onApproveAll}
          onSortZones={onSortZones}
          onExport={onExport}
          onToggleFocusMode={onToggleFocusMode}
          focusMode={focusMode}
          onToggleAutoFocus={onToggleAutoFocus}
          autoFocus={autoFocus}
          onToggleSound={onToggleSound}
          soundEnabled={soundEnabled}
          zoneLabels={zoneLabels}
          onSetZoneLabel={onSetZoneLabel}
          zoneCount={zoneCount}
          onCompareZones={onCompareZones}
          onSnapshotZone={onSnapshotZone}
          onCompareSnapshot={onCompareSnapshot}
          snapshotZones={snapshotZones}
        />
      )}
    </>
  );
}
