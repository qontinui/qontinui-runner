import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import {
  Layers,
  BarChart3,
  GitBranch,
  RefreshCw,
  Download,
  Copy,
  Check,
  Share2,
  GitCompare,
  Network,
  Fingerprint,
  CheckCircle2,
} from "lucide-react";
import type { CooccurrenceExport } from "../../types/ui-bridge-types";
import type { FingerprintDiscoveryResult } from "./discovery-types";

type ViewMode = "states" | "graph" | "transitions" | "matrix" | "stats";

interface StateDiscoveryToolbarProps {
  viewMode: ViewMode;
  onViewModeChange: (mode: ViewMode) => void;
  cooccurrenceData: CooccurrenceExport | null;
  discoveryResult?: FingerprintDiscoveryResult | null;
  isLoading: boolean;
  sessionActive: boolean;
  copied: boolean;
  showDiscoveredStates: boolean;
  compareMode: boolean;
  selectedStatesCount: number;
  isRunningDiscovery?: boolean;
  hasRunDiscovery: boolean;
  onRunDiscovery: () => void;
  onToggleDiscoveredStates: () => void;
  onToggleCompareMode: () => void;
  onExitCompareMode: () => void;
  onOpenComparison: () => void;
  onRefresh: () => Promise<CooccurrenceExport | null>;
  onCopyData: () => void;
  onExportData: () => void;
  onOpenExportDialog: () => void;
}

export function StateDiscoveryToolbar({
  viewMode,
  onViewModeChange,
  cooccurrenceData,
  discoveryResult,
  isLoading,
  sessionActive,
  copied,
  showDiscoveredStates,
  compareMode,
  selectedStatesCount,
  isRunningDiscovery,
  hasRunDiscovery,
  onRunDiscovery,
  onToggleDiscoveredStates,
  onToggleCompareMode,
  onExitCompareMode,
  onOpenComparison,
  onRefresh,
  onCopyData,
  onExportData,
  onOpenExportDialog,
}: StateDiscoveryToolbarProps) {
  const viewModeButton = (mode: ViewMode, icon: React.ReactNode, label: string, count?: number) => (
    <button
      onClick={() => onViewModeChange(mode)}
      className={`flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
        viewMode === mode
          ? "bg-primary text-primary-foreground"
          : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
      }`}
      disabled={mode === "graph" && (!discoveryResult || discoveryResult.states.length === 0)}
      title={
        mode === "graph" && (!discoveryResult || discoveryResult.states.length === 0)
          ? "Run discovery to see state graph"
          : undefined
      }
    >
      {icon}
      {label}
      {count !== undefined && (
        <Badge variant={viewMode === mode ? "default" : "muted"} size="sm">
          {count}
        </Badge>
      )}
    </button>
  );

  return (
    <div className="flex items-center justify-between mb-3">
      <div className="flex gap-1 p-1 bg-muted/30 rounded-lg">
        {viewModeButton(
          "states",
          <Layers className="w-3.5 h-3.5" />,
          "States",
          cooccurrenceData?.stateCandidates.length,
        )}
        {viewModeButton("graph", <Share2 className="w-3.5 h-3.5" />, "Graph")}
        {viewModeButton(
          "transitions",
          <GitBranch className="w-3.5 h-3.5" />,
          "Transitions",
          cooccurrenceData?.transitions.length,
        )}
        {viewModeButton("matrix", <Network className="w-3.5 h-3.5" />, "Matrix")}
        {viewModeButton("stats", <BarChart3 className="w-3.5 h-3.5" />, "Stats")}
      </div>

      <div className="flex items-center gap-2">
        {hasRunDiscovery && (
          <Button
            variant="primary"
            size="sm"
            onClick={onRunDiscovery}
            disabled={
              isRunningDiscovery || !cooccurrenceData || cooccurrenceData.presenceMatrix.length < 2
            }
            title={
              !cooccurrenceData
                ? "No data to analyze"
                : cooccurrenceData.presenceMatrix.length < 2
                  ? "Need at least 2 captures for discovery"
                  : "Run state discovery analysis"
            }
          >
            <Fingerprint
              className={`w-4 h-4 mr-1.5 ${isRunningDiscovery ? "animate-pulse" : ""}`}
            />
            {isRunningDiscovery ? "Running..." : "Run Discovery"}
          </Button>
        )}

        {discoveryResult && discoveryResult.states.length > 0 && viewMode === "states" && (
          <Button
            variant={showDiscoveredStates ? "primary" : "outline"}
            size="sm"
            onClick={onToggleDiscoveredStates}
            title={showDiscoveredStates ? "Show raw state candidates" : "Show discovered states"}
          >
            {showDiscoveredStates ? (
              <>
                <CheckCircle2 className="w-4 h-4 mr-1.5" />
                Discovered ({discoveryResult.states.length})
              </>
            ) : (
              <>
                <Layers className="w-4 h-4 mr-1.5" />
                Raw
              </>
            )}
          </Button>
        )}

        {discoveryResult &&
          discoveryResult.states.length >= 2 &&
          viewMode === "states" &&
          showDiscoveredStates && (
            <Button
              variant={compareMode ? "primary" : "outline"}
              size="sm"
              onClick={() => (compareMode ? onExitCompareMode() : onToggleCompareMode())}
              title={compareMode ? "Exit compare mode" : "Compare two states side-by-side"}
            >
              <GitCompare className="w-4 h-4 mr-1.5" />
              {compareMode ? "Exit Compare" : "Compare"}
            </Button>
          )}

        {compareMode && selectedStatesCount === 2 && (
          <Button variant="success" size="sm" onClick={onOpenComparison}>
            Compare Selected ({selectedStatesCount})
          </Button>
        )}

        {compareMode && selectedStatesCount < 2 && (
          <span className="text-xs text-muted-foreground px-2">
            Select {2 - selectedStatesCount} more state{selectedStatesCount === 1 ? "" : "s"}
          </span>
        )}

        <Button
          variant="outline"
          size="sm"
          onClick={onRefresh}
          disabled={isLoading || !sessionActive}
          title="Refresh co-occurrence data"
        >
          <RefreshCw className={`w-4 h-4 ${isLoading ? "animate-spin" : ""}`} />
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onCopyData}
          disabled={!cooccurrenceData}
          title="Copy data to clipboard"
        >
          {copied ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onExportData}
          disabled={!cooccurrenceData}
          title="Download as JSON"
        >
          <Download className="w-4 h-4" />
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onOpenExportDialog}
          disabled={!cooccurrenceData && !discoveryResult}
          title="Export to Automation Config"
        >
          <Share2 className="w-4 h-4 mr-1.5" />
          Export Config
        </Button>
      </div>
    </div>
  );
}
