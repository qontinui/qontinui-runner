/**
 * Synonym/Alias Dictionary for Natural Language Panel
 *
 * Provides mappings between common action verbs and element concepts
 * to improve natural language understanding and element matching.
 */

/**
 * ACTION_SYNONYMS - Map canonical action verbs to their alternatives
 *
 * The key is the canonical action, and the value is an array of synonyms
 * that should be recognized as equivalent to that action.
 */
export const ACTION_SYNONYMS: Record<string, string[]> = {
  click: ["tap", "press", "hit", "select", "activate", "push"],
  type: ["enter", "input", "fill", "write", "put"],
  clear: ["erase", "delete", "remove", "empty"],
  focus: ["select", "highlight", "activate"],
  check: ["tick", "enable", "turn on"],
  uncheck: ["untick", "disable", "turn off"],
  select: ["choose", "pick", "set"],
  hover: ["mouseover", "point to"],
};

/**
 * Reverse mapping from synonym to canonical action
 * Built from ACTION_SYNONYMS for efficient lookup
 */
const SYNONYM_TO_ACTION: Record<string, string> = {};
for (const [canonical, synonyms] of Object.entries(ACTION_SYNONYMS)) {
  // Map the canonical action to itself
  SYNONYM_TO_ACTION[canonical] = canonical;
  // Map each synonym to the canonical action
  for (const synonym of synonyms) {
    SYNONYM_TO_ACTION[synonym.toLowerCase()] = canonical;
  }
}

/**
 * ELEMENT_SYNONYMS - Map element concepts to their alternatives
 *
 * The key is the canonical concept, and the value is an array of synonyms
 * that should be recognized as equivalent to that concept.
 */
export const ELEMENT_SYNONYMS: Record<string, string[]> = {
  // Action concepts
  login: ["sign in", "log in", "authenticate"],
  submit: ["confirm", "send", "go", "ok", "done", "save"],
  cancel: ["close", "dismiss", "back", "exit"],

  // Field concepts
  username: ["user", "login", "user id", "account"],
  password: ["pass", "secret", "credentials"],
  email: ["mail", "e-mail"],
  search: ["find", "lookup", "query"],

  // UI element types
  menu: ["navigation", "nav"],
  button: ["btn", "action"],
  input: ["field", "textbox", "text field"],
  dropdown: ["select", "combobox", "picker"],
};

/**
 * Reverse mapping from synonym to all matching canonical concepts
 * An element synonym might map to multiple concepts (e.g., "select" could be dropdown or action)
 */
const SYNONYM_TO_ELEMENTS: Record<string, string[]> = {};
for (const [canonical, synonyms] of Object.entries(ELEMENT_SYNONYMS)) {
  // Map the canonical concept to itself
  if (!SYNONYM_TO_ELEMENTS[canonical]) {
    SYNONYM_TO_ELEMENTS[canonical] = [];
  }
  SYNONYM_TO_ELEMENTS[canonical].push(canonical);

  // Map each synonym to the canonical concept
  for (const synonym of synonyms) {
    const lowerSynonym = synonym.toLowerCase();
    if (!SYNONYM_TO_ELEMENTS[lowerSynonym]) {
      SYNONYM_TO_ELEMENTS[lowerSynonym] = [];
    }
    SYNONYM_TO_ELEMENTS[lowerSynonym].push(canonical);
  }
}

/**
 * Normalize an action verb to its canonical form
 *
 * @param text - The action text to normalize (e.g., "tap", "press", "hit")
 * @returns The canonical action (e.g., "click") or the original text if no match
 *
 * @example
 * normalizeAction("tap") // returns "click"
 * normalizeAction("write") // returns "type"
 * normalizeAction("unknown") // returns "unknown"
 */
export function normalizeAction(text: string): string {
  const lower = text.toLowerCase().trim();
  return SYNONYM_TO_ACTION[lower] || lower;
}

/**
 * Find all canonical element concepts that match a given text
 *
 * @param text - The element description text to look up
 * @returns Array of matching canonical element concepts, or empty array if no match
 *
 * @example
 * normalizeElement("sign in") // returns ["login"]
 * normalizeElement("btn") // returns ["button"]
 * normalizeElement("textbox") // returns ["input"]
 */
export function normalizeElement(text: string): string[] {
  const lower = text.toLowerCase().trim();

  // Direct lookup
  if (SYNONYM_TO_ELEMENTS[lower]) {
    return SYNONYM_TO_ELEMENTS[lower];
  }

  // Check for multi-word matches (e.g., "sign in" in "click the sign in button")
  const matches: string[] = [];
  for (const [synonym, concepts] of Object.entries(SYNONYM_TO_ELEMENTS)) {
    if (lower.includes(synonym) || synonym.includes(lower)) {
      for (const concept of concepts) {
        if (!matches.includes(concept)) {
          matches.push(concept);
        }
      }
    }
  }

  return matches;
}

/**
 * Get all synonyms for a canonical element concept
 *
 * @param canonical - The canonical element concept
 * @returns Array of all synonyms including the canonical form
 */
export function getElementSynonyms(canonical: string): string[] {
  const lower = canonical.toLowerCase();
  const synonyms = ELEMENT_SYNONYMS[lower];
  if (synonyms) {
    return [lower, ...synonyms];
  }
  return [lower];
}

/**
 * Get all synonyms for a canonical action
 *
 * @param canonical - The canonical action verb
 * @returns Array of all synonyms including the canonical form
 */
export function getActionSynonyms(canonical: string): string[] {
  const lower = canonical.toLowerCase();
  const synonyms = ACTION_SYNONYMS[lower];
  if (synonyms) {
    return [lower, ...synonyms];
  }
  return [lower];
}

/**
 * Expand a query with all synonym variations
 *
 * Takes a natural language query and returns an array of variations
 * where synonyms have been substituted.
 *
 * @param query - The original query text
 * @returns Array of query variations including the original
 *
 * @example
 * expandQuery("tap the sign in button")
 * // returns ["tap the sign in button", "tap the login button", ...]
 */
export function expandQuery(query: string): string[] {
  const lower = query.toLowerCase().trim();
  const variations: Set<string> = new Set([lower]);

  // Find and expand element synonyms in the query
  for (const [canonical, synonyms] of Object.entries(ELEMENT_SYNONYMS)) {
    const allForms = [canonical, ...synonyms];

    for (const form of allForms) {
      if (lower.includes(form)) {
        // Generate variations with other synonyms
        for (const otherForm of allForms) {
          if (otherForm !== form) {
            const variation = lower.replace(form, otherForm);
            variations.add(variation);
          }
        }
      }
    }
  }

  return Array.from(variations);
}

/**
 * Check if a text contains any synonym of a given canonical concept
 *
 * @param text - The text to search in
 * @param canonical - The canonical concept to look for synonyms of
 * @returns True if any synonym (including canonical) is found in text
 */
export function containsElementSynonym(text: string, canonical: string): boolean {
  const lower = text.toLowerCase();
  const allForms = getElementSynonyms(canonical);
  return allForms.some((form) => lower.includes(form));
}

/**
 * Find all element synonyms present in a text
 *
 * @param text - The text to search
 * @returns Array of [canonical, matchedForm] tuples for each match found
 */
export function findElementSynonymsInText(text: string): Array<[string, string]> {
  const lower = text.toLowerCase();
  const found: Array<[string, string]> = [];

  for (const [canonical, synonyms] of Object.entries(ELEMENT_SYNONYMS)) {
    const allForms = [canonical, ...synonyms];
    for (const form of allForms) {
      if (lower.includes(form)) {
        found.push([canonical, form]);
        break; // Only count each canonical once
      }
    }
  }

  return found;
}
