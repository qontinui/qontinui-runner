/**
 * UI Bridge Components
 *
 * Components for inspecting and interacting with UI Bridge elements,
 * states, and transitions in web applications.
 */

export { UIBridgeInspectorPanel } from "./UIBridgeInspectorPanel";
export type {
  UIBridgeElement,
  UIBridgeState,
  UIBridgeTransition,
  UIBridgeEvent,
  UIBridgeSnapshot,
} from "./inspector-types";

export { ConnectedUIBridgeInspector } from "./ConnectedUIBridgeInspector";
export { ConnectionPanel } from "./ConnectionPanel";
export type { ActiveSource } from "./ConnectionPanel";
export { RawApiPanel } from "./RawApiPanel";
export { SearchComparisonPanel } from "./SearchComparisonPanel";
export { ElementDescriptionPanel } from "./ElementDescriptionPanel";
export { NaturalLanguagePanel } from "./NaturalLanguagePanel";
export { ElementTreeView } from "./ElementTreeView";
export { EventTimelineView } from "./EventTimelineView";
export { ActionExecutorView } from "./ActionExecutorView";
export { LazyThumbnail } from "./LazyThumbnail";
export { SdkAppConnector } from "./SdkAppConnector";
export type { SdkAppConnectorProps, SdkAppConnection } from "./SdkAppConnector";
export { ElementInteractionHeatmap } from "./ElementInteractionHeatmap";
export { SelectorDecayChart } from "./SelectorDecayChart";
export { ActionBaselineTable } from "./ActionBaselineTable";
export { FailureTaxonomyPanel } from "./FailureTaxonomyPanel";
export { AutomationRegressionTable } from "./AutomationRegressionTable";
export { StallFrequencyChart } from "./StallFrequencyChart";
export { InterventionEffectivenessChart } from "./InterventionEffectivenessChart";
export { FailureChainViewer } from "./FailureChainViewer";
export { AnnotationGapTable } from "./AnnotationGapTable";
export { HealthScoreCard } from "./HealthScoreCard";
export { RecommendationsPanel } from "./RecommendationsPanel";
export { AutomationHealthDashboard } from "./AutomationHealthDashboard";
