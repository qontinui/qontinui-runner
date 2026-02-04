/**
 * Test Generator Types (Runner-local)
 *
 * Types matching the legacy TestGeneratorOutput format for use in the runner.
 * Also re-exports canonical spec types from @qontinui/ui-bridge/specs.
 *
 * For migration, use migrateFromTestGeneratorOutput() from @qontinui/ui-bridge/specs.
 */

// Re-export canonical types for consumers ready to migrate
export type {
  SpecCategory,
  SpecSeverity,
  SpecSource,
  SpecTarget,
  SpecAssertion,
  SpecGroup,
  SpecConfig,
} from "@qontinui/ui-bridge/specs";

export { migrateFromTestGeneratorOutput } from "@qontinui/ui-bridge/specs";

// Legacy types (backward-compatible with TestGeneratorOutput JSON)

export type TestCategory =
  | "element-presence"
  | "accessibility"
  | "form-validation"
  | "state-consistency"
  | "modal-dialog"
  | "navigation"
  | "cross-page-consistency"
  | "custom";

export type TestSeverity = "critical" | "warning" | "info";

export type TestTarget =
  | { type: "elementId"; elementId: string; label?: string }
  | { type: "formId"; formId: string; label?: string }
  | { type: "modalId"; modalId: string; label?: string };

export interface TestAssertion {
  id: string;
  description: string;
  category: TestCategory;
  severity: TestSeverity;
  target: TestTarget;
  assertionType: string;
  expected?: unknown;
  attributeName?: string;
  source: "auto" | "manual";
  reviewed: boolean;
  enabled: boolean;
  notes?: string;
}

export interface TestSpecification {
  id: string;
  name: string;
  description: string;
  category: TestCategory;
  assertions: TestAssertion[];
  stateId: string;
  transitionId?: string;
  source: "auto" | "manual";
  createdAt: string;
  updatedAt: string;
}

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

export interface TestGeneratorOutput {
  version: "1.0.0";
  projectId: string;
  generatorType: "snapshot" | "navigation";
  states: NonVisualState[];
  transitions: NonVisualTransition[];
  testSpecifications: TestSpecification[];
  snapshotMetadata?: {
    snapshotId: string;
    pageUrl: string;
    pageTitle: string;
    capturedAt: string;
    elementCount: number;
    formCount: number;
    modalCount: number;
  };
  explorationMetadata?: {
    explorationId: string;
    targetUrl: string;
    statesDiscovered: number;
    transitionsDiscovered: number;
    exploredAt: string;
  };
  createdAt: string;
  updatedAt: string;
}
