/**
 * SpecDiscoveryTab.tsx
 *
 * Connects to live browser pages/apps via UI Bridge to discover specs
 * and generate verification workflows.
 *
 * Key features:
 *   - Real-time connection to browser tabs
 *   - Mobile device support (via ADB)
 *   - Spec discovery from connected pages
 *   - Spec group selection for workflow generation
 *   - Workflow generation, save, and apply
 *   - Search, Describe, NL panels for element interaction
 */

import { useState, useCallback, useEffect, useMemo } from "react";
import {
  Globe,
  Smartphone,
  RefreshCw,
  Play,
  Zap,
  Layers,
  Shield,
  Wrench,
  Copy,
  Save,
  FileText,
  Code,
  Loader2,
  CheckCircle2,
  AlertCircle,
  Bot,
  Search,
  MessageSquare,
  ShieldCheck,
  FileCode,
} from "lucide-react";
import { useLiveBrowser, type BrowserTab, type MobileDevice } from "@/hooks/useLiveBrowser";
import { SearchComparisonPanel } from "@/components/ui-bridge/SearchComparisonPanel";
import { ElementDescriptionPanel } from "@/components/ui-bridge/ElementDescriptionPanel";
import { NaturalLanguagePanel } from "@/components/ui-bridge/NaturalLanguagePanel";
import type { ExternalElement, PageContext, CommandResult } from "@/hooks/useExternalUIBridge";
import { cn } from "@/lib/utils";
import { buildSpecWorkflow, type SpecConfig, type SpecGroup } from "@/lib/workflow-builder";
import type { UnifiedWorkflow } from "@/types/unified-workflow";

// Use 127.0.0.1 instead of localhost to force IPv4 (runner only listens on IPv4)
const API_BASE = "http://127.0.0.1:9876";

// =============================================================================
// Types
// =============================================================================

interface UIBridgeContext {
  url?: string;
  title?: string;
  elements?: Array<{
    id: string;
    tagName: string;
    type: string;
    text?: string;
    label?: string;
    visible: boolean;
    enabled: boolean;
  }>;
  pageSnapshot?: string;
}

/** Discovered spec from a connected page */
interface DiscoveredSpec {
  specId: string;
  config: SpecConfig;
}

interface SpecDiscoveryTabProps {
  onLog?: (level: string, message: string) => void;
  onNavigateToLibrary?: () => void;
  onApplyWorkflow?: (workflow: UnifiedWorkflow) => void | Promise<void>;
}

type LeftPanelTab = "search" | "describe" | "nl";
type RightPanelTab = "definition" | "output";

interface Target {
  id: string;
  type: "browser" | "mobile";
  name: string;
  connected: boolean;
}

// Storage key for persisting state
const STORAGE_KEY = "spec-discovery-state";

interface PersistedState {
  instructions: string;
  expectedResults: string;
  agenticInstructions: string;
  discoveredSpecs?: DiscoveredSpec[];
}

// Load persisted state from localStorage
function loadPersistedState(): Partial<PersistedState> {
  try {
    const stored =
      localStorage.getItem(STORAGE_KEY) || localStorage.getItem("live-page-generator-state"); // backward compat
    if (stored) {
      return JSON.parse(stored);
    }
  } catch (e) {
    console.error("[SpecDiscovery] Failed to load persisted state:", e);
  }
  return {};
}

// =============================================================================
// Main Component
// =============================================================================

export function SpecDiscoveryTab({
  onLog,
  onNavigateToLibrary: _onNavigateToLibrary,
  onApplyWorkflow,
}: SpecDiscoveryTabProps) {
  // Load persisted state once on mount
  const [persistedState] = useState<Partial<PersistedState>>(() => loadPersistedState());

  // Tab state
  const [leftPanelTab, setLeftPanelTab] = useState<LeftPanelTab>("search");
  const [rightPanelTab, setRightPanelTab] = useState<RightPanelTab>("definition");

  // Target selection state
  const [selectedTabId, setSelectedTabId] = useState<number | null>(null);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string | null>(null);

  // Instructions state (with persistence)
  const [instructions, setInstructions] = useState(persistedState.instructions ?? "");
  const [expectedResults, setExpectedResults] = useState(persistedState.expectedResults ?? "");
  const [agenticInstructions, setAgenticInstructions] = useState(
    persistedState.agenticInstructions ?? "",
  );

  // Workflow action state
  const [isSavingWorkflow, setIsSavingWorkflow] = useState(false);
  const [savedWorkflowId, setSavedWorkflowId] = useState<string | null>(null);
  const [isApplyingWorkflow, setIsApplyingWorkflow] = useState(false);
  const [workflowApplied, setWorkflowApplied] = useState(false);

  // Spec discovery state
  const [discoveredSpecs, setDiscoveredSpecs] = useState<DiscoveredSpec[]>(
    persistedState.discoveredSpecs ?? [],
  );
  const [isDiscoveringSpecs, setIsDiscoveringSpecs] = useState(false);
  const [specAutoFilled, setSpecAutoFilled] = useState(
    (persistedState.discoveredSpecs ?? []).length > 0,
  );
  const [generatedWorkflow, setGeneratedWorkflow] = useState<UnifiedWorkflow | null>(null);
  const [selectedSpecGroupIds, setSelectedSpecGroupIds] = useState<Set<string>>(() => {
    const restored = persistedState.discoveredSpecs ?? [];
    if (restored.length > 0) {
      const ids = new Set<string>();
      for (const spec of restored) {
        for (const group of spec.config?.groups ?? []) {
          ids.add(group.id);
        }
      }
      return ids;
    }
    return new Set<string>();
  });
  const [generationError, setGenerationError] = useState<string | null>(null);

  // UI Bridge context
  const [_uiBridgeContext, setUiBridgeContext] = useState<UIBridgeContext | null>(null);

  // Persist state to localStorage when values change
  useEffect(() => {
    const stateToSave: PersistedState = {
      instructions,
      expectedResults,
      agenticInstructions,
      discoveredSpecs,
    };
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(stateToSave));
    } catch (e) {
      console.error("[SpecDiscovery] Failed to persist state:", e);
    }
  }, [instructions, expectedResults, agenticInstructions, discoveredSpecs]);

  // Use the live browser hook
  const {
    browserTabs,
    mobileDevices,
    isLoadingTargets,
    connectionStatus,
    connectedTarget,
    elements,
    selectedElementId,
    error: _connectionError,
    refreshTargets,
    connectToTab,
    connectToMobile,
    disconnect: _disconnect,
    refreshElements: _refreshElements,
    selectElement,
    highlightElement,
    executeAction,
  } = useLiveBrowser();

  // Convert elements to ExternalElement format for UI Bridge panels
  const externalElements = useMemo(() => elements as unknown as ExternalElement[], [elements]);

  // Get selected element for UI Bridge panels
  const selectedElement = useMemo(
    () =>
      selectedElementId
        ? (externalElements.find((el) => el.id === selectedElementId) ?? null)
        : null,
    [externalElements, selectedElementId],
  );

  // Create page context for ElementDescriptionPanel
  const pageContext = useMemo((): PageContext | null => {
    if (!connectedTarget || connectionStatus !== "connected") return null;
    return {
      url: connectedTarget.url || "",
      title: connectedTarget.name || "",
      elements: externalElements,
      timestamp: Date.now(),
    };
  }, [connectedTarget, connectionStatus, externalElements]);

  // Wrapper for executeAction that returns CommandResult
  const handleExecuteActionWithResult = useCallback(
    async (
      elementId: string,
      action: string,
      params?: Record<string, unknown>,
    ): Promise<CommandResult> => {
      try {
        await executeAction(elementId, action, params);
        return { success: true };
      } catch (error) {
        return {
          success: false,
          error: error instanceof Error ? error.message : "Action failed",
        };
      }
    },
    [executeAction],
  );

  // Wrapper for selectElement that matches expected signature
  const handleSelectElement = useCallback(
    (elementId: string | null) => {
      selectElement(elementId);
    },
    [selectElement],
  );

  // Refresh targets on mount
  useEffect(() => {
    refreshTargets();
  }, [refreshTargets]);

  // Build targets array for rendering
  const targets: Target[] = [
    ...browserTabs.map((tab) => ({
      id: `browser-${tab.id}`,
      type: "browser" as const,
      name: tab.title || tab.url,
      connected: selectedTabId === tab.id && connectionStatus === "connected",
    })),
    ...mobileDevices.map((device) => ({
      id: `mobile-${device.device_id}`,
      type: "mobile" as const,
      name: device.model || device.device_id,
      connected: selectedDeviceId === device.device_id && connectionStatus === "connected",
    })),
  ];

  // Handle tab selection
  const handleSelectTab = useCallback(
    async (tab: BrowserTab) => {
      setSelectedTabId(tab.id);
      setSelectedDeviceId(null);
      try {
        await connectToTab(tab.id);
      } catch (error) {
        onLog?.("error", `Failed to connect to tab: ${error}`);
      }
    },
    [connectToTab, onLog],
  );

  // Handle device selection
  const handleSelectDevice = useCallback(
    async (device: MobileDevice) => {
      setSelectedDeviceId(device.device_id);
      setSelectedTabId(null);
      try {
        await connectToMobile(device.device_id);
      } catch (error) {
        onLog?.("error", `Failed to connect to device: ${error}`);
      }
    },
    [connectToMobile, onLog],
  );

  // Handle target click from unified list
  const handleTargetClick = useCallback(
    (target: Target) => {
      if (target.type === "browser") {
        const tabId = parseInt(target.id.replace("browser-", ""));
        const tab = browserTabs.find((t) => t.id === tabId);
        if (tab) handleSelectTab(tab);
      } else {
        const deviceId = target.id.replace("mobile-", "");
        const device = mobileDevices.find((d) => d.device_id === deviceId);
        if (device) handleSelectDevice(device);
      }
    },
    [browserTabs, mobileDevices, handleSelectTab, handleSelectDevice],
  );

  // Auto-capture context when connected
  useEffect(() => {
    if (connectionStatus === "connected" && elements.length > 0) {
      setUiBridgeContext({
        url: connectedTarget?.url,
        title: connectedTarget?.name,
        elements: elements.map((el) => ({
          id: el.id,
          tagName: el.tagName,
          type: el.type,
          text: el.text,
          label: el.label,
          visible: el.visible,
          enabled: el.enabled,
        })),
      });
    }
  }, [connectionStatus, connectedTarget, elements]);

  // =============================================================================
  // Spec Discovery Functions
  // =============================================================================

  /** Discover specs from the connected page's SpecStore */
  const discoverSpecs = useCallback(async () => {
    if (connectionStatus !== "connected") {
      onLog?.("warning", "Not connected to a browser tab");
      return;
    }

    setIsDiscoveringSpecs(true);

    try {
      const response = await fetch(`${API_BASE}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "getSpecs", params: {} }),
      });

      if (!response.ok) {
        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
      }

      const data = await response.json();
      if (!data.success) {
        throw new Error(data.error || "Failed to get specs");
      }

      const specs: DiscoveredSpec[] = data.data?.specs || [];
      setDiscoveredSpecs(specs);

      if (specs.length > 0) {
        // Auto-fill instructions from spec descriptions
        const allGroups: SpecGroup[] = [];
        for (const spec of specs) {
          if (spec.config?.groups) {
            allGroups.push(...spec.config.groups);
          }
        }

        // Select all groups by default
        setSelectedSpecGroupIds(new Set(allGroups.map((g) => g.id)));

        // Build instructions summary from specs
        const instructionLines: string[] = [];
        for (const group of allGroups) {
          instructionLines.push(`Verify: ${group.name}: ${group.description}`);
          for (const assertion of group.assertions) {
            if (assertion.enabled) {
              instructionLines.push(`  - ${assertion.description} [${assertion.severity}]`);
            }
          }
        }
        setInstructions(instructionLines.join("\n"));

        // Build expected results from assertions
        const expectedLines = allGroups.flatMap((g) =>
          g.assertions
            .filter((a) => a.enabled)
            .map((a) => `- [${a.severity.toUpperCase()}] ${a.description}`),
        );
        setExpectedResults(expectedLines.join("\n"));

        // Clear and set agentic instructions for spec-driven workflow
        setAgenticInstructions(
          "Some spec assertions failed. Analyze the failure details and fix the code to make all assertions pass. Focus on the critical failures first.",
        );

        setSpecAutoFilled(true);

        onLog?.("success", `Discovered ${specs.length} spec(s) with ${allGroups.length} group(s)`);
      } else {
        onLog?.("info", "No specs found on this page");
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      onLog?.("error", `Failed to discover specs: ${errorMsg}`);
    } finally {
      setIsDiscoveringSpecs(false);
    }
  }, [connectionStatus, onLog]);

  /** Generate a UnifiedWorkflow from discovered specs */
  const handleGenerateWorkflow = useCallback(() => {
    if (discoveredSpecs.length === 0) {
      onLog?.("warning", "No specs discovered. Connect to a page with specs first.");
      return;
    }

    setGenerationError(null);

    // Merge all spec configs into one
    const mergedGroups: SpecGroup[] = [];
    for (const spec of discoveredSpecs) {
      if (spec.config?.groups) {
        mergedGroups.push(...spec.config.groups);
      }
    }

    const mergedConfig: SpecConfig = {
      version: "1.0.0",
      description: discoveredSpecs.map((s) => s.config?.description || s.specId).join("; "),
      groups: mergedGroups,
    };

    const workflow = buildSpecWorkflow({
      specConfig: mergedConfig,
      selectedGroupIds: selectedSpecGroupIds.size > 0 ? selectedSpecGroupIds : undefined,
      agenticPrompt: agenticInstructions || undefined,
      maxIterations: 3,
      elementSource: "external",
      pageUrl: connectedTarget?.url,
      workflowName: `Spec Verification — ${connectedTarget?.name || "Live Page"}`,
    });

    setGeneratedWorkflow(workflow);
    setRightPanelTab("output");
    onLog?.("success", `Generated workflow with ${workflow.verification_steps.length} spec steps`);
  }, [discoveredSpecs, selectedSpecGroupIds, agenticInstructions, connectedTarget, onLog]);

  /** Apply the generated workflow to the runner */
  const handleApplyWorkflow = useCallback(async () => {
    if (!generatedWorkflow || !onApplyWorkflow) return;
    setIsApplyingWorkflow(true);
    setWorkflowApplied(false);
    try {
      await onApplyWorkflow(generatedWorkflow);
      setWorkflowApplied(true);
      onLog?.("success", `Applied workflow: ${generatedWorkflow.name}`);
      setTimeout(() => setWorkflowApplied(false), 3000);
    } catch (error) {
      onLog?.("error", `Failed to apply workflow: ${error}`);
    } finally {
      setIsApplyingWorkflow(false);
    }
  }, [generatedWorkflow, onApplyWorkflow, onLog]);

  /** Save the generated workflow via the runner API */
  const handleSaveWorkflow = useCallback(async () => {
    if (!generatedWorkflow) return;

    setIsSavingWorkflow(true);
    setSavedWorkflowId(null);

    try {
      const response = await fetch(`${API_BASE}/unified-workflows`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: generatedWorkflow.name,
          description: generatedWorkflow.description || "",
          setup_steps: generatedWorkflow.setup_steps || [],
          verification_steps: generatedWorkflow.verification_steps || [],
          agentic_steps: generatedWorkflow.agentic_steps || [],
          completion_steps: generatedWorkflow.completion_steps || [],
          max_iterations: generatedWorkflow.max_iterations || 3,
        }),
      });

      const result = await response.json();
      if (result.success) {
        setSavedWorkflowId(result.data?.id || "saved");
        onLog?.("success", `Workflow saved: ${generatedWorkflow.name}`);
      } else {
        throw new Error(result.error || "Failed to save workflow");
      }
    } catch (error) {
      onLog?.("error", `Failed to save workflow: ${error}`);
    } finally {
      setIsSavingWorkflow(false);
    }
  }, [generatedWorkflow, onLog]);

  // Copy to clipboard helper
  const copyToClipboard = useCallback(
    async (text: string, label: string) => {
      try {
        await navigator.clipboard.writeText(text);
        onLog?.("success", `${label} copied to clipboard`);
      } catch {
        onLog?.("error", "Failed to copy to clipboard");
      }
    },
    [onLog],
  );

  // =============================================================================
  // Render
  // =============================================================================

  return (
    <div className="flex h-full bg-surface-canvas">
      {/* =========================================
          Left Panel - Exploration
          ========================================= */}
      <div className="flex w-[480px] flex-col border-r border-border bg-surface-raised">
        {/* Connections Section */}
        <div className="border-b border-border">
          <div className="flex items-center justify-between px-4 py-3">
            <h2 className="text-sm font-semibold text-text-primary">Connections</h2>
            <button
              onClick={refreshTargets}
              disabled={isLoadingTargets}
              className="flex h-7 w-7 items-center justify-center rounded text-text-muted hover:text-text-primary hover:bg-surface-hover transition-colors"
            >
              <RefreshCw className={cn("h-4 w-4", isLoadingTargets && "animate-spin")} />
            </button>
          </div>
          <div className="px-2 pb-2">
            <div className="mb-2 px-2 text-xs font-medium uppercase tracking-wider text-text-muted">
              Targets
            </div>
            <div className="max-h-[180px] space-y-1 overflow-y-auto scrollbar-dark">
              {targets.length > 0 ? (
                targets.map((target) => (
                  <TargetItem
                    key={target.id}
                    target={target}
                    onClick={() => handleTargetClick(target)}
                  />
                ))
              ) : isLoadingTargets ? (
                <div className="flex items-center justify-center py-4 text-text-muted text-xs">
                  <Loader2 className="h-4 w-4 animate-spin mr-2" />
                  Scanning...
                </div>
              ) : (
                <div className="text-center py-4 text-text-muted text-xs">
                  <p>No targets found.</p>
                  <p className="text-[10px] mt-1">Connect a browser tab or mobile device</p>
                </div>
              )}
            </div>
          </div>
          <div className="border-t border-border px-4 py-2">
            {/* Discover Specs button — shown when connected */}
            {connectionStatus === "connected" && (
              <button
                onClick={discoverSpecs}
                disabled={isDiscoveringSpecs}
                className="mt-2 w-full flex items-center justify-center gap-1.5 rounded-md py-1.5 text-xs font-medium bg-surface-hover text-text-primary hover:bg-surface-active disabled:opacity-50 transition-colors"
              >
                {isDiscoveringSpecs ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <ShieldCheck className="h-3.5 w-3.5" />
                )}
                {isDiscoveringSpecs ? "Discovering..." : "Discover Specs"}
                {discoveredSpecs.length > 0 && (
                  <span className="ml-1 rounded-full bg-brand-success/20 px-1.5 text-[10px] text-brand-success">
                    {discoveredSpecs.reduce((sum, s) => sum + (s.config?.groups?.length || 0), 0)}
                  </span>
                )}
              </button>
            )}
          </div>
        </div>

        {/* Main Tabs */}
        <div className="flex gap-1 border-b border-border px-3 py-2">
          <button
            onClick={() => setLeftPanelTab("search")}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              leftPanelTab === "search"
                ? "bg-brand-primary/10 text-brand-primary"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            <Search className="h-3.5 w-3.5" />
            Search
          </button>
          <button
            onClick={() => setLeftPanelTab("describe")}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              leftPanelTab === "describe"
                ? "bg-brand-primary/10 text-brand-primary"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            <Bot className="h-3.5 w-3.5" />
            Describe
          </button>
          <button
            onClick={() => setLeftPanelTab("nl")}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              leftPanelTab === "nl"
                ? "bg-brand-primary/10 text-brand-primary"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            <MessageSquare className="h-3.5 w-3.5" />
            NL
          </button>
        </div>

        {/* Tab Content */}
        <div className="flex-1 overflow-y-auto scrollbar-dark">
          {leftPanelTab === "search" && (
            <div className="h-full p-3">
              <SearchComparisonPanel
                elements={externalElements}
                onSelectElement={handleSelectElement}
                onHighlightElement={highlightElement}
                disabled={connectionStatus !== "connected"}
              />
            </div>
          )}

          {leftPanelTab === "describe" && (
            <div className="h-full p-3">
              <ElementDescriptionPanel
                elements={externalElements}
                selectedElement={selectedElement}
                pageContext={pageContext}
                onSelectElement={handleSelectElement}
                disabled={connectionStatus !== "connected"}
              />
            </div>
          )}

          {leftPanelTab === "nl" && (
            <div className="h-full p-3">
              <NaturalLanguagePanel
                elements={externalElements}
                onSelectElement={handleSelectElement}
                onHighlightElement={highlightElement}
                onExecuteAction={handleExecuteActionWithResult}
                pageContext={pageContext}
                disabled={connectionStatus !== "connected"}
              />
            </div>
          )}
        </div>
      </div>

      {/* =========================================
          Right Panel - Test Definition & Output
          ========================================= */}
      <div className="flex flex-1 flex-col overflow-hidden bg-surface-canvas">
        {/* Main Tabs */}
        <div className="flex gap-1 border-b border-border px-3 py-2">
          <button
            onClick={() => setRightPanelTab("definition")}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              rightPanelTab === "definition"
                ? "bg-brand-secondary/10 text-brand-secondary"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            <FileText className="h-3.5 w-3.5" />
            Test Definition
          </button>
          <button
            onClick={() => setRightPanelTab("output")}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              rightPanelTab === "output"
                ? "bg-brand-success/10 text-brand-success"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            <Code className="h-3.5 w-3.5" />
            Output
          </button>
        </div>

        {/* Tab Content */}
        <div className="flex-1 overflow-y-auto scrollbar-dark">
          {rightPanelTab === "definition" ? (
            <div className="p-4">
              <div className="mb-4 flex items-center gap-2">
                <div className="flex h-6 w-6 items-center justify-center rounded bg-brand-secondary/10">
                  <Play className="h-3.5 w-3.5 text-brand-secondary" />
                </div>
                <h2 className="text-sm font-semibold text-text-primary">Define Your Test</h2>
              </div>

              {/* Auto-fill indicator */}
              {specAutoFilled && (
                <div className="mb-3 flex items-center gap-2 rounded-lg bg-brand-success/10 border border-brand-success/20 px-3 py-2">
                  <ShieldCheck className="h-4 w-4 text-brand-success flex-shrink-0" />
                  <span className="text-xs text-brand-success">
                    Fields auto-filled from {discoveredSpecs.length} spec(s) with{" "}
                    {discoveredSpecs.reduce((sum, s) => sum + (s.config?.groups?.length || 0), 0)}{" "}
                    group(s)
                  </span>
                  <button
                    onClick={() => setSpecAutoFilled(false)}
                    className="ml-auto text-[10px] text-text-muted hover:text-text-primary"
                  >
                    Dismiss
                  </button>
                </div>
              )}

              {/* Spec Group Selection — shown when specs are discovered */}
              {discoveredSpecs.length > 0 && (
                <div className="mb-3 rounded-lg border border-border bg-surface-raised/50 p-3">
                  <div className="mb-2 flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <Layers className="h-4 w-4 text-brand-primary" />
                      <span className="text-xs font-medium uppercase tracking-wider text-text-muted">
                        Spec Groups
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      <button
                        onClick={() => {
                          const allIds = new Set<string>();
                          for (const spec of discoveredSpecs) {
                            for (const g of spec.config?.groups || []) {
                              allIds.add(g.id);
                            }
                          }
                          setSelectedSpecGroupIds(allIds);
                        }}
                        className="text-[10px] text-text-muted hover:text-text-primary"
                      >
                        All
                      </button>
                      <span className="text-[10px] text-text-muted">/</span>
                      <button
                        onClick={() => setSelectedSpecGroupIds(new Set())}
                        className="text-[10px] text-text-muted hover:text-text-primary"
                      >
                        None
                      </button>
                    </div>
                  </div>
                  <div className="space-y-1 max-h-[200px] overflow-y-auto">
                    {discoveredSpecs.flatMap((spec) =>
                      (spec.config?.groups || []).map((group) => (
                        <label
                          key={group.id}
                          className="flex items-start gap-2 rounded-md px-2 py-1.5 hover:bg-surface-hover cursor-pointer"
                        >
                          <input
                            type="checkbox"
                            checked={selectedSpecGroupIds.has(group.id)}
                            onChange={(e) => {
                              const next = new Set(selectedSpecGroupIds);
                              if (e.target.checked) {
                                next.add(group.id);
                              } else {
                                next.delete(group.id);
                              }
                              setSelectedSpecGroupIds(next);
                            }}
                            className="mt-0.5 rounded border-border"
                          />
                          <div className="flex-1 min-w-0">
                            <span className="text-xs font-medium text-text-primary block truncate">
                              {group.name}
                            </span>
                            {group.description && (
                              <span className="text-[10px] text-text-muted block truncate">
                                {group.description}
                              </span>
                            )}
                            <span className="text-[10px] text-text-muted">
                              {group.assertions.filter((a) => a.enabled).length} assertion(s)
                            </span>
                          </div>
                        </label>
                      )),
                    )}
                  </div>
                </div>
              )}

              <div className="rounded-xl border border-border bg-surface-raised/50 overflow-hidden">
                {/* Instructions */}
                <div className="border-l-2 border-brand-primary p-4">
                  <div className="mb-2 flex items-center gap-2">
                    <Zap className="h-4 w-4 text-brand-primary" />
                    <label className="text-xs font-medium uppercase tracking-wider text-text-muted">
                      Instructions
                    </label>
                  </div>
                  <textarea
                    value={instructions}
                    onChange={(e) => setInstructions(e.target.value)}
                    placeholder="Describe the test steps..."
                    rows={6}
                    className="w-full min-h-[140px] resize-none rounded-lg border border-border bg-surface-raised px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:border-brand-primary/50 focus:outline-none"
                  />
                </div>

                {/* Assertions */}
                <div className="border-l-2 border-brand-success border-t border-t-ds-default p-4">
                  <div className="mb-2 flex items-center gap-2">
                    <Shield className="h-4 w-4 text-brand-success" />
                    <label className="text-xs font-medium uppercase tracking-wider text-text-muted">
                      Assertions
                    </label>
                  </div>
                  <textarea
                    value={expectedResults}
                    onChange={(e) => setExpectedResults(e.target.value)}
                    placeholder="Define expected results..."
                    rows={6}
                    className="w-full min-h-[140px] resize-none rounded-lg border border-border bg-surface-raised px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:border-brand-success/50 focus:outline-none"
                  />
                </div>

                {/* Fix Logic */}
                <div className="border-l-2 border-brand-secondary border-t border-t-ds-default p-4">
                  <div className="mb-2 flex items-center gap-2">
                    <Wrench className="h-4 w-4 text-brand-secondary" />
                    <label className="text-xs font-medium uppercase tracking-wider text-text-muted">
                      Fix Logic
                    </label>
                  </div>
                  <textarea
                    value={agenticInstructions}
                    onChange={(e) => setAgenticInstructions(e.target.value)}
                    placeholder="Self-healing instructions for test failures..."
                    rows={6}
                    className="w-full min-h-[140px] resize-none rounded-lg border border-border bg-surface-raised px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:border-brand-secondary/50 focus:outline-none"
                  />
                </div>
              </div>

              {/* Generate Button */}
              <div className="mt-4 flex gap-2">
                <button
                  onClick={handleGenerateWorkflow}
                  disabled={discoveredSpecs.length === 0}
                  className="flex-1 flex items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-brand-success to-brand-success/80 py-3 text-sm font-medium text-white hover:from-brand-success/90 hover:to-brand-success/70 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
                >
                  <ShieldCheck className="h-4 w-4" />
                  Generate Workflow
                </button>
              </div>

              {generationError && (
                <div className="mt-4 p-3 bg-error/10 border border-error/30 rounded-lg">
                  <div className="flex items-start gap-2">
                    <AlertCircle className="h-4 w-4 text-error flex-shrink-0 mt-0.5" />
                    <div>
                      <p className="text-sm font-medium text-error">Generation Failed</p>
                      <p className="text-xs text-text-muted mt-1">{generationError}</p>
                    </div>
                  </div>
                </div>
              )}
            </div>
          ) : (
            <div className="flex flex-col h-full">
              {generatedWorkflow ? (
                <>
                  {/* Workflow Preview */}
                  <div className="flex-1 overflow-auto p-3">
                    <div className="space-y-3">
                      {/* Workflow Header */}
                      <div className="rounded-lg border border-border bg-surface-raised p-3">
                        <h3 className="text-sm font-medium text-text-primary">
                          {generatedWorkflow.name}
                        </h3>
                        <p className="text-xs text-text-muted mt-1">
                          {generatedWorkflow.description}
                        </p>
                      </div>

                      {/* Phases */}
                      {generatedWorkflow.setup_steps.length > 0 && (
                        <WorkflowPhaseSection
                          label="Setup"
                          color="text-blue-400"
                          steps={generatedWorkflow.setup_steps}
                        />
                      )}

                      <WorkflowPhaseSection
                        label="Verification"
                        color="text-brand-success"
                        steps={generatedWorkflow.verification_steps}
                      />

                      {generatedWorkflow.agentic_steps.length > 0 && (
                        <WorkflowPhaseSection
                          label="Agentic"
                          color="text-amber-400"
                          steps={generatedWorkflow.agentic_steps}
                        />
                      )}

                      {generatedWorkflow.completion_steps.length > 0 && (
                        <WorkflowPhaseSection
                          label="Completion"
                          color="text-purple-400"
                          steps={generatedWorkflow.completion_steps}
                        />
                      )}

                      {/* Settings */}
                      <div className="rounded-lg border border-border bg-surface-raised p-3">
                        <span className="text-xs text-text-muted">
                          Max iterations: {generatedWorkflow.max_iterations || 3}
                        </span>
                      </div>
                    </div>
                  </div>

                  {/* Workflow Actions */}
                  <div className="flex gap-2 border-t border-border p-3">
                    {onApplyWorkflow && (
                      <button
                        onClick={handleApplyWorkflow}
                        disabled={isApplyingWorkflow}
                        className={cn(
                          "flex-1 flex items-center justify-center gap-2 rounded-lg py-2 text-sm font-medium text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed",
                          workflowApplied
                            ? "bg-brand-success/70"
                            : "bg-brand-success hover:bg-brand-success/90",
                        )}
                      >
                        {isApplyingWorkflow ? (
                          <>
                            <Loader2 className="h-4 w-4 animate-spin" />
                            Applying...
                          </>
                        ) : workflowApplied ? (
                          <>
                            <CheckCircle2 className="h-4 w-4" />
                            Applied
                          </>
                        ) : (
                          <>
                            <Play className="h-4 w-4" />
                            Apply Workflow
                          </>
                        )}
                      </button>
                    )}
                    <button
                      onClick={handleSaveWorkflow}
                      disabled={isSavingWorkflow}
                      className={cn(
                        "flex items-center justify-center gap-2 px-4 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed",
                        savedWorkflowId
                          ? "bg-brand-success/10 text-brand-success"
                          : "bg-surface-hover text-text-primary hover:bg-surface-active",
                      )}
                    >
                      {isSavingWorkflow ? (
                        <>
                          <Loader2 className="h-4 w-4 animate-spin" />
                          Saving...
                        </>
                      ) : savedWorkflowId ? (
                        <>
                          <CheckCircle2 className="h-4 w-4" />
                          Saved
                        </>
                      ) : (
                        <>
                          <Save className="h-4 w-4" />
                          Save
                        </>
                      )}
                    </button>
                    <button
                      onClick={() =>
                        copyToClipboard(JSON.stringify(generatedWorkflow, null, 2), "Workflow JSON")
                      }
                      className="flex items-center justify-center gap-2 px-4 rounded-lg bg-surface-hover text-text-primary hover:bg-surface-active transition-colors"
                    >
                      <Copy className="h-4 w-4" />
                      Copy
                    </button>
                  </div>
                </>
              ) : (
                <div className="flex-1 flex items-center justify-center">
                  <div className="text-center text-text-muted max-w-md">
                    <Bot className="w-12 h-12 mx-auto mb-3 opacity-30" />
                    <h3 className="text-base font-medium mb-1">No Output Yet</h3>
                    <p className="text-sm">
                      Discover specs from the connected page and click "Generate Workflow" to create
                      a spec-driven workflow.
                    </p>
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Sub-Components
// =============================================================================

interface TargetItemProps {
  target: Target;
  onClick: () => void;
}

function TargetItem({ target, onClick }: TargetItemProps) {
  return (
    <button
      onClick={onClick}
      className="w-full flex items-center gap-3 rounded-md px-2 py-1.5 transition-colors hover:bg-surface-hover cursor-pointer text-left"
    >
      <div className="flex h-7 w-7 items-center justify-center rounded-md bg-surface-hover">
        {target.type === "browser" ? (
          <Globe className="h-3.5 w-3.5 text-brand-primary" />
        ) : (
          <Smartphone className="h-3.5 w-3.5 text-brand-secondary" />
        )}
      </div>
      <div className="flex-1 min-w-0">
        <div className="truncate text-sm text-text-primary">{target.name}</div>
        <div className="flex items-center gap-1.5 text-xs text-text-muted">
          {target.connected ? (
            <>
              <span className="h-1.5 w-1.5 rounded-full bg-brand-success" />
              <span className="text-brand-success">Connected</span>
            </>
          ) : (
            <>
              <span className="h-1.5 w-1.5 rounded-full bg-text-muted" />
              <span>Disconnected</span>
            </>
          )}
        </div>
      </div>
    </button>
  );
}

/** Phase section for workflow preview */
interface WorkflowPhaseSectionProps {
  label: string;
  color: string;
  steps: Array<{ id: string; name: string; type: string; description?: string }>;
}

function WorkflowPhaseSection({ label, color, steps }: WorkflowPhaseSectionProps) {
  return (
    <div className="rounded-lg border border-border bg-surface-raised/50 overflow-hidden">
      <div className="px-3 py-2 border-b border-border bg-surface-canvas/50">
        <span className={cn("text-xs font-medium uppercase tracking-wider", color)}>{label}</span>
        <span className="text-xs text-text-muted ml-2">({steps.length})</span>
      </div>
      <div className="divide-y divide-border">
        {steps.map((step) => (
          <div key={step.id} className="px-3 py-2">
            <div className="flex items-center gap-2">
              {step.type === "spec" ? (
                <ShieldCheck className="h-3.5 w-3.5 text-brand-success flex-shrink-0" />
              ) : step.type === "prompt" ? (
                <Bot className="h-3.5 w-3.5 text-amber-400 flex-shrink-0" />
              ) : (
                <FileCode className="h-3.5 w-3.5 text-text-muted flex-shrink-0" />
              )}
              <span className="text-xs text-text-primary font-medium">{step.name}</span>
              <span className="text-[10px] text-text-muted ml-auto">{step.type}</span>
            </div>
            {step.description && (
              <p className="text-[10px] text-text-muted mt-1 ml-5 line-clamp-2">
                {step.description}
              </p>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

export default SpecDiscoveryTab;
