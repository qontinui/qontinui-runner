/**
 * Spec Workflow Builder Types
 *
 * Re-exports spec types from @qontinui/ui-bridge/specs.
 * Defines local types for state machine data used in navigation workflows.
 */

export type {
  SpecCategory,
  SpecSeverity,
  SpecSource,
  SpecTarget,
  SpecAssertion,
  SpecGroup,
  SpecConfig,
  SpecMetadata,
  AssertionType,
  SearchCriteria,
} from "@qontinui/ui-bridge/specs";

export { migrateFromTestGeneratorOutput, validateSpecConfig } from "@qontinui/ui-bridge/specs";

export type { LegacyTestGeneratorOutput } from "@qontinui/ui-bridge/specs";

// Local types for state machine data (carried in SpecConfig.metadata)

export interface NonVisualState {
  id: string;
  name: string;
  description: string;
  elementIds: string[];
  pageUrl?: string;
  pageTitle?: string;
  confidence: number;
}

export interface NonVisualTransition {
  id: string;
  triggerElementId: string;
  triggerLabel?: string;
  triggerAction: "click" | "type" | "hover" | "scroll" | "custom";
  fromStateId: string;
  toStateId: string;
  confidence: number;
}

// Generator metadata stored in SpecConfig.metadata
export interface GeneratorSpecMetadata {
  generatorType?: "snapshot" | "navigation";
  projectId?: string;
  states?: NonVisualState[];
  transitions?: NonVisualTransition[];
  [key: string]: unknown;
}
