/**
 * Spec Viewer/Editor types
 */

// Re-export UI Bridge spec types for convenience
export type {
  SpecConfig,
  SpecGroup,
  SpecAssertion,
  SetupAction,
  SpecCategory,
  SpecSeverity,
  SpecSource,
  SpecTarget,
  SpecMetadata,
  ArchitectureConfig,
  TechStackEntry,
  DirectoryEntry,
  ArchitecturePattern,
  ArchitectureConstraint,
  FeatureSpec,
  FeatureDependency,
  ApiConfig,
  ApiEndpoint,
  DataModel,
  SchemaField,
  HttpMethod,
  DataConfig,
  DataEntity,
  DataColumn,
  DataRelation,
  DataSeed,
  DataMigration,
  DataIndex,
  DependencyConfig,
  DependencyLink,
  ArtifactRef,
  ModuleRef,
  DependencyCluster,
  ConstraintConfig,
  PerformanceBudget,
  BundleBudget,
  BrowserTarget,
  ResponsiveBreakpoint,
  AccessibilityTarget,
  CapacityConstraint,
} from "ui-bridge";

// The kind of spec loaded
export type SpecKind =
  | "page-spec"
  | "style-guide"
  | "architecture"
  | "api"
  | "data"
  | "dependency"
  | "constraint";

// Spec tree node for the sidebar
export interface SpecTreeNode {
  id: string;
  label: string;
  type: SpecKind | "group" | "assertion";
  /** Parent spec ID (for groups/assertions) */
  specId?: string;
  /** Parent group ID (for assertions) */
  groupId?: string;
  /** Number of child items */
  childCount?: number;
  /** App name this spec belongs to */
  appName?: string;
  /** Page URL */
  pageUrl?: string;
  children?: SpecTreeNode[];
}

// Connection state for UI Bridge discovery
export interface ConnectionState {
  status: "disconnected" | "connecting" | "connected" | "error";
  url: string;
  appName?: string;
  error?: string;
}

// Selection state — what's currently selected in the tree
export type SpecSelection =
  | { type: "none" }
  | { type: "spec"; specId: string }
  | { type: "group"; specId: string; groupId: string }
  | { type: "assertion"; specId: string; groupId: string; assertionId: string };

// Loaded spec — supports all spec kinds
export type LoadedSpec =
  | {
      specId: string;
      appName: string;
      kind: "page-spec";
      config: import("ui-bridge").SpecConfig;
      source: "bundled" | "discovered" | "file";
    }
  | {
      specId: string;
      appName: string;
      kind: "architecture";
      config: import("ui-bridge").ArchitectureConfig;
      source: "bundled" | "discovered" | "file";
    }
  | {
      specId: string;
      appName: string;
      kind: "api";
      config: import("ui-bridge").ApiConfig;
      source: "bundled" | "discovered" | "file";
    }
  | {
      specId: string;
      appName: string;
      kind: "data";
      config: import("ui-bridge").DataConfig;
      source: "bundled" | "discovered" | "file";
    }
  | {
      specId: string;
      appName: string;
      kind: "dependency";
      config: import("ui-bridge").DependencyConfig;
      source: "bundled" | "discovered" | "file";
    }
  | {
      specId: string;
      appName: string;
      kind: "constraint";
      config: import("ui-bridge").ConstraintConfig;
      source: "bundled" | "discovered" | "file";
    };
