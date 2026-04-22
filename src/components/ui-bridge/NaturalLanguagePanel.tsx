/**
 * Natural Language Action Panel
 *
 * Allows users to execute actions using natural language commands.
 * Part of Phase 3 of the AI-Native UI Bridge enhancements.
 *
 * Features:
 * - Natural language input
 * - Intent parsing (action + target)
 * - Element matching with confidence
 * - Alternative interpretations
 * - Execute with confirmation
 * - Integration with persisted element descriptions
 * - Command history
 */

import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import {
  MessageSquare,
  Zap,
  Target,
  AlertCircle,
  Check,
  ChevronDown,
  ChevronRight,
  Play,
  RefreshCw,
  Sparkles,
  MousePointer,
  Type,
  Eye,
  Trash2,
  ToggleRight,
  List,
  CheckCircle,
  XCircle,
  Bot,
  Loader2,
  Lightbulb,
  History,
  Database,
} from "lucide-react";
import type { ExternalElement, CommandResult, PageContext } from "../../types/ui-bridge-types";
import {
  ElementDescriptionService,
  type PersistedElementDescription,
} from "../../services/element-description-service";
import {
  normalizeAction,
  normalizeElement,
  expandQuery,
  getElementSynonyms,
  containsElementSynonym,
} from "../../lib/ui-bridge/synonyms";
import {
  generateQuerySuggestions,
  shouldRegenerateSuggestions,
  type QuerySuggestion,
} from "../../lib/ui-bridge/querySuggestions";
import {
  matchElementsWithLLM,
  isLLMAvailable,
  type LLMMatchResult,
} from "../../lib/ui-bridge/llmElementMatcher";
import { createLogger } from "@/lib/logger";

const logger = createLogger("NaturalLanguagePanel");

interface NaturalLanguagePanelProps {
  elements: ExternalElement[];
  onExecuteAction: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<CommandResult>;
  onSelectElement: (elementId: string) => void;
  onHighlightElement: (elementId: string) => void;
  pageContext?: PageContext | null;
  disabled?: boolean;
}

/** Command history entry */
interface CommandHistoryEntry {
  command: string;
  action: string;
  elementId: string;
  timestamp: number;
  success: boolean;
}

const COMMAND_HISTORY_KEY = "qontinui-nl-command-history";
const MAX_HISTORY_ENTRIES = 20;

interface ParsedIntent {
  action: string;
  actionLabel: string;
  targetDescription: string;
  params?: Record<string, unknown>;
  confidence: number;
}

interface ElementMatch {
  element: ExternalElement;
  score: number;
  matchReason: string;
  /** Whether this match came from LLM */
  fromLLM?: boolean;
}

interface InterpretationResult {
  intent: ParsedIntent;
  matches: ElementMatch[];
  bestMatch: ElementMatch | null;
  /** Whether LLM was used for matching */
  usedLLM?: boolean;
  /** LLM matching status */
  llmStatus?: "idle" | "loading" | "success" | "error" | "timeout";
  /** LLM error message if any */
  llmError?: string;
}

// Action patterns with their variations
const ACTION_PATTERNS: Array<{
  action: string;
  label: string;
  patterns: RegExp[];
  icon: React.ReactNode;
  requiresText?: boolean;
}> = [
  {
    action: "click",
    label: "Click",
    patterns: [
      /^click\s+(?:on\s+)?(?:the\s+)?(.+)/i,
      /^press\s+(?:the\s+)?(.+)/i,
      /^tap\s+(?:on\s+)?(?:the\s+)?(.+)/i,
      /^select\s+(?:the\s+)?(.+)/i,
      /^activate\s+(?:the\s+)?(.+)/i,
    ],
    icon: <MousePointer className="w-4 h-4" />,
  },
  {
    action: "type",
    label: "Type",
    patterns: [
      /^type\s+"([^"]+)"\s+(?:in(?:to)?|on)\s+(?:the\s+)?(.+)/i,
      /^enter\s+"([^"]+)"\s+(?:in(?:to)?|on)\s+(?:the\s+)?(.+)/i,
      /^fill\s+(?:the\s+)?(.+)\s+with\s+"([^"]+)"/i,
      /^input\s+"([^"]+)"\s+(?:in(?:to)?|on)\s+(?:the\s+)?(.+)/i,
    ],
    icon: <Type className="w-4 h-4" />,
    requiresText: true,
  },
  {
    action: "clear",
    label: "Clear",
    patterns: [
      /^clear\s+(?:the\s+)?(.+)/i,
      /^empty\s+(?:the\s+)?(.+)/i,
      /^reset\s+(?:the\s+)?(.+)/i,
    ],
    icon: <Trash2 className="w-4 h-4" />,
  },
  {
    action: "focus",
    label: "Focus",
    patterns: [/^focus\s+(?:on\s+)?(?:the\s+)?(.+)/i, /^go\s+to\s+(?:the\s+)?(.+)/i],
    icon: <Eye className="w-4 h-4" />,
  },
  {
    action: "check",
    label: "Check",
    patterns: [
      /^check\s+(?:the\s+)?(.+)/i,
      /^enable\s+(?:the\s+)?(.+)/i,
      /^turn\s+on\s+(?:the\s+)?(.+)/i,
    ],
    icon: <Check className="w-4 h-4" />,
  },
  {
    action: "uncheck",
    label: "Uncheck",
    patterns: [
      /^uncheck\s+(?:the\s+)?(.+)/i,
      /^disable\s+(?:the\s+)?(.+)/i,
      /^turn\s+off\s+(?:the\s+)?(.+)/i,
    ],
    icon: <ToggleRight className="w-4 h-4" />,
  },
  {
    action: "select",
    label: "Select Option",
    patterns: [
      /^choose\s+"([^"]+)"\s+(?:from|in)\s+(?:the\s+)?(.+)/i,
      /^pick\s+"([^"]+)"\s+(?:from|in)\s+(?:the\s+)?(.+)/i,
      /^select\s+"([^"]+)"\s+(?:from|in)\s+(?:the\s+)?(.+)/i,
    ],
    icon: <List className="w-4 h-4" />,
    requiresText: true,
  },
  {
    action: "hover",
    label: "Hover",
    patterns: [/^hover\s+(?:over\s+)?(?:the\s+)?(.+)/i, /^mouse\s+over\s+(?:the\s+)?(.+)/i],
    icon: <MousePointer className="w-4 h-4" />,
  },
];

/**
 * Parse natural language input to extract intent
 *
 * Uses synonym normalization to detect actions even when users
 * use alternative verbs (e.g., "tap" -> "click", "write" -> "type")
 */
function parseIntent(input: string): ParsedIntent | null {
  const trimmed = input.trim();
  if (!trimmed) return null;

  // First, try to match against defined patterns
  for (const pattern of ACTION_PATTERNS) {
    for (const regex of pattern.patterns) {
      const match = trimmed.match(regex);
      if (match) {
        let targetDescription: string;
        let params: Record<string, unknown> | undefined;

        if (pattern.requiresText && match.length >= 3) {
          // For actions like "type X in Y" or "fill Y with X"
          if (pattern.action === "type" || pattern.action === "select") {
            // Check if it's "fill X with Y" pattern (target first)
            if (regex.source.includes("fill")) {
              targetDescription = match[1];
              params = { text: match[2] };
            } else {
              // "type X in Y" pattern (text first)
              params = { text: match[1] };
              targetDescription = match[2];
            }
          } else {
            targetDescription = match[2] || match[1];
            params = { value: match[1] };
          }
        } else {
          targetDescription = match[1];
        }

        return {
          action: pattern.action,
          actionLabel: pattern.label,
          targetDescription: targetDescription.trim(),
          params,
          confidence: 0.9, // High confidence for pattern match
        };
      }
    }
  }

  // Second, try to detect action via synonym normalization
  // This catches cases like "hit the submit button" where "hit" isn't in patterns
  const words = trimmed.split(/\s+/);
  if (words.length >= 2) {
    const firstWord = words[0].toLowerCase();
    const normalizedAction = normalizeAction(firstWord);

    // Check if the normalized action maps to a known pattern
    const matchedPattern = ACTION_PATTERNS.find((p) => p.action === normalizedAction);
    if (matchedPattern && normalizedAction !== firstWord) {
      // We found a synonym match - extract the target description
      // Remove the action word and common filler words
      const targetDescription = trimmed
        .substring(firstWord.length)
        .trim()
        .replace(/^(?:on\s+)?(?:the\s+)?/i, "")
        .trim();

      if (targetDescription) {
        return {
          action: matchedPattern.action,
          actionLabel: matchedPattern.label,
          targetDescription,
          confidence: 0.8, // Slightly lower confidence for synonym match
        };
      }
    }
  }

  // Fallback: assume click if no action verb found
  // Look for just a target description
  const fallbackMatch = trimmed.match(/^(?:the\s+)?(.+)/i);
  if (fallbackMatch) {
    return {
      action: "click",
      actionLabel: "Click",
      targetDescription: fallbackMatch[1].trim(),
      confidence: 0.5, // Lower confidence for assumed action
    };
  }

  return null;
}

/**
 * Calculate string similarity (simple word overlap)
 */
function calculateSimilarity(a: string, b: string): number {
  const wordsA = new Set(a.toLowerCase().split(/\s+/));
  const wordsB = new Set(b.toLowerCase().split(/\s+/));

  let overlap = 0;
  wordsA.forEach((word) => {
    if (wordsB.has(word)) overlap++;
  });

  const maxSize = Math.max(wordsA.size, wordsB.size);
  return maxSize > 0 ? overlap / maxSize : 0;
}

/**
 * Find elements matching the target description
 *
 * Uses synonym expansion to match elements even when users use
 * alternative terms (e.g., "sign in" matches "login", "btn" matches "button")
 *
 * Also checks against persisted element descriptions for enhanced matching.
 */
function findMatchingElements(
  targetDescription: string,
  elements: ExternalElement[],
  descriptions?: Map<string, PersistedElementDescription>,
): ElementMatch[] {
  const matches: ElementMatch[] = [];
  const lowerTarget = targetDescription.toLowerCase();
  const targetWords = lowerTarget.split(/\s+/);

  // Expand the query with synonyms to check multiple variations
  const expandedQueries = expandQuery(lowerTarget);

  // Get canonical element concepts from the target for synonym matching
  const targetConcepts = normalizeElement(lowerTarget);

  for (const element of elements) {
    let score = 0;
    const reasons: string[] = [];
    const elementId = element.id.toLowerCase();
    const elementLabel = element.label?.toLowerCase() || "";
    const elementText = element.text?.toLowerCase() || "";

    // Check persisted description if available
    const desc = descriptions?.get(element.id);
    if (desc) {
      // Check aliases from persisted description
      for (const alias of desc.aliases) {
        const lowerAlias = alias.toLowerCase();
        if (lowerAlias.includes(lowerTarget) || lowerTarget.includes(lowerAlias)) {
          score += 0.95; // High score for alias match
          reasons.push(`Alias match: "${alias}"`);
          break;
        }
      }

      // Check description text
      const lowerDesc = desc.description.toLowerCase();
      if (lowerDesc.includes(lowerTarget)) {
        score += 0.7;
        reasons.push("Description contains target");
      } else {
        const descSimilarity = calculateSimilarity(desc.description, lowerTarget);
        if (descSimilarity > 0.3) {
          score += descSimilarity * 0.6;
          reasons.push(`Description similarity: ${(descSimilarity * 100).toFixed(0)}%`);
        }
      }

      // Check purpose
      const lowerPurpose = desc.purpose.toLowerCase();
      if (lowerPurpose.includes(lowerTarget)) {
        score += 0.5;
        reasons.push("Purpose contains target");
      }

      // Check interaction (AI-generated)
      if (desc.interaction) {
        const lowerInteraction = desc.interaction.toLowerCase();
        if (lowerInteraction.includes(lowerTarget)) {
          score += 0.4;
          reasons.push("Interaction guide matches");
        }
      }
    }

    // Check ID match (original + expanded queries)
    let idMatched = false;
    if (elementId.includes(lowerTarget)) {
      score += 0.8;
      reasons.push("ID contains target");
      idMatched = true;
    } else {
      // Check expanded queries against ID
      for (const expanded of expandedQueries) {
        if (expanded !== lowerTarget && elementId.includes(expanded)) {
          score += 0.75;
          reasons.push(`ID contains synonym "${expanded}"`);
          idMatched = true;
          break;
        }
      }
    }

    if (!idMatched) {
      // Check word overlap with ID
      const idSimilarity = calculateSimilarity(element.id, lowerTarget);
      if (idSimilarity > 0.3) {
        score += idSimilarity * 0.6;
        reasons.push(`ID similarity: ${(idSimilarity * 100).toFixed(0)}%`);
      }
    }

    // Check label match (original + expanded queries)
    if (element.label) {
      let labelMatched = false;
      if (elementLabel.includes(lowerTarget)) {
        score += 0.9;
        reasons.push("Label contains target");
        labelMatched = true;
      } else {
        // Check expanded queries against label
        for (const expanded of expandedQueries) {
          if (expanded !== lowerTarget && elementLabel.includes(expanded)) {
            score += 0.85;
            reasons.push(`Label contains synonym "${expanded}"`);
            labelMatched = true;
            break;
          }
        }
      }

      if (!labelMatched) {
        const labelSimilarity = calculateSimilarity(element.label, lowerTarget);
        if (labelSimilarity > 0.3) {
          score += labelSimilarity * 0.7;
          reasons.push(`Label similarity: ${(labelSimilarity * 100).toFixed(0)}%`);
        }
      }
    }

    // Check text match (original + expanded queries)
    if (element.text) {
      let textMatched = false;
      if (elementText.includes(lowerTarget)) {
        score += 0.85;
        reasons.push("Text contains target");
        textMatched = true;
      } else {
        // Check expanded queries against text
        for (const expanded of expandedQueries) {
          if (expanded !== lowerTarget && elementText.includes(expanded)) {
            score += 0.8;
            reasons.push(`Text contains synonym "${expanded}"`);
            textMatched = true;
            break;
          }
        }
      }

      if (!textMatched) {
        const textSimilarity = calculateSimilarity(element.text, lowerTarget);
        if (textSimilarity > 0.3) {
          score += textSimilarity * 0.65;
          reasons.push(`Text similarity: ${(textSimilarity * 100).toFixed(0)}%`);
        }
      }
    }

    // Check type match (e.g., "button", "input") - use synonyms
    if (targetWords.includes(element.type)) {
      score += 0.3;
      reasons.push("Type matches");
    } else {
      // Check if target contains a synonym of the element type
      // E.g., if element is "button" and target contains "btn"
      if (containsElementSynonym(lowerTarget, element.type)) {
        score += 0.25;
        reasons.push(`Type synonym matches "${element.type}"`);
      }
    }

    // Check for specific element type keywords (using synonyms from dictionary)
    const typeKeywords: Record<string, string[]> = {
      button: getElementSynonyms("button"),
      input: getElementSynonyms("input"),
      link: ["link", "anchor", "href"],
      checkbox: ["checkbox", "check box", "toggle"],
      select: getElementSynonyms("dropdown"),
    };

    for (const [type, keywords] of Object.entries(typeKeywords)) {
      if (element.type === type) {
        for (const keyword of keywords) {
          if (lowerTarget.includes(keyword)) {
            score += 0.2;
            reasons.push(`Keyword "${keyword}" matches type`);
            break;
          }
        }
      }
    }

    // Check if element matches any canonical concepts from the target
    // E.g., if user says "sign in" and element has "login" in its properties
    for (const concept of targetConcepts) {
      const synonyms = getElementSynonyms(concept);
      for (const syn of synonyms) {
        if (elementId.includes(syn) || elementLabel.includes(syn) || elementText.includes(syn)) {
          score += 0.4;
          reasons.push(`Matches concept "${concept}" (via "${syn}")`);
          break;
        }
      }
    }

    if (score > 0 && reasons.length > 0) {
      matches.push({
        element,
        score: Math.min(score, 1), // Cap at 1
        matchReason: reasons.join(", "),
      });
    }
  }

  // Sort by score descending
  return matches.sort((a, b) => b.score - a.score).slice(0, 5);
}

/**
 * Interpret natural language command
 */
function interpretCommand(
  input: string,
  elements: ExternalElement[],
  descriptions?: Map<string, PersistedElementDescription>,
): InterpretationResult | null {
  const intent = parseIntent(input);
  if (!intent) return null;

  const matches = findMatchingElements(intent.targetDescription, elements, descriptions);
  const bestMatch = matches.length > 0 ? matches[0] : null;

  return {
    intent,
    matches,
    bestMatch,
  };
}

// Example commands for quick start
const EXAMPLE_COMMANDS = [
  'Click the "Submit" button',
  'Type "hello@example.com" in the email field',
  "Clear the search input",
  "Check the remember me checkbox",
  'Select "Option 1" from the dropdown',
  "Focus on the username field",
];

export function NaturalLanguagePanel({
  elements,
  onExecuteAction,
  onSelectElement: _onSelectElement,
  onHighlightElement,
  pageContext,
  disabled = false,
}: NaturalLanguagePanelProps) {
  const [input, setInput] = useState("");
  const [interpretation, setInterpretation] = useState<InterpretationResult | null>(null);
  const [isExecuting, setIsExecuting] = useState(false);
  const [lastResult, setLastResult] = useState<{
    success: boolean;
    command: string;
    elementId: string;
    error?: string;
    timestamp: number;
  } | null>(null);
  const [showExamples, setShowExamples] = useState(true);
  const [showAlternatives, setShowAlternatives] = useState(false);
  const [showSuggestions, setShowSuggestions] = useState(true);
  const [showHistory, setShowHistory] = useState(false);

  // Persisted element descriptions for enhanced matching
  const [descriptions, setDescriptions] = useState<Map<string, PersistedElementDescription>>(
    new Map(),
  );
  const loadedPageRef = useRef<string | null>(null);

  // Command history
  const [commandHistory, setCommandHistory] = useState<CommandHistoryEntry[]>(() => {
    return instanceStorage.getJSON<CommandHistoryEntry[]>(COMMAND_HISTORY_KEY, []);
  });

  // AI Matching state
  const [aiMatchingEnabled, setAiMatchingEnabled] = useState(() => {
    // Load preference from instanceStorage
    const stored = instanceStorage.getItem("nlp-ai-matching-enabled");
    return stored === "true";
  });
  const [aiAvailable, setAiAvailable] = useState(false);
  const [isLoadingAI, setIsLoadingAI] = useState(false);

  // Track previous elements for suggestion regeneration
  const prevElementsRef = useRef<ExternalElement[]>([]);

  // Check AI availability on mount
  useEffect(() => {
    isLLMAvailable().then(setAiAvailable);
  }, []);

  // Save AI matching preference to instanceStorage
  useEffect(() => {
    instanceStorage.setItem("nlp-ai-matching-enabled", String(aiMatchingEnabled));
  }, [aiMatchingEnabled]);

  // Load persisted descriptions when page context changes
  useEffect(() => {
    if (!pageContext?.url) {
      loadedPageRef.current = null;

      setDescriptions(new Map());
      return;
    }

    if (loadedPageRef.current === pageContext.url) {
      return;
    }

    loadedPageRef.current = pageContext.url;
    const loaded = ElementDescriptionService.loadDescriptions(pageContext.url);
    setDescriptions(loaded);
    logger.debug(`Loaded ${loaded.size} descriptions for enhanced matching`);
  }, [pageContext?.url]);

  // Save command history to instanceStorage
  useEffect(() => {
    instanceStorage.setJSON(COMMAND_HISTORY_KEY, commandHistory);
  }, [commandHistory]);

  // Add command to history
  const addToHistory = useCallback(
    (command: string, action: string, elementId: string, success: boolean) => {
      setCommandHistory((prev) => {
        const newEntry: CommandHistoryEntry = {
          command,
          action,
          elementId,
          timestamp: Date.now(),
          success,
        };
        // Remove duplicate command if exists
        const filtered = prev.filter((e) => e.command !== command);
        // Add to front, limit size
        return [newEntry, ...filtered].slice(0, MAX_HISTORY_ENTRIES);
      });
    },
    [],
  );

  // Clear command history
  const clearHistory = useCallback(() => {
    setCommandHistory([]);
    instanceStorage.removeItem(COMMAND_HISTORY_KEY);
  }, []);

  // Generate query suggestions based on page elements
  const querySuggestions = useMemo(() => {
    return generateQuerySuggestions(elements, { maxSuggestions: 6 });
  }, [elements]);

  // Regenerate suggestions when elements change significantly
  useEffect(() => {
    if (shouldRegenerateSuggestions(prevElementsRef.current, elements)) {
      // Show suggestions when elements change (new page loaded)
      setShowSuggestions(true);
    }
    prevElementsRef.current = elements;
  }, [elements]);

  // Collapse suggestions when user starts typing their own query
  useEffect(() => {
    if (input.trim().length > 0) {
      setShowSuggestions(false);
    }
  }, [input]);

  // Handle clicking a suggestion
  const handleSuggestionClick = useCallback(
    async (suggestion: QuerySuggestion) => {
      // Fill the input with the suggestion text
      setInput(suggestion.text);

      // Also immediately execute it
      setIsExecuting(true);
      setLastResult(null);

      try {
        const result = await onExecuteAction(suggestion.elementId, suggestion.action, undefined);

        setLastResult({
          success: result.success,
          command: suggestion.text,
          elementId: suggestion.elementId,
          error: result.error,
          timestamp: Date.now(),
        });

        if (result.success) {
          // Clear input on success
          setInput("");
          setInterpretation(null);
          // Show suggestions again for next action
          setShowSuggestions(true);
        }
      } catch (err) {
        setLastResult({
          success: false,
          command: suggestion.text,
          elementId: suggestion.elementId,
          error: err instanceof Error ? err.message : "Execution failed",
          timestamp: Date.now(),
        });
      } finally {
        setIsExecuting(false);
      }
    },
    [onExecuteAction],
  );

  // Real-time interpretation as user types
  useEffect(() => {
    let cancelled = false;

    async function interpretWithAI() {
      if (!input.trim() || elements.length === 0) {
        setInterpretation(null);
        return;
      }

      // First, do regex/synonym matching with descriptions for enhanced matching
      const regexResult = interpretCommand(input, elements, descriptions);
      setInterpretation(regexResult);

      // If AI matching is enabled and regex match has low confidence, try LLM
      const shouldTryAI =
        aiMatchingEnabled &&
        aiAvailable &&
        regexResult &&
        (!regexResult.bestMatch || regexResult.bestMatch.score < 0.7);

      if (shouldTryAI && regexResult) {
        setIsLoadingAI(true);

        // Update interpretation to show loading state
        setInterpretation({
          ...regexResult,
          llmStatus: "loading",
        });

        try {
          // Call LLM for element matching
          const llmResult = await matchElementsWithLLM(
            regexResult.intent.targetDescription,
            elements,
            { timeout: 30000, maxResults: 5 },
          );

          if (cancelled) return;

          if (llmResult.success && llmResult.matches.length > 0) {
            // Convert LLM matches to ElementMatch format
            const llmMatches: ElementMatch[] = llmResult.matches
              .map((m: LLMMatchResult) => {
                const element = elements.find((e) => e.id === m.elementId);
                if (!element) return null;
                return {
                  element,
                  score: m.confidence,
                  matchReason: m.explanation || "AI matched",
                  fromLLM: true,
                } as ElementMatch;
              })
              .filter((m): m is ElementMatch => m !== null);

            if (llmMatches.length > 0) {
              // Merge with regex matches, preferring LLM matches
              const allMatches = [...llmMatches];
              for (const regexMatch of regexResult.matches) {
                if (!allMatches.some((m) => m.element.id === regexMatch.element.id)) {
                  allMatches.push(regexMatch);
                }
              }

              // Sort by score
              allMatches.sort((a, b) => b.score - a.score);

              setInterpretation({
                ...regexResult,
                matches: allMatches.slice(0, 5),
                bestMatch: allMatches[0] || null,
                usedLLM: true,
                llmStatus: "success",
              });
            } else {
              // LLM returned no matches, keep regex result
              setInterpretation({
                ...regexResult,
                llmStatus: "success",
              });
            }
          } else {
            // LLM failed or returned no matches
            setInterpretation({
              ...regexResult,
              llmStatus: llmResult.error ? "error" : "success",
              llmError: llmResult.error,
            });
          }
        } catch (err) {
          if (!cancelled) {
            setInterpretation({
              ...regexResult,
              llmStatus: "error",
              llmError: err instanceof Error ? err.message : "AI matching failed",
            });
          }
        } finally {
          if (!cancelled) {
            setIsLoadingAI(false);
          }
        }
      }
    }

    // Debounce the AI call
    const timeoutId = setTimeout(interpretWithAI, 300);

    return () => {
      cancelled = true;
      clearTimeout(timeoutId);
    };
  }, [input, elements, descriptions, aiMatchingEnabled, aiAvailable]);

  // Execute the interpreted command
  const handleExecute = useCallback(async () => {
    if (!interpretation?.bestMatch || isExecuting) return;

    const { intent, bestMatch } = interpretation;
    setIsExecuting(true);
    setLastResult(null);

    try {
      const result = await onExecuteAction(bestMatch.element.id, intent.action, intent.params);

      setLastResult({
        success: result.success,
        command: input,
        elementId: bestMatch.element.id,
        error: result.error,
        timestamp: Date.now(),
      });

      // Add to history
      addToHistory(input, intent.action, bestMatch.element.id, result.success);

      if (result.success) {
        // Clear input on success
        setInput("");
        setInterpretation(null);
      }
    } catch (err) {
      setLastResult({
        success: false,
        command: input,
        elementId: bestMatch.element.id,
        error: err instanceof Error ? err.message : "Execution failed",
        timestamp: Date.now(),
      });
      addToHistory(input, intent.action, bestMatch.element.id, false);
    } finally {
      setIsExecuting(false);
    }
  }, [interpretation, input, isExecuting, onExecuteAction, addToHistory]);

  // Execute on an alternative match
  const handleExecuteAlternative = useCallback(
    async (match: ElementMatch) => {
      if (!interpretation || isExecuting) return;

      const { intent } = interpretation;
      setIsExecuting(true);
      setLastResult(null);

      try {
        const result = await onExecuteAction(match.element.id, intent.action, intent.params);

        setLastResult({
          success: result.success,
          command: input,
          elementId: match.element.id,
          error: result.error,
          timestamp: Date.now(),
        });

        // Add to history
        addToHistory(input, intent.action, match.element.id, result.success);

        if (result.success) {
          setInput("");
          setInterpretation(null);
        }
      } catch (err) {
        setLastResult({
          success: false,
          command: input,
          elementId: match.element.id,
          error: err instanceof Error ? err.message : "Execution failed",
          timestamp: Date.now(),
        });
        addToHistory(input, intent.action, match.element.id, false);
      } finally {
        setIsExecuting(false);
      }
    },
    [interpretation, input, isExecuting, onExecuteAction, addToHistory],
  );

  // Load command from history
  const handleLoadHistoryCommand = useCallback((entry: CommandHistoryEntry) => {
    setInput(entry.command);
    setShowHistory(false);
  }, []);

  // Load example command
  const handleLoadExample = useCallback((example: string) => {
    setInput(example);
    setShowExamples(false);
  }, []);

  // Get action icon
  const getActionIcon = (action: string) => {
    const pattern = ACTION_PATTERNS.find((p) => p.action === action);
    return pattern?.icon || <Zap className="w-4 h-4" />;
  };

  if (disabled || elements.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <MessageSquare className="w-8 h-8 opacity-50" />
        <p>Connect to a browser tab to use natural language commands</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* AI Matching Toggle */}
      {aiAvailable && (
        <div className="mb-3 flex items-center justify-between">
          <button
            onClick={() => setAiMatchingEnabled(!aiMatchingEnabled)}
            className={`flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
              aiMatchingEnabled
                ? "bg-primary/20 text-primary border border-primary/30"
                : "bg-muted/30 text-muted-foreground border border-border/50 hover:bg-muted/50"
            }`}
            title={
              aiMatchingEnabled
                ? "AI matching enabled - uses LLM for semantic element matching"
                : "Enable AI matching for better element recognition"
            }
          >
            <Bot className="w-3.5 h-3.5" />
            AI Matching
            {aiMatchingEnabled && <Check className="w-3 h-3" />}
          </button>
          {isLoadingAI && (
            <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
              <span>Analyzing with AI...</span>
            </div>
          )}
        </div>
      )}

      {/* Input area */}
      <div className="mb-4">
        <div className="flex gap-2">
          <div className="relative flex-1">
            <MessageSquare className="absolute left-2.5 top-3 w-4 h-4 text-muted-foreground" />
            <textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  handleExecute();
                }
              }}
              placeholder='Try: Click the "Submit" button'
              rows={2}
              className="w-full pl-9 pr-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary resize-none"
            />
          </div>
          <Button
            onClick={handleExecute}
            disabled={!interpretation?.bestMatch || isExecuting}
            className="self-end"
          >
            {isExecuting ? (
              <>
                <RefreshCw className="w-4 h-4 mr-1 animate-spin" />
                Running
              </>
            ) : (
              <>
                <Play className="w-4 h-4 mr-1" />
                Execute
              </>
            )}
          </Button>
        </div>
        <p className="text-xs text-muted-foreground mt-1">
          Press Enter to execute, Shift+Enter for new line
        </p>
      </div>

      {/* Last result */}
      {lastResult && (
        <div
          className={`mb-4 p-3 rounded-lg border ${
            lastResult.success
              ? "bg-green-500/10 border-green-500/30"
              : "bg-destructive/10 border-destructive/30"
          }`}
        >
          <div className="flex items-center gap-2">
            {lastResult.success ? (
              <CheckCircle className="w-4 h-4 text-green-500" />
            ) : (
              <XCircle className="w-4 h-4 text-destructive" />
            )}
            <span className="text-sm font-medium">
              {lastResult.success ? "Executed successfully" : "Execution failed"}
            </span>
            <span className="text-xs text-muted-foreground ml-auto">
              {new Date(lastResult.timestamp).toLocaleTimeString()}
            </span>
          </div>
          <p className="text-xs text-muted-foreground mt-1">
            Command: "{lastResult.command}" → {lastResult.elementId}
          </p>
          {lastResult.error && <p className="text-xs text-destructive mt-1">{lastResult.error}</p>}
        </div>
      )}

      {/* Interpretation preview */}
      {interpretation && (
        <div className="mb-4 p-3 bg-muted/20 rounded-lg border border-border/30">
          <div className="flex items-center gap-2 mb-2">
            <Sparkles className="w-4 h-4 text-primary" />
            <span className="text-sm font-medium">Parsed Intent</span>
            {interpretation.usedLLM && (
              <Badge variant="info" className="text-[10px]">
                <Bot className="w-2.5 h-2.5 mr-0.5" />
                AI
              </Badge>
            )}
            {interpretation.llmStatus === "loading" && (
              <Badge variant="muted" className="text-[10px]">
                <Loader2 className="w-2.5 h-2.5 mr-0.5 animate-spin" />
                Analyzing...
              </Badge>
            )}
            <Badge
              variant={interpretation.intent.confidence >= 0.7 ? "success" : "warning"}
              className="text-[10px] ml-auto"
            >
              {(interpretation.intent.confidence * 100).toFixed(0)}% confidence
            </Badge>
          </div>

          <div className="grid grid-cols-2 gap-2 text-sm">
            <div>
              <span className="text-xs text-muted-foreground">Action:</span>
              <div className="flex items-center gap-1 mt-0.5">
                {getActionIcon(interpretation.intent.action)}
                <span className="font-medium">{interpretation.intent.actionLabel}</span>
              </div>
            </div>
            <div>
              <span className="text-xs text-muted-foreground">Target:</span>
              <p className="mt-0.5 truncate">"{interpretation.intent.targetDescription}"</p>
            </div>
            {interpretation.intent.params && (
              <div className="col-span-2">
                <span className="text-xs text-muted-foreground">Parameters:</span>
                <p className="mt-0.5 font-mono text-xs bg-muted/30 p-1 rounded">
                  {JSON.stringify(interpretation.intent.params)}
                </p>
              </div>
            )}
          </div>

          {/* Best match */}
          {interpretation.bestMatch ? (
            <div className="mt-3 pt-3 border-t border-border/30">
              <div className="flex items-center gap-2 mb-2">
                <Target className="w-4 h-4 text-accent" />
                <span className="text-sm font-medium">Best Match</span>
                {interpretation.bestMatch.fromLLM && (
                  <Badge variant="info" className="text-[10px]">
                    <Bot className="w-2.5 h-2.5 mr-0.5" />
                    AI
                  </Badge>
                )}
                <Badge variant="success" className="text-[10px]">
                  {(interpretation.bestMatch.score * 100).toFixed(0)}% match
                </Badge>
              </div>

              <div className="p-2 bg-accent/10 border border-accent/20 rounded">
                <div className="flex items-center gap-2">
                  <Badge variant="muted" className="text-[10px]">
                    {interpretation.bestMatch.element.type}
                  </Badge>
                  <span className="font-mono text-sm truncate flex-1">
                    {interpretation.bestMatch.element.id}
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-6 px-2"
                    onClick={() => onHighlightElement(interpretation.bestMatch!.element.id)}
                  >
                    <Eye className="w-3 h-3" />
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground mt-1">
                  {interpretation.bestMatch.matchReason}
                </p>
                {(interpretation.bestMatch.element.label ||
                  interpretation.bestMatch.element.text) && (
                  <p className="text-xs mt-1 truncate">
                    {interpretation.bestMatch.element.label ||
                      interpretation.bestMatch.element.text}
                  </p>
                )}
              </div>
            </div>
          ) : (
            <div className="mt-3 pt-3 border-t border-border/30">
              <div className="flex items-center gap-2 text-amber-500">
                <AlertCircle className="w-4 h-4" />
                <span className="text-sm">No matching element found</span>
              </div>
              <p className="text-xs text-muted-foreground mt-1">
                Try being more specific or check the Elements tab to see available IDs
              </p>
              {aiMatchingEnabled &&
                !interpretation.usedLLM &&
                interpretation.llmStatus !== "loading" && (
                  <p className="text-xs text-muted-foreground mt-1">
                    <Bot className="w-3 h-3 inline mr-1" />
                    AI matching is analyzing the page...
                  </p>
                )}
            </div>
          )}

          {/* LLM Error */}
          {interpretation.llmStatus === "error" && interpretation.llmError && (
            <div className="mt-3 pt-3 border-t border-border/30">
              <div className="flex items-center gap-2 text-amber-500">
                <AlertCircle className="w-4 h-4" />
                <span className="text-xs">AI matching error: {interpretation.llmError}</span>
              </div>
            </div>
          )}

          {/* Alternative matches */}
          {interpretation.matches.length > 1 && (
            <div className="mt-3">
              <button
                className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
                onClick={() => setShowAlternatives(!showAlternatives)}
              >
                {showAlternatives ? (
                  <ChevronDown className="w-3.5 h-3.5" />
                ) : (
                  <ChevronRight className="w-3.5 h-3.5" />
                )}
                {interpretation.matches.length - 1} alternative match
                {interpretation.matches.length > 2 ? "es" : ""}
              </button>

              {showAlternatives && (
                <div className="mt-2 space-y-1">
                  {interpretation.matches.slice(1).map((match) => (
                    <div
                      key={match.element.id}
                      className="p-2 bg-muted/10 border border-border/30 rounded text-sm"
                    >
                      <div className="flex items-center gap-2">
                        <Badge variant="muted" className="text-[10px]">
                          {match.element.type}
                        </Badge>
                        <span className="font-mono text-xs truncate flex-1">
                          {match.element.id}
                        </span>
                        <span className="text-xs text-muted-foreground">
                          {(match.score * 100).toFixed(0)}%
                        </span>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-5 px-1.5 text-xs"
                          onClick={() => handleExecuteAlternative(match)}
                          disabled={isExecuting}
                        >
                          <Play className="w-3 h-3 mr-0.5" />
                          Use
                        </Button>
                      </div>
                      <p className="text-xs text-muted-foreground mt-0.5">{match.matchReason}</p>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {/* Description Enhanced Matching Indicator */}
      {descriptions.size > 0 && !interpretation && (
        <div className="mb-3 flex items-center gap-2 text-xs text-emerald-600 dark:text-emerald-400">
          <Database className="w-3.5 h-3.5" />
          <span>{descriptions.size} element descriptions loaded for enhanced matching</span>
        </div>
      )}

      {/* Smart Query Suggestions - show when not typing and suggestions available */}
      {!interpretation && querySuggestions.length > 0 && (
        <div className="mb-4">
          <button
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground mb-2"
            onClick={() => setShowSuggestions(!showSuggestions)}
          >
            {showSuggestions ? (
              <ChevronDown className="w-3.5 h-3.5" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5" />
            )}
            <Lightbulb className="w-3.5 h-3.5 text-amber-500" />
            <span>Suggested for this page</span>
            <Badge variant="muted" className="text-[10px] ml-1">
              {querySuggestions.length}
            </Badge>
          </button>

          {showSuggestions && (
            <div className="flex flex-wrap gap-2">
              {querySuggestions.map((suggestion) => (
                <button
                  key={suggestion.elementId}
                  className="inline-flex items-center gap-1.5 px-2.5 py-1.5 text-xs bg-primary/10 hover:bg-primary/20 border border-primary/20 hover:border-primary/40 rounded-full transition-all duration-150 disabled:opacity-50 disabled:cursor-not-allowed"
                  onClick={() => handleSuggestionClick(suggestion)}
                  onMouseEnter={() => onHighlightElement(suggestion.elementId)}
                  disabled={isExecuting}
                  title={suggestion.reason}
                >
                  {suggestion.action === "click" && <MousePointer className="w-3 h-3" />}
                  {suggestion.action === "type" && <Type className="w-3 h-3" />}
                  {suggestion.action === "check" && <Check className="w-3 h-3" />}
                  {suggestion.action === "uncheck" && <ToggleRight className="w-3 h-3" />}
                  {suggestion.action === "focus" && <Eye className="w-3 h-3" />}
                  <span className="max-w-[180px] truncate">{suggestion.text}</span>
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Command History */}
      {!interpretation && commandHistory.length > 0 && (
        <div className="mb-4">
          <button
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground mb-2"
            onClick={() => setShowHistory(!showHistory)}
          >
            {showHistory ? (
              <ChevronDown className="w-3.5 h-3.5" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5" />
            )}
            <History className="w-3.5 h-3.5 text-blue-500" />
            <span>Command History</span>
            <Badge variant="muted" className="text-[10px] ml-1">
              {commandHistory.length}
            </Badge>
            <button
              className="ml-auto text-[10px] text-muted-foreground hover:text-destructive"
              onClick={(e) => {
                e.stopPropagation();
                clearHistory();
              }}
              title="Clear history"
            >
              <Trash2 className="w-3 h-3" />
            </button>
          </button>

          {showHistory && (
            <div className="space-y-1 max-h-40 overflow-auto">
              {commandHistory.map((entry, i) => (
                <button
                  key={`${entry.command}-${entry.timestamp}-${i}`}
                  className={`w-full p-2 text-left text-xs rounded transition-colors flex items-center gap-2 ${
                    entry.success
                      ? "bg-muted/10 hover:bg-muted/20"
                      : "bg-destructive/5 hover:bg-destructive/10"
                  }`}
                  onClick={() => handleLoadHistoryCommand(entry)}
                >
                  {entry.success ? (
                    <CheckCircle className="w-3 h-3 text-green-500 shrink-0" />
                  ) : (
                    <XCircle className="w-3 h-3 text-destructive shrink-0" />
                  )}
                  <span className="truncate flex-1">{entry.command}</span>
                  <span className="text-[10px] text-muted-foreground">
                    {new Date(entry.timestamp).toLocaleTimeString()}
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Example commands */}
      {!interpretation && (
        <div className="flex-1">
          <button
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground mb-2"
            onClick={() => setShowExamples(!showExamples)}
          >
            {showExamples ? (
              <ChevronDown className="w-3.5 h-3.5" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5" />
            )}
            Example Commands
          </button>

          {showExamples && (
            <div className="space-y-1">
              {EXAMPLE_COMMANDS.map((example, i) => (
                <button
                  key={`${example}-${i}`}
                  className="w-full p-2 text-left text-sm bg-muted/10 hover:bg-muted/20 rounded transition-colors"
                  onClick={() => handleLoadExample(example)}
                >
                  <span className="text-muted-foreground">"</span>
                  {example}
                  <span className="text-muted-foreground">"</span>
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Help section */}
      <div className="mt-auto pt-4 border-t border-border/30">
        <div className="text-xs text-muted-foreground space-y-1">
          <p className="font-medium">Supported Actions:</p>
          <div className="flex flex-wrap gap-2">
            {ACTION_PATTERNS.slice(0, 6).map((pattern) => (
              <span key={pattern.action} className="flex items-center gap-1">
                {pattern.icon}
                {pattern.label}
              </span>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

export default NaturalLanguagePanel;
