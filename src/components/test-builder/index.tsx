/**
 * Test Builder Module
 *
 * Exports for the verification test builder components.
 */

export { TestBuilderTab } from "./TestBuilderTab";
export { TestBuilderProvider, useTestBuilder } from "./TestBuilderContext";
export { TestLibraryPanel } from "./TestLibraryPanel";
export { TestEditorPanel, getCodeFromTest, defaultTemplates } from "./TestEditorPanel";
export { TestPropertiesPanel } from "./TestPropertiesPanel";
export { TestExecutionPanel } from "./TestExecutionPanel";
export { ImportExportDialog } from "./ImportExportDialog";

// AI Test Generation components
export { PageAnalyzer } from "./PageAnalyzer";
export { ElementPicker } from "./ElementPicker";
export { AiTestGenerator } from "./AiTestGenerator";

// Re-export types
export type {
  TestType,
  TestCategory,
  TestStatus,
  VerificationTest,
  TestDefinition,
  TestExecutionResult,
  TestResult,
  CreateTestInput,
  CommandResponse,
  TestTypeInfo,
  TriggerPoint,
  TestAssociation,
  VisionConfig,
  VisionAssertion,
  RepoTestConfig,
  // AI Test Generation types
  BoundingBox,
  DetectedElement,
  PageAnalysis,
  VisualEvidence,
} from "./types";
