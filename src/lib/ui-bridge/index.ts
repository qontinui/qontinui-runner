/**
 * UI Bridge Integration for Qontinui Runner
 *
 * This module provides Tauri-specific integrations for the ui-bridge package.
 */

export { TauriRenderLogStorage } from "./TauriRenderLogStorage";
export {
  useRenderLogManager,
  type UseRenderLogManagerOptions,
  type RenderLogManagerHandle,
} from "./useRenderLogManager";
export { RenderLogWrapper, type RenderLogWrapperProps } from "./RenderLogWrapper";
export { generateAIContext, type AIContextOptions } from "./generateAIContext";
export {
  ACTION_SYNONYMS,
  ELEMENT_SYNONYMS,
  normalizeAction,
  normalizeElement,
  getElementSynonyms,
  getActionSynonyms,
  expandQuery,
  containsElementSynonym,
  findElementSynonymsInText,
} from "./synonyms";
export {
  buildElementPath,
  findParent,
  findSiblings,
  findNearestLandmark,
  buildDOMContext,
  formatPathSegment,
  getLandmarkDisplayName,
  type PathSegment,
  type ParentInfo,
  type SiblingInfo,
  type LandmarkInfo,
  type DOMContext,
} from "./domContext";

// Selector generation utilities
export {
  generateCssSelector,
  generateXPath,
  generateTestIdSelector,
  generateAccessibleSelector,
  generatePlaywrightCode,
  generatePuppeteerCode,
  generateCypressCode,
  generateQontinuiCode,
  generateSeleniumCode,
  getAllSelectors,
  getBestSelector,
  type SelectorResult,
  type AutomationAction,
} from "./selectorGenerator";

// State discovery export utilities
export {
  exportToAutomationConfig,
  exportStateCandidatesToConfig,
  exportDiscoveredStatesToConfig,
  getAvailableStatesForExport,
  getDefaultExportOptions,
  type StateExportOptions,
  type AutomationConfig,
  type AutomationState,
  type AutomationTransition,
  type AutomationElement,
} from "./stateExporter";

// Query suggestions for natural language panel
export {
  generateQuerySuggestions,
  shouldRegenerateSuggestions,
  type QuerySuggestion,
  type SuggestionConfig,
} from "./querySuggestions";

// LLM-powered element matching
export {
  matchElementsWithLLM,
  parseIntentWithLLM,
  isLLMAvailable,
  clearLLMCache,
  type LLMMatchOptions,
  type LLMMatchResult,
  type LLMMatchResponse,
  type LLMIntentResponse,
  type ParsedIntent as LLMParsedIntent,
} from "./llmElementMatcher";
