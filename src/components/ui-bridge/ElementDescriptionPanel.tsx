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
 * - Persistent storage of descriptions
 * - Bulk AI description generation
 */

import { useState, useCallback, useMemo, useEffect, useRef } from "react";
import { instanceStorage } from "@/lib/instance-storage";
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
  MapPin,
  GitBranch,
  ArrowRight,
  Code,
  Crosshair,
  Download,
  Upload,
  Trash2,
  Database,
  Loader2,
} from "lucide-react";
import {
  ElementDescriptionService,
  type PersistedElementDescription,
} from "../../services/element-description-service";
import type { ExternalElement, PageContext } from "../../types/ui-bridge-types";
import {
  buildDOMContext,
  getLandmarkDisplayName,
  type DOMContext,
  type PathSegment,
  type SiblingInfo,
} from "../../lib/ui-bridge/domContext";
import {
  getAllSelectors,
  generatePlaywrightCode,
  generatePuppeteerCode,
  generateCypressCode,
  generateQontinuiCode,
  generateSeleniumCode,
  type AutomationAction,
} from "../../lib/ui-bridge/selectorGenerator";
import { createLogger } from "@/lib/logger";

const logger = createLogger("ElementDescriptionPanel");

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
  /** How a user would interact with this element (AI-generated) */
  interaction?: string;
  /** Accessibility notes and considerations (AI-generated) */
  accessibilityNotes?: string | null;
}

/** Response from generate_element_ai_description Tauri command */
interface AiDescriptionResponse {
  description: string;
  purpose: string;
  interaction: string;
  accessibility_notes: string | null;
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
  let purpose: string;

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
  elements: ExternalElement[],
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
  const [descriptions, setDescriptions] = useState<Map<string, ElementDescription>>(new Map());
  const [pageSummary, setPageSummary] = useState<PageSummary | null>(null);
  const [isGeneratingAi, setIsGeneratingAi] = useState(false);
  const [isGeneratingBulk, setIsGeneratingBulk] = useState(false);
  const [bulkProgress, setBulkProgress] = useState<{ current: number; total: number } | null>(null);
  const [aiAvailable, setAiAvailable] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);
  const [expandedSections, setExpandedSections] = useState<Set<string>>(
    new Set(["page-summary", "selected-element"]),
  );
  const [copiedAlias, setCopiedAlias] = useState<string | null>(null);
  const [copiedSelector, setCopiedSelector] = useState<string | null>(null);
  const [copiedCode, setCopiedCode] = useState(false);
  const [storageStats, setStorageStats] = useState<{
    totalPages: number;
    totalDescriptions: number;
    aiDescriptions: number;
  } | null>(null);
  const [selectedFramework, setSelectedFramework] = useState<string>(() => {
    // Load from instanceStorage, default to Playwright
    if (typeof window !== "undefined") {
      return instanceStorage.getItem("qontinui-preferred-framework") || "playwright";
    }
    return "playwright";
  });

  // Track if we've loaded descriptions for the current page
  const loadedPageRef = useRef<string | null>(null);

  // Save framework preference to instanceStorage
  useEffect(() => {
    if (typeof window !== "undefined") {
      instanceStorage.setItem("qontinui-preferred-framework", selectedFramework);
    }
  }, [selectedFramework]);

  // Check if AI is available on mount
  useEffect(() => {
    const checkAi = async () => {
      try {
        // Check if AI settings are configured
        const result = await invoke<{ success: boolean; data?: { provider?: string } }>(
          "get_ai_settings",
        );
        setAiAvailable(result.success && !!result.data?.provider);
      } catch {
        setAiAvailable(false);
      }
    };
    checkAi();

    // Load storage stats
    const stats = ElementDescriptionService.getStats();
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setStorageStats({
      totalPages: stats.totalPages,
      totalDescriptions: stats.totalDescriptions,
      aiDescriptions: stats.aiDescriptions,
    });
  }, []);

  // Load persisted descriptions when page context changes
  useEffect(() => {
    if (!pageContext?.url) {
      loadedPageRef.current = null;
      return;
    }

    // Only load if we haven't loaded for this page yet
    if (loadedPageRef.current === pageContext.url) {
      return;
    }

    loadedPageRef.current = pageContext.url;

    // Load from persistence
    const persisted = ElementDescriptionService.loadDescriptions(pageContext.url);
    if (persisted.size > 0) {
      // Convert to ElementDescription format
      const loaded = new Map<string, ElementDescription>();
      for (const [elementId, desc] of persisted) {
        loaded.set(elementId, {
          elementId: desc.elementId,
          description: desc.description,
          purpose: desc.purpose,
          aliases: desc.aliases,
          suggestedActions: desc.suggestedActions,
          source: desc.source,
          generatedAt: desc.generatedAt,
          interaction: desc.interaction,
          accessibilityNotes: desc.accessibilityNotes,
        });
      }
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setDescriptions(loaded);
      logger.debug(`Loaded ${loaded.size} persisted descriptions`);
    }
  }, [pageContext?.url]);

  // Generate heuristic description for selected element
  const selectedDescription = useMemo(() => {
    if (!selectedElement) return null;

    // Check cache first
    const cached = descriptions.get(selectedElement.id);
    if (cached) return cached;

    // Generate heuristic
    return generateHeuristicDescription(selectedElement);
  }, [selectedElement, descriptions]);

  // Build DOM context for selected element
  const domContext: DOMContext | null = useMemo(() => {
    if (!selectedElement) return null;
    return buildDOMContext(selectedElement, elements);
  }, [selectedElement, elements]);

  // Generate page summary when context changes
  useEffect(() => {
    if (pageContext && elements.length > 0) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
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

  // Copy selector to clipboard
  const handleCopySelector = useCallback(async (selector: string) => {
    try {
      await navigator.clipboard.writeText(selector);
      setCopiedSelector(selector);
      setTimeout(() => setCopiedSelector(null), 2000);
    } catch {
      // Ignore errors
    }
  }, []);

  // Copy code snippet to clipboard
  const handleCopyCode = useCallback(async (code: string) => {
    try {
      await navigator.clipboard.writeText(code);
      setCopiedCode(true);
      setTimeout(() => setCopiedCode(false), 2000);
    } catch {
      // Ignore errors
    }
  }, []);

  // Generate selectors for selected element
  const elementSelectors = useMemo(() => {
    if (!selectedElement) return [];
    return getAllSelectors(selectedElement);
  }, [selectedElement]);

  // Get current action based on element type
  const getDefaultAction = useCallback((element: ExternalElement): AutomationAction => {
    switch (element.type) {
      case "input":
      case "textarea":
        return "fill";
      case "checkbox":
      case "radio":
        return "check";
      case "select":
        return "select";
      default:
        return "click";
    }
  }, []);

  // Generate code snippet for selected framework
  const currentCodeSnippet = useMemo(() => {
    if (!selectedElement) return null;

    const action = getDefaultAction(selectedElement);

    switch (selectedFramework) {
      case "playwright":
        return generatePlaywrightCode(selectedElement, action);
      case "puppeteer":
        return generatePuppeteerCode(selectedElement, action);
      case "cypress":
        return generateCypressCode(selectedElement, action);
      case "qontinui":
        return generateQontinuiCode(selectedElement, action);
      case "selenium":
        return generateSeleniumCode(selectedElement, action);
      default:
        return generatePlaywrightCode(selectedElement, action);
    }
  }, [selectedElement, selectedFramework, getDefaultAction]);

  // Generate AI description using the Tauri command
  const handleGenerateAiDescription = useCallback(async () => {
    if (!selectedElement || !aiAvailable) return;

    setIsGeneratingAi(true);
    setAiError(null);

    try {
      // Build element states list
      const states: string[] = [];
      if (selectedElement.enabled === false) states.push("disabled");
      if (selectedElement.enabled === true) states.push("enabled");
      if (selectedElement.focused) states.push("focused");
      if (selectedElement.visible === false) states.push("hidden");
      if (selectedElement.is_required) states.push("required");
      if (selectedElement.is_readonly) states.push("readonly");
      if (selectedElement.is_expanded !== undefined) {
        states.push(selectedElement.is_expanded ? "expanded" : "collapsed");
      }
      if (selectedElement.is_selected) states.push("selected");
      if (selectedElement.checked !== undefined) {
        states.push(selectedElement.checked ? "checked" : "unchecked");
      }

      // Get parent info from domContext
      const parentInfo = domContext?.parent
        ? {
            element_type: domContext.parent.type,
            role: domContext.parent.role || undefined,
            label: domContext.parent.label || undefined,
          }
        : undefined;

      // Call the Tauri command
      const result = await invoke<{
        success: boolean;
        message?: string;
        data?: AiDescriptionResponse;
      }>("generate_element_ai_description", {
        input: {
          element_type: selectedElement.type,
          role: selectedElement.role || selectedElement.accessibility?.role || undefined,
          label: selectedElement.label || selectedElement.accessibleName || undefined,
          text: selectedElement.text || undefined,
          value: selectedElement.value || undefined,
          placeholder: selectedElement.placeholder || undefined,
          states,
          actions: selectedElement.actions || [],
          parent_info: parentInfo,
          page_context: pageContext
            ? {
                url: pageContext.url || undefined,
                title: pageContext.title || undefined,
              }
            : undefined,
        },
      });

      if (!result.success || !result.data) {
        throw new Error(result.message || "Failed to generate AI description");
      }

      // Create the AI-enhanced description
      const heuristic = generateHeuristicDescription(selectedElement);
      const aiEnhanced: ElementDescription = {
        elementId: selectedElement.id,
        description: result.data.description,
        purpose: result.data.purpose,
        aliases: heuristic.aliases, // Keep the heuristic aliases
        suggestedActions: heuristic.suggestedActions,
        source: "ai",
        generatedAt: Date.now(),
        interaction: result.data.interaction,
        accessibilityNotes: result.data.accessibility_notes,
      };

      setDescriptions((prev) => {
        const next = new Map(prev);
        next.set(selectedElement.id, aiEnhanced);
        return next;
      });

      // Persist to storage
      if (pageContext?.url) {
        ElementDescriptionService.saveDescription(
          pageContext.url,
          pageContext.title || "Untitled",
          aiEnhanced as PersistedElementDescription,
        );
        // Update stats
        const stats = ElementDescriptionService.getStats();
        setStorageStats({
          totalPages: stats.totalPages,
          totalDescriptions: stats.totalDescriptions,
          aiDescriptions: stats.aiDescriptions,
        });
      }
    } catch (err) {
      setAiError(err instanceof Error ? err.message : "Failed to generate AI description");
    } finally {
      setIsGeneratingAi(false);
    }
  }, [selectedElement, aiAvailable, domContext, pageContext]);

  // Generate AI descriptions for all interactive elements
  const handleBulkGenerateAi = useCallback(async () => {
    if (!aiAvailable || !pageContext?.url || isGeneratingBulk) return;

    // Filter to interactive elements that don't have AI descriptions yet
    const interactiveTypes = [
      "button",
      "input",
      "textarea",
      "select",
      "checkbox",
      "radio",
      "link",
      "tab",
    ];
    const toGenerate = elements.filter(
      (el) =>
        interactiveTypes.includes(el.type) &&
        el.visible &&
        (!descriptions.get(el.id) || descriptions.get(el.id)?.source !== "ai"),
    );

    if (toGenerate.length === 0) {
      setAiError("All interactive elements already have AI descriptions");
      return;
    }

    setIsGeneratingBulk(true);
    setBulkProgress({ current: 0, total: toGenerate.length });
    setAiError(null);

    const newDescriptions = new Map(descriptions);
    let successCount = 0;

    for (let i = 0; i < toGenerate.length; i++) {
      const element = toGenerate[i];
      setBulkProgress({ current: i + 1, total: toGenerate.length });

      try {
        // Build element states list
        const states: string[] = [];
        if (element.enabled === false) states.push("disabled");
        if (element.enabled === true) states.push("enabled");
        if (element.focused) states.push("focused");
        if (element.visible === false) states.push("hidden");
        if (element.is_required) states.push("required");
        if (element.is_readonly) states.push("readonly");
        if (element.is_expanded !== undefined) {
          states.push(element.is_expanded ? "expanded" : "collapsed");
        }
        if (element.is_selected) states.push("selected");
        if (element.checked !== undefined) {
          states.push(element.checked ? "checked" : "unchecked");
        }

        // Call the Tauri command
        const result = await invoke<{
          success: boolean;
          message?: string;
          data?: AiDescriptionResponse;
        }>("generate_element_ai_description", {
          input: {
            element_type: element.type,
            role: element.role || element.accessibility?.role || undefined,
            label: element.label || element.accessibleName || undefined,
            text: element.text || undefined,
            value: element.value || undefined,
            placeholder: element.placeholder || undefined,
            states,
            actions: element.actions || [],
            parent_info: undefined,
            page_context: pageContext
              ? {
                  url: pageContext.url || undefined,
                  title: pageContext.title || undefined,
                }
              : undefined,
          },
        });

        if (result.success && result.data) {
          const heuristic = generateHeuristicDescription(element);
          const aiEnhanced: ElementDescription = {
            elementId: element.id,
            description: result.data.description,
            purpose: result.data.purpose,
            aliases: heuristic.aliases,
            suggestedActions: heuristic.suggestedActions,
            source: "ai",
            generatedAt: Date.now(),
            interaction: result.data.interaction,
            accessibilityNotes: result.data.accessibility_notes,
          };
          newDescriptions.set(element.id, aiEnhanced);
          successCount++;
        }
      } catch (err) {
        console.warn(`[BulkGenerate] Failed for ${element.id}:`, err);
        // Continue with next element
      }

      // Small delay to avoid rate limiting
      if (i < toGenerate.length - 1) {
        await new Promise((r) => setTimeout(r, 200));
      }
    }

    // Update state with all new descriptions
    setDescriptions(newDescriptions);

    // Persist all to storage
    if (pageContext?.url) {
      const toPersist = new Map<string, PersistedElementDescription>();
      for (const [id, desc] of newDescriptions) {
        if (desc.source === "ai") {
          toPersist.set(id, desc as PersistedElementDescription);
        }
      }
      ElementDescriptionService.saveDescriptions(
        pageContext.url,
        pageContext.title || "Untitled",
        toPersist,
      );

      // Update stats
      const stats = ElementDescriptionService.getStats();
      setStorageStats({
        totalPages: stats.totalPages,
        totalDescriptions: stats.totalDescriptions,
        aiDescriptions: stats.aiDescriptions,
      });
    }

    setIsGeneratingBulk(false);
    setBulkProgress(null);

    if (successCount < toGenerate.length) {
      setAiError(`Generated ${successCount}/${toGenerate.length} descriptions (some failed)`);
    }
  }, [elements, descriptions, aiAvailable, pageContext, isGeneratingBulk]);

  // Export descriptions
  const handleExport = useCallback(() => {
    const json = ElementDescriptionService.exportAll();
    const blob = new Blob([json], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `element-descriptions-${new Date().toISOString().split("T")[0]}.json`;
    a.click();
    URL.revokeObjectURL(url);
  }, []);

  // Import descriptions
  const handleImport = useCallback(() => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".json";
    input.onchange = async (e) => {
      const file = (e.target as HTMLInputElement).files?.[0];
      if (!file) return;

      try {
        const text = await file.text();
        ElementDescriptionService.importFromJson(text, true);

        // Reload descriptions for current page
        if (pageContext?.url) {
          loadedPageRef.current = null; // Force reload
          const persisted = ElementDescriptionService.loadDescriptions(pageContext.url);
          const loaded = new Map<string, ElementDescription>();
          for (const [elementId, desc] of persisted) {
            loaded.set(elementId, desc);
          }
          setDescriptions(loaded);
        }

        // Update stats
        const stats = ElementDescriptionService.getStats();
        setStorageStats({
          totalPages: stats.totalPages,
          totalDescriptions: stats.totalDescriptions,
          aiDescriptions: stats.aiDescriptions,
        });
      } catch (err) {
        setAiError(err instanceof Error ? err.message : "Failed to import descriptions");
      }
    };
    input.click();
  }, [pageContext]);

  // Clear descriptions for current page
  const handleClearPage = useCallback(() => {
    if (!pageContext?.url) return;

    if (window.confirm("Clear all descriptions for this page?")) {
      ElementDescriptionService.clearPage(pageContext.url);
      setDescriptions(new Map());
      loadedPageRef.current = null;

      // Update stats
      const stats = ElementDescriptionService.getStats();
      setStorageStats({
        totalPages: stats.totalPages,
        totalDescriptions: stats.totalDescriptions,
        aiDescriptions: stats.aiDescriptions,
      });
    }
  }, [pageContext]);

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
                      <Badge key={`${action}-${i}`} variant="outline" className="text-xs">
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
                  <span className="font-mono text-sm truncate flex-1">{selectedElement.id}</span>
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
                    <span className="text-xs text-muted-foreground">Aliases (for search):</span>
                    <div className="flex flex-wrap gap-1 mt-1">
                      {selectedDescription.aliases.map((alias, i) => (
                        <button
                          key={`${alias}-${i}`}
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
                        <Badge key={`${action}-${i}`} variant="outline" className="text-xs">
                          <Zap className="w-3 h-3 mr-1" />
                          {action}
                        </Badge>
                      ))}
                    </div>
                  </div>
                )}

                {/* AI-specific fields: Interaction and Accessibility Notes */}
                {selectedDescription.source === "ai" && (
                  <>
                    {/* Interaction */}
                    {selectedDescription.interaction && (
                      <div className="mb-3">
                        <span className="text-xs text-muted-foreground">How to Interact:</span>
                        <p className="text-sm mt-1 text-muted-foreground">
                          {selectedDescription.interaction}
                        </p>
                      </div>
                    )}

                    {/* Accessibility Notes */}
                    {selectedDescription.accessibilityNotes && (
                      <div className="mb-3 p-2 bg-amber-500/10 border border-amber-500/20 rounded">
                        <span className="text-xs text-amber-600 dark:text-amber-400 font-medium flex items-center gap-1">
                          <AlertCircle className="w-3 h-3" />
                          Accessibility Note:
                        </span>
                        <p className="text-sm mt-1 text-amber-700 dark:text-amber-300">
                          {selectedDescription.accessibilityNotes}
                        </p>
                      </div>
                    )}
                  </>
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

      {/* Selectors Section */}
      {selectedElement && (
        <div className="mb-4">
          <button
            className="w-full flex items-center gap-2 p-2 bg-muted/20 hover:bg-muted/30 rounded-lg transition-colors"
            onClick={() => toggleSection("selectors")}
          >
            {expandedSections.has("selectors") ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
            <Crosshair className="w-4 h-4 text-cyan-500" />
            <span className="font-medium text-sm">Selectors</span>
            <Badge variant="muted" className="text-[10px] ml-auto">
              {elementSelectors.length}
            </Badge>
          </button>

          {expandedSections.has("selectors") && (
            <div className="mt-2 p-3 bg-muted/10 rounded-lg border border-border/30 space-y-2">
              {elementSelectors.map((selectorResult, index) => (
                <div
                  key={`${selectorResult.type}-${index}`}
                  className="group flex items-start gap-2 p-2 bg-muted/20 rounded-md hover:bg-muted/30 transition-colors"
                >
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 mb-1">
                      <Badge
                        variant={
                          selectorResult.type === "testId"
                            ? "success"
                            : selectorResult.type === "accessible"
                              ? "info"
                              : selectorResult.type === "css"
                                ? "purple"
                                : "muted"
                        }
                        size="sm"
                        className="uppercase text-[9px] font-bold"
                      >
                        {selectorResult.type}
                      </Badge>
                      <span className="text-[10px] text-muted-foreground">
                        {selectorResult.description}
                      </span>
                      {selectorResult.isLikelyUnique && (
                        <Badge variant="outline" size="sm" className="text-[9px]">
                          unique
                        </Badge>
                      )}
                    </div>
                    <code className="block text-xs font-mono text-foreground break-all bg-muted/30 p-1.5 rounded">
                      {selectorResult.selector}
                    </code>
                    <div className="flex items-center gap-2 mt-1">
                      <div className="flex-1 h-1 bg-muted/30 rounded-full overflow-hidden">
                        <div
                          className={`h-full transition-all ${
                            selectorResult.reliability >= 80
                              ? "bg-green-500"
                              : selectorResult.reliability >= 50
                                ? "bg-yellow-500"
                                : "bg-red-500"
                          }`}
                          style={{ width: `${selectorResult.reliability}%` }}
                        />
                      </div>
                      <span className="text-[10px] text-muted-foreground">
                        {selectorResult.reliability}%
                      </span>
                    </div>
                  </div>
                  <button
                    className="p-1.5 rounded hover:bg-muted/50 transition-colors opacity-0 group-hover:opacity-100"
                    onClick={() => handleCopySelector(selectorResult.selector)}
                    title="Copy selector"
                  >
                    {copiedSelector === selectorResult.selector ? (
                      <Check className="w-3.5 h-3.5 text-green-500" />
                    ) : (
                      <Copy className="w-3.5 h-3.5 text-muted-foreground" />
                    )}
                  </button>
                </div>
              ))}
              {elementSelectors.length === 0 && (
                <div className="text-center py-2 text-muted-foreground text-xs">
                  No selectors available for this element
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {/* Code Snippets Section */}
      {selectedElement && (
        <div className="mb-4">
          <button
            className="w-full flex items-center gap-2 p-2 bg-muted/20 hover:bg-muted/30 rounded-lg transition-colors"
            onClick={() => toggleSection("code-snippets")}
          >
            {expandedSections.has("code-snippets") ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
            <Code className="w-4 h-4 text-green-500" />
            <span className="font-medium text-sm">Code Snippets</span>
          </button>

          {expandedSections.has("code-snippets") && (
            <div className="mt-2 p-3 bg-muted/10 rounded-lg border border-border/30">
              {/* Framework Tabs */}
              <div className="flex flex-wrap gap-1 mb-3">
                {[
                  { id: "playwright", label: "Playwright" },
                  { id: "puppeteer", label: "Puppeteer" },
                  { id: "cypress", label: "Cypress" },
                  { id: "selenium", label: "Selenium" },
                  { id: "qontinui", label: "Qontinui" },
                ].map((fw) => (
                  <button
                    key={fw.id}
                    className={`px-2.5 py-1 text-xs rounded-md transition-colors ${
                      selectedFramework === fw.id
                        ? "bg-primary text-primary-foreground"
                        : "bg-muted/30 hover:bg-muted/50 text-muted-foreground"
                    }`}
                    onClick={() => setSelectedFramework(fw.id)}
                  >
                    {fw.label}
                  </button>
                ))}
              </div>

              {/* Code Display */}
              {currentCodeSnippet && (
                <div className="relative">
                  <div className="flex items-center justify-between mb-1">
                    <Badge variant="muted" size="sm" className="text-[9px]">
                      {currentCodeSnippet.language}
                    </Badge>
                    <button
                      className="p-1 rounded hover:bg-muted/50 transition-colors"
                      onClick={() => handleCopyCode(currentCodeSnippet.code)}
                      title="Copy code"
                    >
                      {copiedCode ? (
                        <Check className="w-3.5 h-3.5 text-green-500" />
                      ) : (
                        <Copy className="w-3.5 h-3.5 text-muted-foreground" />
                      )}
                    </button>
                  </div>
                  <pre className="text-xs font-mono bg-muted/30 p-3 rounded-md overflow-x-auto whitespace-pre-wrap">
                    {currentCodeSnippet.code}
                  </pre>
                </div>
              )}

              {/* Hint */}
              <p className="text-[10px] text-muted-foreground mt-2">
                Action: {getDefaultAction(selectedElement)} (based on element type)
              </p>
            </div>
          )}
        </div>
      )}

      {/* DOM Context Section */}
      {selectedElement && domContext && (
        <div className="mb-4">
          <button
            className="w-full flex items-center gap-2 p-2 bg-muted/20 hover:bg-muted/30 rounded-lg transition-colors"
            onClick={() => toggleSection("dom-context")}
          >
            {expandedSections.has("dom-context") ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
            <GitBranch className="w-4 h-4 text-purple-500" />
            <span className="font-medium text-sm">DOM Context</span>
            {domContext.landmark && (
              <Badge variant="muted" className="text-[10px] ml-auto">
                {getLandmarkDisplayName(domContext.landmark.role)}
              </Badge>
            )}
          </button>

          {expandedSections.has("dom-context") && (
            <div className="mt-2 p-3 bg-muted/10 rounded-lg border border-border/30 space-y-4">
              {/* Path Breadcrumb */}
              {domContext.path.length > 0 && (
                <div>
                  <span className="text-xs text-muted-foreground flex items-center gap-1 mb-2">
                    <MapPin className="w-3 h-3" />
                    Element Path
                  </span>
                  <div className="flex flex-wrap items-center gap-1">
                    {domContext.path.map((segment: PathSegment, index: number) => (
                      <span
                        key={`${segment.elementId ?? segment.tagName}-${index}`}
                        className="flex items-center gap-1"
                      >
                        {index > 0 && <ArrowRight className="w-3 h-3 text-muted-foreground/50" />}
                        {segment.elementId ? (
                          <button
                            className={`px-1.5 py-0.5 text-xs rounded transition-colors ${
                              segment.isCurrent
                                ? "bg-primary/20 text-primary font-medium border border-primary/30"
                                : "bg-muted/30 hover:bg-muted/50 text-muted-foreground"
                            }`}
                            onClick={() => segment.elementId && onSelectElement(segment.elementId)}
                            title={
                              segment.label
                                ? `${segment.tagName}: ${segment.label}`
                                : segment.tagName
                            }
                          >
                            {segment.tagName}
                            {segment.role && segment.role !== segment.tagName && (
                              <span className="text-[10px] opacity-70 ml-0.5">
                                [{segment.role}]
                              </span>
                            )}
                          </button>
                        ) : (
                          <span
                            className="px-1.5 py-0.5 text-xs text-muted-foreground/60 italic"
                            title="Parent not in element list"
                          >
                            {segment.tagName}
                          </span>
                        )}
                      </span>
                    ))}
                  </div>
                </div>
              )}

              {/* Parent Info */}
              {domContext.parent && (
                <div>
                  <span className="text-xs text-muted-foreground mb-1.5 block">Parent Element</span>
                  <button
                    className="flex items-center gap-2 px-2 py-1.5 bg-muted/30 hover:bg-muted/50 rounded-md transition-colors w-full text-left"
                    onClick={() =>
                      domContext.parent && onSelectElement(domContext.parent.elementId)
                    }
                  >
                    {renderTypeIcon(domContext.parent.type)}
                    <span className="text-xs font-mono truncate flex-1">
                      {domContext.parent.tagName}
                      {domContext.parent.role &&
                        domContext.parent.role !== domContext.parent.tagName && (
                          <span className="text-muted-foreground ml-1">
                            [{domContext.parent.role}]
                          </span>
                        )}
                    </span>
                    {domContext.parent.label && (
                      <span className="text-xs text-muted-foreground truncate max-w-[120px]">
                        "{domContext.parent.label}"
                      </span>
                    )}
                  </button>
                </div>
              )}

              {/* Landmark Context */}
              {domContext.landmark && (
                <div>
                  <span className="text-xs text-muted-foreground mb-1.5 block">
                    Landmark Context
                  </span>
                  <div className="flex items-center gap-2 px-2 py-1.5 bg-purple-500/10 border border-purple-500/20 rounded-md">
                    <MapPin className="w-3.5 h-3.5 text-purple-500" />
                    <span className="text-xs font-medium text-purple-600 dark:text-purple-400">
                      {getLandmarkDisplayName(domContext.landmark.role)}
                    </span>
                    {domContext.landmark.label && (
                      <span className="text-xs text-muted-foreground truncate">
                        - "{domContext.landmark.label}"
                      </span>
                    )}
                    {domContext.landmark.positionZone &&
                      domContext.landmark.positionZone !== domContext.landmark.role && (
                        <Badge variant="muted" className="text-[10px] ml-auto">
                          {domContext.landmark.positionZone}
                        </Badge>
                      )}
                  </div>
                </div>
              )}

              {/* Siblings */}
              {domContext.siblings.length > 1 && (
                <div>
                  <span className="text-xs text-muted-foreground mb-1.5 block">
                    Siblings ({domContext.siblings.length})
                  </span>
                  <div className="space-y-1 max-h-32 overflow-auto">
                    {domContext.siblings.map((sibling: SiblingInfo) => (
                      <button
                        key={sibling.elementId}
                        className={`flex items-center gap-2 px-2 py-1 rounded-md transition-colors w-full text-left text-xs ${
                          sibling.isCurrent
                            ? "bg-primary/10 border border-primary/30"
                            : "bg-muted/20 hover:bg-muted/30 border border-transparent"
                        }`}
                        onClick={() => !sibling.isCurrent && onSelectElement(sibling.elementId)}
                        disabled={sibling.isCurrent}
                      >
                        {renderTypeIcon(sibling.type)}
                        <span className="font-mono truncate flex-1">
                          {sibling.tagName}
                          {sibling.role && sibling.role !== sibling.tagName && (
                            <span className="text-muted-foreground ml-1">[{sibling.role}]</span>
                          )}
                        </span>
                        {sibling.label && (
                          <span className="text-muted-foreground truncate max-w-[100px]">
                            "{sibling.label}"
                          </span>
                        )}
                        {sibling.isCurrent && (
                          <Badge variant="default" className="text-[10px]">
                            current
                          </Badge>
                        )}
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {/* All Elements Quick View */}
      <div className="mb-4">
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
              const desc = descriptions.get(element.id) || generateHeuristicDescription(element);
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
                  <p className="text-xs text-muted-foreground mt-1 truncate">{desc.description}</p>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {/* Bulk Generation & Storage Section */}
      {aiAvailable && (
        <div className="mb-4">
          <button
            className="w-full flex items-center gap-2 p-2 bg-muted/20 hover:bg-muted/30 rounded-lg transition-colors"
            onClick={() => toggleSection("bulk-actions")}
          >
            {expandedSections.has("bulk-actions") ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
            <Database className="w-4 h-4 text-emerald-500" />
            <span className="font-medium text-sm">Bulk Actions & Storage</span>
            {storageStats && (
              <Badge variant="muted" className="text-[10px] ml-auto">
                {storageStats.totalDescriptions} saved
              </Badge>
            )}
          </button>

          {expandedSections.has("bulk-actions") && (
            <div className="mt-2 p-3 bg-muted/10 rounded-lg border border-border/30 space-y-3">
              {/* Bulk Generate Button */}
              <div>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={handleBulkGenerateAi}
                  disabled={isGeneratingBulk || elements.length === 0}
                  className="w-full"
                >
                  {isGeneratingBulk ? (
                    <>
                      <Loader2 className="w-3.5 h-3.5 mr-1 animate-spin" />
                      Generating {bulkProgress?.current}/{bulkProgress?.total}...
                    </>
                  ) : (
                    <>
                      <Sparkles className="w-3.5 h-3.5 mr-1" />
                      Generate AI Descriptions for All Elements
                    </>
                  )}
                </Button>
                <p className="text-[10px] text-muted-foreground mt-1">
                  Generate descriptions for all interactive elements without AI descriptions
                </p>
              </div>

              {/* Storage Stats */}
              {storageStats && (
                <div className="grid grid-cols-3 gap-2 text-center">
                  <div className="p-2 bg-muted/20 rounded">
                    <div className="text-lg font-semibold">{storageStats.totalPages}</div>
                    <div className="text-[10px] text-muted-foreground">Pages</div>
                  </div>
                  <div className="p-2 bg-muted/20 rounded">
                    <div className="text-lg font-semibold">{storageStats.totalDescriptions}</div>
                    <div className="text-[10px] text-muted-foreground">Total</div>
                  </div>
                  <div className="p-2 bg-primary/10 rounded">
                    <div className="text-lg font-semibold text-primary">
                      {storageStats.aiDescriptions}
                    </div>
                    <div className="text-[10px] text-muted-foreground">AI</div>
                  </div>
                </div>
              )}

              {/* Storage Actions */}
              <div className="flex gap-2">
                <Button variant="outline" size="sm" onClick={handleExport} className="flex-1">
                  <Download className="w-3.5 h-3.5 mr-1" />
                  Export
                </Button>
                <Button variant="outline" size="sm" onClick={handleImport} className="flex-1">
                  <Upload className="w-3.5 h-3.5 mr-1" />
                  Import
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={handleClearPage}
                  className="text-destructive hover:text-destructive"
                  disabled={!pageContext?.url}
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </Button>
              </div>
            </div>
          )}
        </div>
      )}

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
          {storageStats && storageStats.totalDescriptions > 0 && (
            <p className="flex items-center gap-1 text-emerald-600 dark:text-emerald-400">
              <Database className="w-3 h-3" />
              Descriptions are persisted across sessions
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

export default ElementDescriptionPanel;
