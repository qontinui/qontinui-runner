/**
 * Element Description Panel
 *
 * Displays AI-generated or heuristic descriptions for UI elements.
 * Part of Phase 2 of the AI-Native UI Bridge enhancements.
 *
 * Features:
 * - Heuristic descriptions (always available)
 * - AI-generated descriptions (when AI is configured)
 * - Element aliases for search
 * - Page summary
 */

import { useState, useCallback, useMemo, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import {
  Sparkles,
  RefreshCw,
  Copy,
  Check,
  FileText,
  Tag,
  Zap,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  Bot,
  Lightbulb,
  Hash,
  Type,
  MousePointer,
  Eye,
} from "lucide-react";
import type { ExternalElement, PageContext } from "../../hooks/useExternalUIBridge";

interface ElementDescriptionPanelProps {
  elements: ExternalElement[];
  selectedElement: ExternalElement | null;
  pageContext: PageContext | null;
  onSelectElement: (elementId: string) => void;
  disabled?: boolean;
}

interface ElementDescription {
  elementId: string;
  description: string;
  purpose: string;
  aliases: string[];
  suggestedActions: string[];
  source: "heuristic" | "ai";
  generatedAt: number;
}

interface PageSummary {
  title: string;
  description: string;
  mainActions: string[];
  source: "heuristic" | "ai";
  generatedAt: number;
}

/**
 * Generate a heuristic description for an element based on its attributes
 */
function generateHeuristicDescription(element: ExternalElement): ElementDescription {
  const parts: string[] = [];
  const aliases: string[] = [];
  let purpose = "";

  // Add the ID as an alias
  aliases.push(element.id);

  // Describe based on type
  switch (element.type) {
    case "button":
      parts.push("A button");
      if (element.label || element.text) {
        const text = element.label || element.text;
        parts.push(`labeled "${text}"`);
        aliases.push(text!.toLowerCase());
        // Extract words for aliases
        text!.split(/\s+/).forEach((word) => {
          if (word.length > 2) aliases.push(word.toLowerCase());
        });
      }
      purpose = "Trigger an action when clicked";
      break;

    case "input":
      parts.push("A text input field");
      if (element.label) {
        parts.push(`for "${element.label}"`);
        aliases.push(element.label.toLowerCase());
      }
      if (element.placeholder) {
        parts.push(`with placeholder "${element.placeholder}"`);
        aliases.push(element.placeholder.toLowerCase());
      }
      purpose = "Accept user text input";
      break;

    case "textarea":
      parts.push("A multi-line text area");
      if (element.label) {
        parts.push(`for "${element.label}"`);
        aliases.push(element.label.toLowerCase());
      }
      purpose = "Accept longer text input";
      break;

    case "select":
      parts.push("A dropdown select menu");
      if (element.label) {
        parts.push(`for "${element.label}"`);
        aliases.push(element.label.toLowerCase());
      }
      purpose = "Select from predefined options";
      break;

    case "checkbox":
      parts.push("A checkbox");
      if (element.label || element.text) {
        const text = element.label || element.text;
        parts.push(`for "${text}"`);
        aliases.push(text!.toLowerCase());
      }
      purpose = "Toggle a boolean option";
      break;

    case "radio":
      parts.push("A radio button option");
      if (element.label || element.text) {
        const text = element.label || element.text;
        parts.push(`for "${text}"`);
        aliases.push(text!.toLowerCase());
      }
      purpose = "Select one option from a group";
      break;

    case "link":
      parts.push("A link");
      if (element.text) {
        parts.push(`labeled "${element.text}"`);
        aliases.push(element.text.toLowerCase());
      }
      purpose = "Navigate to another page or section";
      break;

    case "tab":
      parts.push("A tab");
      if (element.label || element.text) {
        const text = element.label || element.text;
        parts.push(`labeled "${text}"`);
        aliases.push(text!.toLowerCase());
      }
      purpose = "Switch between content sections";
      break;

    default:
      parts.push(`A ${element.type} element`);
      if (element.label || element.text) {
        const text = element.label || element.text;
        parts.push(`with text "${text}"`);
        aliases.push(text!.toLowerCase());
      }
      purpose = "Interactive element";
  }

  // Add visibility info
  if (!element.visible) {
    parts.push("(currently hidden)");
  }
  if (!element.enabled) {
    parts.push("(currently disabled)");
  }

  // Generate suggested actions
  const suggestedActions = element.actions.slice(0, 3);

  // Deduplicate aliases
  const uniqueAliases = Array.from(new Set(aliases.filter((a) => a.length > 2)));

  return {
    elementId: element.id,
    description: parts.join(" "),
    purpose,
    aliases: uniqueAliases,
    suggestedActions,
    source: "heuristic",
    generatedAt: Date.now(),
  };
}

/**
 * Generate a heuristic page summary based on elements
 */
function generateHeuristicPageSummary(
  pageContext: PageContext,
  elements: ExternalElement[]
): PageSummary {
  const parts: string[] = [];
  const mainActions: string[] = [];

  // Use page title
  if (pageContext.title) {
    parts.push(`Page titled "${pageContext.title}".`);
  }

  // Count element types
  const typeCounts: Record<string, number> = {};
  elements.forEach((el) => {
    typeCounts[el.type] = (typeCounts[el.type] || 0) + 1;
  });

  // Describe composition
  const composition: string[] = [];
  if (typeCounts.button) composition.push(`${typeCounts.button} button(s)`);
  if (typeCounts.input) composition.push(`${typeCounts.input} input field(s)`);
  if (typeCounts.link) composition.push(`${typeCounts.link} link(s)`);
  if (typeCounts.tab) composition.push(`${typeCounts.tab} tab(s)`);

  if (composition.length > 0) {
    parts.push(`Contains ${composition.join(", ")}.`);
  }

  // Find main actions (buttons with prominent labels)
  const buttons = elements.filter((el) => el.type === "button" && el.visible);
  buttons.slice(0, 5).forEach((btn) => {
    if (btn.label || btn.text) {
      mainActions.push(btn.label || btn.text || btn.id);
    }
  });

  if (mainActions.length > 0) {
    parts.push(`Main actions: ${mainActions.join(", ")}.`);
  }

  return {
    title: pageContext.title || "Untitled Page",
    description: parts.join(" "),
    mainActions,
    source: "heuristic",
    generatedAt: Date.now(),
  };
}

export function ElementDescriptionPanel({
  elements,
  selectedElement,
  pageContext,
  onSelectElement,
  disabled = false,
}: ElementDescriptionPanelProps) {
  const [descriptions, setDescriptions] = useState<Map<string, ElementDescription>>(
    new Map()
  );
  const [pageSummary, setPageSummary] = useState<PageSummary | null>(null);
  const [isGeneratingAi, setIsGeneratingAi] = useState(false);
  const [aiAvailable, setAiAvailable] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);
  const [expandedSections, setExpandedSections] = useState<Set<string>>(
    new Set(["page-summary", "selected-element"])
  );
  const [copiedAlias, setCopiedAlias] = useState<string | null>(null);

  // Check if AI is available on mount
  useEffect(() => {
    const checkAi = async () => {
      try {
        // Check if AI settings are configured
        const result = await invoke<{ success: boolean; data?: { provider?: string } }>(
          "get_ai_settings"
        );
        setAiAvailable(result.success && !!result.data?.provider);
      } catch {
        setAiAvailable(false);
      }
    };
    checkAi();
  }, []);

  // Generate heuristic description for selected element
  const selectedDescription = useMemo(() => {
    if (!selectedElement) return null;

    // Check cache first
    const cached = descriptions.get(selectedElement.id);
    if (cached) return cached;

    // Generate heuristic
    return generateHeuristicDescription(selectedElement);
  }, [selectedElement, descriptions]);

  // Generate page summary when context changes
  useEffect(() => {
    if (pageContext && elements.length > 0) {
      setPageSummary(generateHeuristicPageSummary(pageContext, elements));
    } else {
      setPageSummary(null);
    }
  }, [pageContext, elements]);

  // Toggle section expansion
  const toggleSection = useCallback((section: string) => {
    setExpandedSections((prev) => {
      const next = new Set(prev);
      if (next.has(section)) {
        next.delete(section);
      } else {
        next.add(section);
      }
      return next;
    });
  }, []);

  // Copy alias to clipboard
  const handleCopyAlias = useCallback(async (alias: string) => {
    try {
      await navigator.clipboard.writeText(alias);
      setCopiedAlias(alias);
      setTimeout(() => setCopiedAlias(null), 2000);
    } catch {
      // Ignore errors
    }
  }, []);

  // Generate AI description (placeholder for future implementation)
  const handleGenerateAiDescription = useCallback(async () => {
    if (!selectedElement || !aiAvailable) return;

    setIsGeneratingAi(true);
    setAiError(null);

    try {
      // TODO: Implement actual AI description generation
      // For now, simulate with enhanced heuristic
      await new Promise((resolve) => setTimeout(resolve, 1000));

      const heuristic = generateHeuristicDescription(selectedElement);
      const aiEnhanced: ElementDescription = {
        ...heuristic,
        description: `[AI Enhanced] ${heuristic.description}`,
        source: "ai",
        generatedAt: Date.now(),
      };

      setDescriptions((prev) => {
        const next = new Map(prev);
        next.set(selectedElement.id, aiEnhanced);
        return next;
      });
    } catch (err) {
      setAiError(err instanceof Error ? err.message : "Failed to generate AI description");
    } finally {
      setIsGeneratingAi(false);
    }
  }, [selectedElement, aiAvailable]);

  // Render element type icon
  const renderTypeIcon = (type: string) => {
    switch (type) {
      case "button":
        return <MousePointer className="w-4 h-4" />;
      case "input":
      case "textarea":
        return <Type className="w-4 h-4" />;
      case "link":
        return <Zap className="w-4 h-4" />;
      default:
        return <Hash className="w-4 h-4" />;
    }
  };

  if (disabled || elements.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <Bot className="w-8 h-8 opacity-50" />
        <p>Connect to a browser tab to view descriptions</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full overflow-auto">
      {/* Page Summary Section */}
      {pageSummary && (
        <div className="mb-4">
          <button
            className="w-full flex items-center gap-2 p-2 bg-muted/20 hover:bg-muted/30 rounded-lg transition-colors"
            onClick={() => toggleSection("page-summary")}
          >
            {expandedSections.has("page-summary") ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
            <FileText className="w-4 h-4 text-primary" />
            <span className="font-medium text-sm">Page Summary</span>
            <Badge
              variant={pageSummary.source === "ai" ? "default" : "muted"}
              className="text-[10px] ml-auto"
            >
              {pageSummary.source === "ai" ? "AI" : "Auto"}
            </Badge>
          </button>

          {expandedSections.has("page-summary") && (
            <div className="mt-2 p-3 bg-muted/10 rounded-lg border border-border/30">
              <h3 className="font-medium text-sm mb-2">{pageSummary.title}</h3>
              <p className="text-sm text-muted-foreground mb-3">{pageSummary.description}</p>

              {pageSummary.mainActions.length > 0 && (
                <div>
                  <span className="text-xs text-muted-foreground">Main Actions:</span>
                  <div className="flex flex-wrap gap-1 mt-1">
                    {pageSummary.mainActions.map((action, i) => (
                      <Badge key={i} variant="outline" className="text-xs">
                        {action}
                      </Badge>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {/* Selected Element Section */}
      <div className="mb-4">
        <button
          className="w-full flex items-center gap-2 p-2 bg-muted/20 hover:bg-muted/30 rounded-lg transition-colors"
          onClick={() => toggleSection("selected-element")}
        >
          {expandedSections.has("selected-element") ? (
            <ChevronDown className="w-4 h-4 text-muted-foreground" />
          ) : (
            <ChevronRight className="w-4 h-4 text-muted-foreground" />
          )}
          <Lightbulb className="w-4 h-4 text-amber-500" />
          <span className="font-medium text-sm">Element Description</span>
          {selectedElement && (
            <Badge variant="muted" className="text-[10px] ml-auto">
              {selectedElement.type}
            </Badge>
          )}
        </button>

        {expandedSections.has("selected-element") && (
          <div className="mt-2 p-3 bg-muted/10 rounded-lg border border-border/30">
            {selectedElement && selectedDescription ? (
              <>
                {/* Element ID */}
                <div className="flex items-center gap-2 mb-3">
                  {renderTypeIcon(selectedElement.type)}
                  <span className="font-mono text-sm truncate flex-1">
                    {selectedElement.id}
                  </span>
                  <Badge
                    variant={selectedDescription.source === "ai" ? "default" : "muted"}
                    className="text-[10px]"
                  >
                    {selectedDescription.source === "ai" ? "AI" : "Auto"}
                  </Badge>
                </div>

                {/* Description */}
                <div className="mb-3">
                  <span className="text-xs text-muted-foreground">Description:</span>
                  <p className="text-sm mt-1">{selectedDescription.description}</p>
                </div>

                {/* Purpose */}
                <div className="mb-3">
                  <span className="text-xs text-muted-foreground">Purpose:</span>
                  <p className="text-sm mt-1 text-muted-foreground">
                    {selectedDescription.purpose}
                  </p>
                </div>

                {/* Aliases */}
                {selectedDescription.aliases.length > 0 && (
                  <div className="mb-3">
                    <span className="text-xs text-muted-foreground">
                      Aliases (for search):
                    </span>
                    <div className="flex flex-wrap gap-1 mt-1">
                      {selectedDescription.aliases.map((alias, i) => (
                        <button
                          key={i}
                          className="group flex items-center gap-1 px-2 py-0.5 text-xs bg-muted/30 hover:bg-muted/50 rounded transition-colors"
                          onClick={() => handleCopyAlias(alias)}
                          title="Click to copy"
                        >
                          <Tag className="w-3 h-3 text-muted-foreground" />
                          {alias}
                          {copiedAlias === alias ? (
                            <Check className="w-3 h-3 text-green-500" />
                          ) : (
                            <Copy className="w-3 h-3 text-muted-foreground opacity-0 group-hover:opacity-100" />
                          )}
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                {/* Suggested Actions */}
                {selectedDescription.suggestedActions.length > 0 && (
                  <div className="mb-3">
                    <span className="text-xs text-muted-foreground">Suggested Actions:</span>
                    <div className="flex flex-wrap gap-1 mt-1">
                      {selectedDescription.suggestedActions.map((action, i) => (
                        <Badge key={i} variant="outline" className="text-xs">
                          <Zap className="w-3 h-3 mr-1" />
                          {action}
                        </Badge>
                      ))}
                    </div>
                  </div>
                )}

                {/* AI Generation Button */}
                {aiAvailable && (
                  <div className="pt-2 border-t border-border/30">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={handleGenerateAiDescription}
                      disabled={isGeneratingAi}
                      className="w-full"
                    >
                      {isGeneratingAi ? (
                        <>
                          <RefreshCw className="w-3.5 h-3.5 mr-1 animate-spin" />
                          Generating...
                        </>
                      ) : (
                        <>
                          <Sparkles className="w-3.5 h-3.5 mr-1" />
                          Generate AI Description
                        </>
                      )}
                    </Button>
                    {aiError && (
                      <div className="mt-2 p-2 bg-destructive/10 border border-destructive/30 rounded text-xs text-destructive flex items-center gap-1">
                        <AlertCircle className="w-3 h-3" />
                        {aiError}
                      </div>
                    )}
                  </div>
                )}
              </>
            ) : (
              <div className="text-center py-4 text-muted-foreground text-sm">
                <Eye className="w-6 h-6 mx-auto mb-2 opacity-50" />
                <p>Select an element to view its description</p>
              </div>
            )}
          </div>
        )}
      </div>

      {/* All Elements Quick View */}
      <div>
        <button
          className="w-full flex items-center gap-2 p-2 bg-muted/20 hover:bg-muted/30 rounded-lg transition-colors"
          onClick={() => toggleSection("all-elements")}
        >
          {expandedSections.has("all-elements") ? (
            <ChevronDown className="w-4 h-4 text-muted-foreground" />
          ) : (
            <ChevronRight className="w-4 h-4 text-muted-foreground" />
          )}
          <Hash className="w-4 h-4 text-blue-500" />
          <span className="font-medium text-sm">All Elements</span>
          <Badge variant="muted" className="text-[10px] ml-auto">
            {elements.length}
          </Badge>
        </button>

        {expandedSections.has("all-elements") && (
          <div className="mt-2 space-y-1 max-h-64 overflow-auto">
            {elements.map((element) => {
              const desc =
                descriptions.get(element.id) || generateHeuristicDescription(element);
              const isSelected = selectedElement?.id === element.id;

              return (
                <button
                  key={element.id}
                  className={`w-full p-2 text-left rounded-md transition-colors ${
                    isSelected
                      ? "bg-primary/10 border border-primary/30"
                      : "bg-muted/10 hover:bg-muted/20 border border-transparent"
                  }`}
                  onClick={() => onSelectElement(element.id)}
                >
                  <div className="flex items-center gap-2">
                    {renderTypeIcon(element.type)}
                    <span className="font-mono text-xs truncate flex-1">{element.id}</span>
                    <Badge variant="muted" className="text-[10px]">
                      {element.type}
                    </Badge>
                  </div>
                  <p className="text-xs text-muted-foreground mt-1 truncate">
                    {desc.description}
                  </p>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {/* Help text */}
      <div className="mt-auto pt-4 border-t border-border/30">
        <div className="text-xs text-muted-foreground space-y-1">
          <p className="flex items-center gap-1">
            <Badge variant="muted" className="text-[10px]">
              Auto
            </Badge>
            Heuristic descriptions based on element attributes
          </p>
          {aiAvailable && (
            <p className="flex items-center gap-1">
              <Badge variant="default" className="text-[10px]">
                AI
              </Badge>
              AI-generated descriptions with enhanced context
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

export default ElementDescriptionPanel;
