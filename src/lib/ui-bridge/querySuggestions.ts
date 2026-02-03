/**
 * Query Suggestions for Natural Language Panel
 *
 * Generates context-aware query suggestions based on available page elements.
 * Prioritizes interactive elements and primary actions for best UX.
 */

import type { ExternalElement } from "../../hooks/useExternalUIBridge";

/**
 * A suggested query for the natural language panel
 */
export interface QuerySuggestion {
  /** The suggested query text (e.g., "Click the 'Submit' button") */
  text: string;
  /** The action verb (click, type, check, etc.) */
  action: string;
  /** Target element ID */
  elementId: string;
  /** How good this suggestion is (0-1, higher is better) */
  confidence: number;
  /** Why this is suggested (e.g., "Primary action button") */
  reason: string;
}

/**
 * Configuration for suggestion generation
 */
export interface SuggestionConfig {
  /** Maximum number of suggestions to return (default: 6) */
  maxSuggestions?: number;
  /** Minimum confidence threshold (default: 0.3) */
  minConfidence?: number;
}

/**
 * Priority keywords for identifying primary action buttons
 * Higher index = higher priority
 */
const PRIMARY_ACTION_KEYWORDS = [
  // Lowest priority
  "next",
  "continue",
  "proceed",
  // Medium priority
  "save",
  "apply",
  "update",
  "add",
  "create",
  "delete",
  "remove",
  // Higher priority
  "ok",
  "confirm",
  "accept",
  "agree",
  // Highest priority
  "submit",
  "sign in",
  "login",
  "log in",
  "sign up",
  "register",
  "send",
  "checkout",
  "buy",
  "purchase",
  "pay",
];

/**
 * Keywords for cancel/dismiss actions (lower priority suggestions)
 */
const CANCEL_KEYWORDS = ["cancel", "close", "dismiss", "back", "exit", "no", "decline"];

/**
 * Keywords for required/important input fields
 */
const IMPORTANT_FIELD_KEYWORDS = [
  "email",
  "password",
  "username",
  "name",
  "phone",
  "address",
  "search",
];

/**
 * Get the best display text for an element
 */
function getElementDisplayText(element: ExternalElement): string | null {
  // Prefer label, then text, then accessible name
  if (element.label && element.label.length <= 40) {
    return element.label;
  }
  if (element.text && element.text.length <= 40) {
    return element.text;
  }
  if (element.accessibleName && element.accessibleName.length <= 40) {
    return element.accessibleName;
  }
  if (element.placeholder && element.placeholder.length <= 40) {
    return element.placeholder;
  }
  // For inputs, use name attribute if available
  if (element.name && element.name.length <= 40) {
    // Convert camelCase or snake_case to readable text
    return element.name
      .replace(/([A-Z])/g, " $1")
      .replace(/_/g, " ")
      .trim()
      .toLowerCase();
  }
  return null;
}

/**
 * Check if text matches any keywords from a list (case-insensitive)
 */
function matchesKeywords(text: string | null | undefined, keywords: string[]): boolean {
  if (!text) return false;
  const lower = text.toLowerCase();
  return keywords.some((kw) => lower.includes(kw));
}

/**
 * Get priority score for primary action keywords (higher = more important)
 */
function getPrimaryActionScore(text: string | null | undefined): number {
  if (!text) return 0;
  const lower = text.toLowerCase();

  // Find the highest priority keyword that matches
  for (let i = PRIMARY_ACTION_KEYWORDS.length - 1; i >= 0; i--) {
    if (lower.includes(PRIMARY_ACTION_KEYWORDS[i])) {
      // Normalize to 0-1 range, with some bonus for exact matches
      const baseScore = (i + 1) / PRIMARY_ACTION_KEYWORDS.length;
      const exactMatch = lower === PRIMARY_ACTION_KEYWORDS[i];
      return exactMatch ? Math.min(baseScore + 0.1, 1) : baseScore;
    }
  }
  return 0;
}

/**
 * Calculate base confidence for an element based on its properties
 */
function calculateElementConfidence(element: ExternalElement): number {
  let confidence = 0.5; // Base confidence

  // Boost for visible elements
  if (element.visible) {
    confidence += 0.1;
  }

  // Boost for enabled elements
  if (element.enabled) {
    confidence += 0.1;
  }

  // Boost for elements with clear labels
  if (element.label || element.accessibleName) {
    confidence += 0.15;
  }

  // Boost for interactive elements
  if (element.is_interactive) {
    confidence += 0.1;
  }

  // Small boost for elements with test IDs or ui-ids
  if (element.hasUiId || element.dataAttributes?.["data-testid"]) {
    confidence += 0.05;
  }

  return Math.min(confidence, 1);
}

/**
 * Generate a suggestion for a button element
 */
function generateButtonSuggestion(element: ExternalElement): QuerySuggestion | null {
  const displayText = getElementDisplayText(element);
  if (!displayText) return null;

  // Skip disabled or hidden buttons
  if (!element.enabled || !element.visible) return null;

  let confidence = calculateElementConfidence(element);
  let reason = "Button";

  // Check for primary action keywords
  const primaryScore = getPrimaryActionScore(displayText);
  if (primaryScore > 0) {
    confidence = Math.min(confidence + primaryScore * 0.3, 1);
    reason = "Primary action button";
  }

  // Lower priority for cancel-type buttons
  if (matchesKeywords(displayText, CANCEL_KEYWORDS)) {
    confidence = Math.max(confidence - 0.2, 0.3);
    reason = "Dismiss action";
  }

  return {
    text: `Click the "${displayText}" button`,
    action: "click",
    elementId: element.id,
    confidence,
    reason,
  };
}

/**
 * Generate a suggestion for an input field
 */
function generateInputSuggestion(element: ExternalElement): QuerySuggestion | null {
  const displayText = getElementDisplayText(element);

  // Skip disabled or hidden inputs
  if (!element.enabled || !element.visible) return null;

  // Skip inputs that already have values (unless they're search fields)
  const isSearch =
    element.inputType === "search" || matchesKeywords(displayText, ["search", "find", "query"]);
  if (element.value && element.value.length > 0 && !isSearch) {
    return null;
  }

  let confidence = calculateElementConfidence(element);
  let reason = "Input field";

  // Handle different input types
  const inputType = element.inputType || "text";

  if (inputType === "password") {
    confidence += 0.1;
    reason = "Password field";
    const fieldName = displayText || "password";
    return {
      text: `Type in the "${fieldName}" field`,
      action: "type",
      elementId: element.id,
      confidence,
      reason,
    };
  }

  if (inputType === "email") {
    confidence += 0.1;
    reason = "Email field";
    const fieldName = displayText || "email";
    return {
      text: `Type in the "${fieldName}" field`,
      action: "type",
      elementId: element.id,
      confidence,
      reason,
    };
  }

  if (inputType === "search" || isSearch) {
    confidence += 0.05;
    reason = "Search field";
    const fieldName = displayText || "search";
    return {
      text: `Type in the "${fieldName}" field`,
      action: "type",
      elementId: element.id,
      confidence,
      reason,
    };
  }

  // Check for important field keywords
  if (matchesKeywords(displayText, IMPORTANT_FIELD_KEYWORDS)) {
    confidence += 0.1;
    reason = "Required field";
  }

  // Check for required attribute
  if (element.is_required) {
    confidence += 0.15;
    reason = "Required field";
  }

  if (!displayText) return null;

  return {
    text: `Type in the "${displayText}" field`,
    action: "type",
    elementId: element.id,
    confidence,
    reason,
  };
}

/**
 * Generate a suggestion for a checkbox element
 */
function generateCheckboxSuggestion(element: ExternalElement): QuerySuggestion | null {
  const displayText = getElementDisplayText(element);
  if (!displayText) return null;

  // Skip disabled or hidden checkboxes
  if (!element.enabled || !element.visible) return null;

  const confidence = calculateElementConfidence(element);
  const isChecked = element.checked === true;

  if (isChecked) {
    return {
      text: `Uncheck the "${displayText}" checkbox`,
      action: "uncheck",
      elementId: element.id,
      confidence: confidence - 0.1, // Lower priority since already checked
      reason: "Checkbox (currently checked)",
    };
  }

  return {
    text: `Check the "${displayText}" checkbox`,
    action: "check",
    elementId: element.id,
    confidence,
    reason: "Checkbox",
  };
}

/**
 * Generate a suggestion for a link element
 */
function generateLinkSuggestion(element: ExternalElement): QuerySuggestion | null {
  const displayText = getElementDisplayText(element);
  if (!displayText) return null;

  // Skip disabled or hidden links
  if (!element.enabled || !element.visible) return null;

  // Skip very short link texts (likely icons)
  if (displayText.length < 3) return null;

  let confidence = calculateElementConfidence(element) - 0.1; // Links slightly lower priority
  let reason = "Link";

  // Boost for primary action keywords in links
  const primaryScore = getPrimaryActionScore(displayText);
  if (primaryScore > 0) {
    confidence = Math.min(confidence + primaryScore * 0.2, 1);
    reason = "Action link";
  }

  return {
    text: `Click the "${displayText}" link`,
    action: "click",
    elementId: element.id,
    confidence,
    reason,
  };
}

/**
 * Generate a suggestion for a dropdown/select element
 */
function generateSelectSuggestion(element: ExternalElement): QuerySuggestion | null {
  const displayText = getElementDisplayText(element);

  // Skip disabled or hidden selects
  if (!element.enabled || !element.visible) return null;

  const confidence = calculateElementConfidence(element);
  const fieldName = displayText || "dropdown";

  return {
    text: `Select from the "${fieldName}" dropdown`,
    action: "focus",
    elementId: element.id,
    confidence,
    reason: "Dropdown menu",
  };
}

/**
 * Generate query suggestions based on available page elements
 *
 * @param elements - Array of ExternalElement from the page
 * @param config - Optional configuration for suggestion generation
 * @returns Array of QuerySuggestion sorted by confidence (highest first)
 */
export function generateQuerySuggestions(
  elements: ExternalElement[],
  config: SuggestionConfig = {},
): QuerySuggestion[] {
  const { maxSuggestions = 6, minConfidence = 0.3 } = config;

  if (!elements || elements.length === 0) {
    return [];
  }

  const suggestions: QuerySuggestion[] = [];
  const seenElementTypes = new Map<string, number>(); // Track element types to avoid too many similar suggestions

  for (const element of elements) {
    // Skip hidden or non-interactive elements
    if (!element.visible || element.accessibility?.ariaHidden) {
      continue;
    }

    let suggestion: QuerySuggestion | null = null;

    // Generate suggestion based on element type
    switch (element.type) {
      case "button":
        suggestion = generateButtonSuggestion(element);
        break;
      case "input":
        suggestion = generateInputSuggestion(element);
        break;
      case "checkbox":
        suggestion = generateCheckboxSuggestion(element);
        break;
      case "link":
        // Limit link suggestions to avoid flooding
        if ((seenElementTypes.get("link") || 0) < 2) {
          suggestion = generateLinkSuggestion(element);
        }
        break;
      case "select":
        suggestion = generateSelectSuggestion(element);
        break;
      default:
        // Handle elements with button role but different type
        if (element.role === "button" || element.accessibility?.role === "button") {
          suggestion = generateButtonSuggestion(element);
        }
        // Handle elements with textbox role
        else if (element.role === "textbox" || element.accessibility?.role === "textbox") {
          suggestion = generateInputSuggestion(element);
        }
        // Handle links by role
        else if (element.role === "link" || element.accessibility?.role === "link") {
          if ((seenElementTypes.get("link") || 0) < 2) {
            suggestion = generateLinkSuggestion(element);
          }
        }
        break;
    }

    if (suggestion && suggestion.confidence >= minConfidence) {
      suggestions.push(suggestion);
      const typeKey = element.type || element.role || "unknown";
      seenElementTypes.set(typeKey, (seenElementTypes.get(typeKey) || 0) + 1);
    }
  }

  // Sort by confidence (highest first) and limit
  return suggestions.sort((a, b) => b.confidence - a.confidence).slice(0, maxSuggestions);
}

/**
 * Check if suggestions should be regenerated based on element changes
 *
 * @param prevElements - Previous element list
 * @param newElements - New element list
 * @returns True if suggestions should be regenerated
 */
export function shouldRegenerateSuggestions(
  prevElements: ExternalElement[],
  newElements: ExternalElement[],
): boolean {
  // Regenerate if element count changed significantly
  if (Math.abs(prevElements.length - newElements.length) > 3) {
    return true;
  }

  // Regenerate if no previous elements
  if (prevElements.length === 0) {
    return true;
  }

  // Check if the first few interactive elements changed
  const getInteractiveIds = (elements: ExternalElement[]) =>
    elements
      .filter((el) => el.visible && el.enabled && el.is_interactive)
      .slice(0, 10)
      .map((el) => el.id)
      .sort()
      .join(",");

  return getInteractiveIds(prevElements) !== getInteractiveIds(newElements);
}
