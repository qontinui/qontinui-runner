/**
 * Canvas Component Registry
 *
 * Maps component type strings to React components.
 * Unknown types render a fallback.
 */

import type { ComponentType } from "react";
import type { CanvasPanelComponentProps } from "./types";
import { MarkdownPanel } from "./MarkdownPanel";
import { TablePanel } from "./TablePanel";
import { CodeDiffPanel } from "./CodeDiffPanel";
import { KeyValuePanel } from "./KeyValuePanel";
import { AlertPanel } from "./AlertPanel";
import { TerminalPanel } from "./TerminalPanel";
import { TimelinePanel } from "./TimelinePanel";
import { FileTreePanel } from "./FileTreePanel";
import { ProgressChartPanel } from "./ProgressChartPanel";
import { FindingListPanel } from "./FindingListPanel";
import { ChecklistPanel } from "./ChecklistPanel";
import { ArchitectureGraphPanel } from "./ArchitectureGraphPanel";
import { SummaryStatsPanel } from "./SummaryStatsPanel";
import { StateTimelinePanel } from "./StateTimelinePanel";
import { WaterfallPanel } from "./WaterfallPanel";
import { SparklinePanel } from "./SparklinePanel";
import { WaffleChartPanel } from "./WaffleChartPanel";
import { PhaseTimelinePanel } from "./PhaseTimelinePanel";
import { IterationComparisonPanel } from "./IterationComparisonPanel";
import { StepDurationChartPanel } from "./StepDurationChartPanel";
import { PhaseDistributionPanel } from "./PhaseDistributionPanel";
import { DependencyGraphPanel } from "./DependencyGraphPanel";
import { CostBreakdownPanel } from "./CostBreakdownPanel";
import { MissionBriefPanel } from "./MissionBriefPanel";
import { AcceptanceCriteriaPanel } from "./AcceptanceCriteriaPanel";

export type { CanvasPanelComponentProps } from "./types";

/**
 * Fallback component for unknown panel types.
 */
function FallbackPanel({ data }: CanvasPanelComponentProps) {
  return (
    <div className="text-sm text-muted-foreground">
      <p className="mb-2 italic">Unknown component type</p>
      <pre className="text-xs bg-muted/50 p-2 rounded overflow-auto max-h-[200px]">
        {JSON.stringify(data, null, 2)}
      </pre>
    </div>
  );
}

/**
 * Registry mapping component type strings to React components.
 */
const COMPONENT_MAP: Record<string, ComponentType<CanvasPanelComponentProps>> = {
  Markdown: MarkdownPanel,
  Table: TablePanel,
  CodeDiff: CodeDiffPanel,
  KeyValue: KeyValuePanel,
  Alert: AlertPanel,
  Terminal: TerminalPanel,
  Timeline: TimelinePanel,
  FileTree: FileTreePanel,
  ProgressChart: ProgressChartPanel,
  FindingList: FindingListPanel,
  Checklist: ChecklistPanel,
  ArchitectureGraph: ArchitectureGraphPanel,
  SummaryStats: SummaryStatsPanel,
  StateTimeline: StateTimelinePanel,
  Waterfall: WaterfallPanel,
  Sparkline: SparklinePanel,
  WaffleChart: WaffleChartPanel,
  PhaseTimeline: PhaseTimelinePanel,
  IterationComparison: IterationComparisonPanel,
  StepDurationChart: StepDurationChartPanel,
  PhaseDistribution: PhaseDistributionPanel,
  DependencyGraph: DependencyGraphPanel,
  CostBreakdown: CostBreakdownPanel,
  MissionBrief: MissionBriefPanel,
  AcceptanceCriteria: AcceptanceCriteriaPanel,
};

/**
 * Get the React component for a canvas panel type.
 */
export const CanvasComponentRegistry = {
  get(componentType: string): ComponentType<CanvasPanelComponentProps> {
    return COMPONENT_MAP[componentType] ?? FallbackPanel;
  },

  has(componentType: string): boolean {
    return componentType in COMPONENT_MAP;
  },
};
