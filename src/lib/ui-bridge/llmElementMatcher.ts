/**
 * LLM-Powered Element Matching for Natural Language Panel
 *
 * Provides semantic element matching using AI when regex/synonym matching
 * has low confidence. Uses the runner's AI infrastructure for LLM calls.
 */

import type { ExternalElement } from "../../types/ui-bridge-types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// =============================================================================
// Types
// =============================================================================

export interface LLMMatchOptions {
  /** Timeout for LLM call in milliseconds (default: 30000) */
  timeout?: number;
  /** Maximum number of results to return (default: 5) */
  maxResults?: number;
  /** Minimum confidence threshold (0-1) to consider a match (default: 0.3) */
  minConfidence?: number;
  /** Include explanation in the result (default: true) */
  includeExplanation?: boolean;
}

export interface LLMMatchResult {
  /** Element ID that matches the query */
  elementId: string;
  /** Confidence score from 0-1 */
  confidence: number;
  /** Explanation of why this element matches */
  explanation?: string;
}

export interface LLMMatchResponse {
  /** Whether the LLM call succeeded */
  success: boolean;
  /** Matched elements sorted by confidence */
  matches: LLMMatchResult[];
  /** Error message if failed */
  error?: string;
  /** Time taken for the LLM call in ms */
  duration?: number;
}

export interface ParsedIntent {
  /** The action to perform (click, type, etc.) */
  action: string;
  /** Description of the target element */
  targetDescription: string;
  /** Parameters like text to type */
  params?: Record<string, unknown>;
  /** Confidence in the parsing */
  confidence: number;
}

export interface LLMIntentResponse {
  /** Whether the LLM call succeeded */
  success: boolean;
  /** Parsed intent */
  intent?: ParsedIntent;
  /** Error message if failed */
  error?: string;
  /** Time taken for the LLM call in ms */
  duration?: number;
}

// =============================================================================
// Cache
// =============================================================================

interface CacheEntry<T> {
  data: T;
  timestamp: number;
}

const CACHE_TTL_MS = 5 * 60 * 1000; // 5 minutes

// Simple Map-based cache with TTL
const matchCache = new Map<string, CacheEntry<LLMMatchResponse>>();
const intentCache = new Map<string, CacheEntry<LLMIntentResponse>>();

function getCacheKey(query: string, elementIds: string[]): string {
  // Create a deterministic cache key from query and element IDs
  const sortedIds = [...elementIds].sort().join(",");
  return `${query}|${sortedIds}`;
}

function getFromCache<T>(cache: Map<string, CacheEntry<T>>, key: string): T | null {
  const entry = cache.get(key);
  if (!entry) return null;

  const now = Date.now();
  if (now - entry.timestamp > CACHE_TTL_MS) {
    cache.delete(key);
    return null;
  }

  return entry.data;
}

function setInCache<T>(cache: Map<string, CacheEntry<T>>, key: string, data: T): void {
  // Clean up old entries periodically
  if (cache.size > 100) {
    const now = Date.now();
    for (const [k, v] of cache.entries()) {
      if (now - v.timestamp > CACHE_TTL_MS) {
        cache.delete(k);
      }
    }
  }

  cache.set(key, { data, timestamp: Date.now() });
}

/**
 * Clear the LLM matching caches
 */
export function clearLLMCache(): void {
  matchCache.clear();
  intentCache.clear();
}

// =============================================================================
// Element Context Formatting
// =============================================================================

interface ElementSummary {
  id: string;
  type: string;
  role?: string;
  label?: string;
  text?: string;
  placeholder?: string;
  value?: string;
  enabled: boolean;
  visible: boolean;
}

/**
 * Format elements into a concise representation for the LLM prompt
 */
function formatElementsForLLM(elements: ExternalElement[]): ElementSummary[] {
  return elements.map((el) => ({
    id: el.id,
    type: el.type,
    role: el.role || el.accessibility?.role,
    label: el.label,
    text: el.text?.slice(0, 100), // Truncate long text
    placeholder: el.placeholder,
    value: el.value?.slice(0, 50),
    enabled: el.enabled,
    visible: el.visible,
  }));
}

// =============================================================================
// LLM Prompts
// =============================================================================

function buildElementMatchPrompt(
  query: string,
  elements: ElementSummary[],
  maxResults: number,
): string {
  const elementsJson = JSON.stringify(elements, null, 2);

  return `You are a UI automation assistant. Given a user's natural language query and a list of UI elements, identify which element(s) best match what the user is looking for.

USER QUERY: "${query}"

AVAILABLE ELEMENTS:
${elementsJson}

INSTRUCTIONS:
1. Analyze the user's query to understand what element they're trying to interact with
2. Match against element properties: type, role, label, text, placeholder, id
3. Consider semantic meanings (e.g., "sign in" matches "login", "submit" matches "confirm")
4. Return the top ${maxResults} matches with confidence scores

RESPOND WITH ONLY VALID JSON in this exact format:
{
  "matches": [
    {
      "elementId": "exact-element-id-from-list",
      "confidence": 0.95,
      "explanation": "Brief reason why this matches"
    }
  ]
}

CONFIDENCE GUIDELINES:
- 0.9-1.0: Exact or very close match (label/text contains query words)
- 0.7-0.9: Strong semantic match (synonyms, related terms)
- 0.5-0.7: Partial match (some words match, similar context)
- 0.3-0.5: Weak match (might be what user wants)
- Below 0.3: Unlikely match

If no element matches well, return empty matches array: {"matches": []}`;
}

function buildIntentParsePrompt(query: string): string {
  return `You are a UI automation assistant. Parse the user's natural language command into a structured action.

USER COMMAND: "${query}"

SUPPORTED ACTIONS:
- click: Click on an element
- type: Enter text into a field (requires "text" parameter)
- clear: Clear a field's content
- focus: Focus on an element
- check: Check a checkbox
- uncheck: Uncheck a checkbox
- select: Select an option from a dropdown (requires "value" parameter)
- hover: Hover over an element

RESPOND WITH ONLY VALID JSON in this exact format:
{
  "action": "click",
  "targetDescription": "description of the target element",
  "params": {},
  "confidence": 0.9
}

EXAMPLES:
- "click the submit button" -> {"action": "click", "targetDescription": "submit button", "params": {}, "confidence": 0.95}
- "type hello@test.com in the email field" -> {"action": "type", "targetDescription": "email field", "params": {"text": "hello@test.com"}, "confidence": 0.9}
- "fill in my name with John" -> {"action": "type", "targetDescription": "name", "params": {"text": "John"}, "confidence": 0.85}
- "check remember me" -> {"action": "check", "targetDescription": "remember me", "params": {}, "confidence": 0.9}
- "select Option 2 from the dropdown" -> {"action": "select", "targetDescription": "dropdown", "params": {"value": "Option 2"}, "confidence": 0.9}

If the command is ambiguous, use "click" as default action with lower confidence.`;
}

// =============================================================================
// LLM API Calls
// =============================================================================

interface SimpleQueryRequest {
  prompt: string;
  timeout_seconds?: number;
}

interface SimpleQueryResponse {
  success: boolean;
  response?: string;
  error?: string;
}

/**
 * Call the runner's simple AI query endpoint for lightweight LLM calls.
 * This uses the configured AI provider (Claude CLI, Claude API, Gemini, etc.)
 */
async function callLLM(prompt: string, timeoutMs: number): Promise<string | null> {
  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), timeoutMs);

    const response = await tracedFetch(`${getApiBase()}/ai/simple-query`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        prompt,
        timeout_seconds: Math.ceil(timeoutMs / 1000),
      } as SimpleQueryRequest),
      signal: controller.signal,
    });

    clearTimeout(timeoutId);

    if (!response.ok) {
      console.error("[LLM] API error:", response.status, response.statusText);
      return null;
    }

    const data: SimpleQueryResponse = await response.json();
    if (!data.success || !data.response) {
      console.error("[LLM] Query failed:", data.error);
      return null;
    }

    return data.response;
  } catch (err) {
    if (err instanceof Error && err.name === "AbortError") {
      console.warn("[LLM] Request timed out");
    } else {
      console.error("[LLM] Request failed:", err);
    }
    return null;
  }
}

/**
 * Extract JSON from LLM response (handles markdown code blocks)
 */
function extractJSON<T>(text: string): T | null {
  // Try to find JSON in code blocks first
  const codeBlockMatch = text.match(/```(?:json)?\s*([\s\S]*?)```/);
  if (codeBlockMatch) {
    try {
      return JSON.parse(codeBlockMatch[1].trim()) as T;
    } catch {
      // Continue to try other methods
    }
  }

  // Try to find JSON object directly
  const jsonMatch = text.match(/\{[\s\S]*\}/);
  if (jsonMatch) {
    try {
      return JSON.parse(jsonMatch[0]) as T;
    } catch {
      // Continue
    }
  }

  // Try parsing the whole response
  try {
    return JSON.parse(text.trim()) as T;
  } catch {
    return null;
  }
}

// =============================================================================
// Public API
// =============================================================================

/**
 * Match elements using LLM-powered semantic understanding.
 *
 * @param query - User's natural language query (e.g., "the sign in button")
 * @param elements - Available UI elements
 * @param options - Matching options
 * @returns Promise resolving to match results
 */
export async function matchElementsWithLLM(
  query: string,
  elements: ExternalElement[],
  options: LLMMatchOptions = {},
): Promise<LLMMatchResponse> {
  const {
    timeout = 30000,
    maxResults = 5,
    minConfidence = 0.3,
    includeExplanation = true,
  } = options;

  const startTime = Date.now();

  // Check cache
  const cacheKey = getCacheKey(
    query,
    elements.map((e) => e.id),
  );
  const cached = getFromCache(matchCache, cacheKey);
  if (cached) {
    return cached;
  }

  // Filter to interactive elements and limit count for prompt size
  const interactiveElements = elements.filter(
    (el) => el.visible && (el.enabled || el.type === "link"),
  );
  const limitedElements = interactiveElements.slice(0, 50); // Limit for prompt size

  if (limitedElements.length === 0) {
    return {
      success: true,
      matches: [],
      duration: Date.now() - startTime,
    };
  }

  // Format elements and build prompt
  const elementSummaries = formatElementsForLLM(limitedElements);
  const prompt = buildElementMatchPrompt(query, elementSummaries, maxResults);

  // Call LLM
  const llmResponse = await callLLM(prompt, timeout);
  const duration = Date.now() - startTime;

  if (!llmResponse) {
    return {
      success: false,
      matches: [],
      error: "LLM request failed or timed out",
      duration,
    };
  }

  // Parse response
  interface LLMMatchParsed {
    matches: Array<{
      elementId: string;
      confidence: number;
      explanation?: string;
    }>;
  }

  const parsed = extractJSON<LLMMatchParsed>(llmResponse);
  if (!parsed || !Array.isArray(parsed.matches)) {
    console.error("[LLM] Failed to parse response:", llmResponse);
    return {
      success: false,
      matches: [],
      error: "Failed to parse LLM response",
      duration,
    };
  }

  // Validate and filter matches
  const validMatches = parsed.matches
    .filter((m) => {
      // Verify element ID exists
      const exists = limitedElements.some((e) => e.id === m.elementId);
      if (!exists) {
        console.warn("[LLM] Invalid element ID in response:", m.elementId);
        return false;
      }
      // Check confidence threshold
      return typeof m.confidence === "number" && m.confidence >= minConfidence;
    })
    .map((m) => ({
      elementId: m.elementId,
      confidence: Math.min(1, Math.max(0, m.confidence)), // Clamp to 0-1
      explanation: includeExplanation ? m.explanation : undefined,
    }))
    .sort((a, b) => b.confidence - a.confidence)
    .slice(0, maxResults);

  const result: LLMMatchResponse = {
    success: true,
    matches: validMatches,
    duration,
  };

  // Cache the result
  setInCache(matchCache, cacheKey, result);

  return result;
}

/**
 * Parse natural language query into structured intent using LLM.
 *
 * @param query - User's natural language command
 * @param options - Options including timeout
 * @returns Promise resolving to parsed intent
 */
export async function parseIntentWithLLM(
  query: string,
  options: { timeout?: number } = {},
): Promise<LLMIntentResponse> {
  const { timeout = 30000 } = options;
  const startTime = Date.now();

  // Check cache
  const cached = getFromCache(intentCache, query);
  if (cached) {
    return cached;
  }

  // Build prompt and call LLM
  const prompt = buildIntentParsePrompt(query);
  const llmResponse = await callLLM(prompt, timeout);
  const duration = Date.now() - startTime;

  if (!llmResponse) {
    return {
      success: false,
      error: "LLM request failed or timed out",
      duration,
    };
  }

  // Parse response
  interface LLMIntentParsed {
    action: string;
    targetDescription: string;
    params?: Record<string, unknown>;
    confidence: number;
  }

  const parsed = extractJSON<LLMIntentParsed>(llmResponse);
  if (!parsed || !parsed.action || !parsed.targetDescription) {
    console.error("[LLM] Failed to parse intent response:", llmResponse);
    return {
      success: false,
      error: "Failed to parse LLM response",
      duration,
    };
  }

  const result: LLMIntentResponse = {
    success: true,
    intent: {
      action: parsed.action.toLowerCase(),
      targetDescription: parsed.targetDescription,
      params: parsed.params || {},
      confidence: typeof parsed.confidence === "number" ? parsed.confidence : 0.7,
    },
    duration,
  };

  // Cache the result
  setInCache(intentCache, query, result);

  return result;
}

/**
 * Check if the LLM service is available by making a simple health check.
 * This can be used to show/hide the AI matching toggle.
 */
export async function isLLMAvailable(): Promise<boolean> {
  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 5000);

    const response = await tracedFetch(`${getApiBase()}/health`, {
      method: "GET",
      signal: controller.signal,
    });

    clearTimeout(timeoutId);

    if (!response.ok) return false;

    const data = await response.json();
    // Check if AI is configured
    return data.ai_configured === true;
  } catch {
    return false;
  }
}
