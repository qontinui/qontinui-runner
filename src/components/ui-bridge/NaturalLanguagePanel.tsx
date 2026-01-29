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
 */

import { useState, useCallback, useEffect } from "react";
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
} from "lucide-react";
import type { ExternalElement, CommandResult } from "../../hooks/useExternalUIBridge";

interface NaturalLanguagePanelProps {
  elements: ExternalElement[];
  onExecuteAction: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>
  ) => Promise<CommandResult>;
  onSelectElement: (elementId: string) => void;
  onHighlightElement: (elementId: string) => void;
  disabled?: boolean;
}

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
}

interface InterpretationResult {
  intent: ParsedIntent;
  matches: ElementMatch[];
  bestMatch: ElementMatch | null;
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
    patterns: [
      /^focus\s+(?:on\s+)?(?:the\s+)?(.+)/i,
      /^go\s+to\s+(?:the\s+)?(.+)/i,
    ],
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
    patterns: [
      /^hover\s+(?:over\s+)?(?:the\s+)?(.+)/i,
      /^mouse\s+over\s+(?:the\s+)?(.+)/i,
    ],
    icon: <MousePointer className="w-4 h-4" />,
  },
];

/**
 * Parse natural language input to extract intent
 */
function parseIntent(input: string): ParsedIntent | null {
  const trimmed = input.trim();
  if (!trimmed) return null;

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
 */
function findMatchingElements(
  targetDescription: string,
  elements: ExternalElement[]
): ElementMatch[] {
  const matches: ElementMatch[] = [];
  const lowerTarget = targetDescription.toLowerCase();
  const targetWords = lowerTarget.split(/\s+/);

  for (const element of elements) {
    let score = 0;
    const reasons: string[] = [];

    // Check ID match
    if (element.id.toLowerCase().includes(lowerTarget)) {
      score += 0.8;
      reasons.push("ID contains target");
    } else {
      // Check word overlap with ID
      const idSimilarity = calculateSimilarity(element.id, lowerTarget);
      if (idSimilarity > 0.3) {
        score += idSimilarity * 0.6;
        reasons.push(`ID similarity: ${(idSimilarity * 100).toFixed(0)}%`);
      }
    }

    // Check label match
    if (element.label) {
      if (element.label.toLowerCase().includes(lowerTarget)) {
        score += 0.9;
        reasons.push("Label contains target");
      } else {
        const labelSimilarity = calculateSimilarity(element.label, lowerTarget);
        if (labelSimilarity > 0.3) {
          score += labelSimilarity * 0.7;
          reasons.push(`Label similarity: ${(labelSimilarity * 100).toFixed(0)}%`);
        }
      }
    }

    // Check text match
    if (element.text) {
      if (element.text.toLowerCase().includes(lowerTarget)) {
        score += 0.85;
        reasons.push("Text contains target");
      } else {
        const textSimilarity = calculateSimilarity(element.text, lowerTarget);
        if (textSimilarity > 0.3) {
          score += textSimilarity * 0.65;
          reasons.push(`Text similarity: ${(textSimilarity * 100).toFixed(0)}%`);
        }
      }
    }

    // Check type match (e.g., "button", "input")
    if (targetWords.includes(element.type)) {
      score += 0.3;
      reasons.push("Type matches");
    }

    // Check for specific element type keywords
    const typeKeywords: Record<string, string[]> = {
      button: ["button", "btn", "submit", "click"],
      input: ["input", "field", "textbox", "text field"],
      link: ["link", "anchor", "href"],
      checkbox: ["checkbox", "check box", "toggle"],
      select: ["dropdown", "select", "menu", "picker"],
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
  elements: ExternalElement[]
): InterpretationResult | null {
  const intent = parseIntent(input);
  if (!intent) return null;

  const matches = findMatchingElements(intent.targetDescription, elements);
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

  // Real-time interpretation as user types
  useEffect(() => {
    if (input.trim() && elements.length > 0) {
      const result = interpretCommand(input, elements);
      setInterpretation(result);
    } else {
      setInterpretation(null);
    }
  }, [input, elements]);

  // Execute the interpreted command
  const handleExecute = useCallback(async () => {
    if (!interpretation?.bestMatch || isExecuting) return;

    const { intent, bestMatch } = interpretation;
    setIsExecuting(true);
    setLastResult(null);

    try {
      const result = await onExecuteAction(
        bestMatch.element.id,
        intent.action,
        intent.params
      );

      setLastResult({
        success: result.success,
        command: input,
        elementId: bestMatch.element.id,
        error: result.error,
        timestamp: Date.now(),
      });

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
    } finally {
      setIsExecuting(false);
    }
  }, [interpretation, input, isExecuting, onExecuteAction]);

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
      } finally {
        setIsExecuting(false);
      }
    },
    [interpretation, input, isExecuting, onExecuteAction]
  );

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
              className="w-full pl-9 pr-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md focus:outline-none focus:ring-1 focus:ring-primary resize-none"
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
          {lastResult.error && (
            <p className="text-xs text-destructive mt-1">{lastResult.error}</p>
          )}
        </div>
      )}

      {/* Interpretation preview */}
      {interpretation && (
        <div className="mb-4 p-3 bg-muted/20 rounded-lg border border-border/30">
          <div className="flex items-center gap-2 mb-2">
            <Sparkles className="w-4 h-4 text-primary" />
            <span className="text-sm font-medium">Parsed Intent</span>
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
                      <p className="text-xs text-muted-foreground mt-0.5">
                        {match.matchReason}
                      </p>
                    </div>
                  ))}
                </div>
              )}
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
                  key={i}
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
