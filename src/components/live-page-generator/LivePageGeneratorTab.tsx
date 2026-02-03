/**
 * LivePageGeneratorTab.tsx
 *
 * Connects to live browser pages/apps via UI Bridge and uses AI to generate:
 *   - Python verification test code
 *   - Agentic step prompts
 *
 * Key features:
 *   - Real-time connection to browser tabs (via Qontinui DevTools extension)
 *   - Mobile device support (via ADB)
 *   - UI Bridge element context capture
 *   - Natural language instructions
 *   - Save to Test and Task libraries
 *
 * Design: Two-panel layout based on v0 design with qontinui design tokens
 */

import { useState, useCallback, useEffect, useMemo } from "react";
import {
  Globe,
  Smartphone,
  RefreshCw,
  Plug,
  MousePointer,
  Camera,
  Sparkles,
  Play,
  Zap,
  ChevronRight,
  ChevronDown,
  Layout,
  Type,
  Workflow,
  Layers,
  Shield,
  Wrench,
  FileCode,
  Copy,
  Save,
  FileText,
  Code,
  Loader2,
  CheckCircle2,
  AlertCircle,
  Bot,
  Clock,
  ExternalLink,
  ImageIcon,
  Search,
  MessageSquare,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useLiveBrowser, type BrowserTab, type MobileDevice } from "@/hooks/useLiveBrowser";
import { SearchComparisonPanel } from "@/components/ui-bridge/SearchComparisonPanel";
import { ElementDescriptionPanel } from "@/components/ui-bridge/ElementDescriptionPanel";
import { NaturalLanguagePanel } from "@/components/ui-bridge/NaturalLanguagePanel";
import type { ExternalElement, PageContext, CommandResult } from "@/hooks/useExternalUIBridge";
import { ContextSelector } from "@/components/contexts/ContextSelector";
import type { ContextSelection } from "@/types/context";
import { cn } from "@/lib/utils";

// Use 127.0.0.1 instead of localhost to force IPv4 (runner only listens on IPv4)
const API_BASE = "http://127.0.0.1:9876";

// =============================================================================
// Types
// =============================================================================

interface PageCapture {
  url: string;
  title: string;
  capturedAt: string;
  stepIndex: number;
  elements: Array<{
    id: string;
    tagName: string;
    type: string;
    text?: string;
    label?: string;
    visible: boolean;
    enabled: boolean;
  }>;
}

interface MultiPageContext {
  pages: PageCapture[];
  totalElements: number;
}

interface GeneratedContent {
  verificationTest: string;
  agenticStep: string;
  testName: string;
  agenticName: string;
}

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

interface LivePageGeneratorTabProps {
  onLog?: (level: string, message: string) => void;
  onNavigateToLibrary?: () => void;
}

type TargetType = "browser" | "mobile" | "none";
type LeftPanelTab = "flow" | "context" | "search" | "describe" | "nl";
type RightPanelTab = "definition" | "output";
type OutputSubTab = "python" | "agentic";

interface Target {
  id: string;
  type: "browser" | "mobile";
  name: string;
  connected: boolean;
}

interface FlowStep {
  step: number;
  action: string;
  description: string;
  status: "running" | "completed" | "error";
  error?: string;
}

interface TreeNode {
  id: string;
  name: string;
  type: "page" | "element";
  icon?: "input" | "button" | "image" | "text";
  elementCount?: number;
  children?: TreeNode[];
}

// Storage key for persisting state
const STORAGE_KEY = "live-page-generator-state";

interface PersistedState {
  flowPrompt: string;
  instructions: string;
  expectedResults: string;
  agenticInstructions: string;
  multiPageContext: MultiPageContext | null;
  explorationLog: FlowStep[];
  selectedPageIndex: number | null;
}

// Icon maps for flow steps and elements
const flowIconMap = {
  capture: Camera,
  click: MousePointer,
  type: Type,
  wait: Clock,
  ai_decision: Bot,
  navigate: Play,
  input: Zap,
};

const elementIconMap = {
  input: Type,
  button: MousePointer,
  image: ImageIcon,
  text: Layout,
};

// Load persisted state from localStorage
function loadPersistedState(): Partial<PersistedState> {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored) {
      const parsed = JSON.parse(stored);
      if (parsed.explorationLog && Array.isArray(parsed.explorationLog)) {
        parsed.explorationLog = parsed.explorationLog.map((log: FlowStep) =>
          log.status === "running"
            ? { ...log, status: "error" as const, error: "Session was interrupted" }
            : log,
        );
      }
      return parsed;
    }
  } catch (e) {
    console.error("[LivePageGenerator] Failed to load persisted state:", e);
  }
  return {};
}

// =============================================================================
// Main Component
// =============================================================================

export function LivePageGeneratorTab({ onLog, onNavigateToLibrary }: LivePageGeneratorTabProps) {
  // Load persisted state once on mount
  const [persistedState] = useState<Partial<PersistedState>>(() => loadPersistedState());

  // Tab state
  const [leftPanelTab, setLeftPanelTab] = useState<LeftPanelTab>("flow");
  const [rightPanelTab, setRightPanelTab] = useState<RightPanelTab>("definition");
  const [outputSubTab, setOutputSubTab] = useState<OutputSubTab>("python");

  // Target selection state
  const [_selectedTargetType, setSelectedTargetType] = useState<TargetType>("none");
  const [selectedTabId, setSelectedTabId] = useState<number | null>(null);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string | null>(null);

  // Instructions state (with persistence)
  const [instructions, setInstructions] = useState(persistedState.instructions ?? "");
  const [expectedResults, setExpectedResults] = useState(persistedState.expectedResults ?? "");
  const [agenticInstructions, setAgenticInstructions] = useState(
    persistedState.agenticInstructions ?? "",
  );

  // Generation state
  const [isGenerating, setIsGenerating] = useState(false);
  const [generatedContent, setGeneratedContent] = useState<GeneratedContent | null>(null);
  const [generationError, setGenerationError] = useState<string | null>(null);

  // Saving state
  const [isSavingTest, setIsSavingTest] = useState(false);
  const [isSavingTask, setIsSavingTask] = useState(false);
  const [savedTestId, setSavedTestId] = useState<string | null>(null);
  const [savedTaskId, setSavedTaskId] = useState<string | null>(null);

  // UI Bridge context
  const [uiBridgeContext, setUiBridgeContext] = useState<UIBridgeContext | null>(null);
  const [_isCapturingContext, setIsCapturingContext] = useState(false);

  // Context selection for AI
  const [contextSelection, setContextSelection] = useState<ContextSelection>({
    autoDetect: true,
    selectedIds: [],
  });

  // AI-driven flow exploration state (with persistence)
  const [flowPrompt, setFlowPrompt] = useState(persistedState.flowPrompt ?? "");
  const [isExploring, setIsExploring] = useState(false);
  const [explorationLog, setExplorationLog] = useState<FlowStep[]>(
    persistedState.explorationLog ?? [],
  );
  const [multiPageContext, setMultiPageContext] = useState<MultiPageContext | null>(
    persistedState.multiPageContext ?? null,
  );
  const [flowError, setFlowError] = useState<string | null>(null);
  const [selectedPageIndex, setSelectedPageIndex] = useState<number | null>(
    persistedState.selectedPageIndex ?? null,
  );

  // Tree expansion state for captured pages
  const [expandedNodes, setExpandedNodes] = useState<Set<string>>(new Set(["page-0"]));

  // Persist state to localStorage when values change
  useEffect(() => {
    const stateToSave: PersistedState = {
      flowPrompt,
      instructions,
      expectedResults,
      agenticInstructions,
      multiPageContext,
      explorationLog,
      selectedPageIndex,
    };
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(stateToSave));
    } catch (e) {
      console.error("[LivePageGenerator] Failed to persist state:", e);
    }
  }, [
    flowPrompt,
    instructions,
    expectedResults,
    agenticInstructions,
    multiPageContext,
    explorationLog,
    selectedPageIndex,
  ]);

  // Use the live browser hook
  const {
    browserTabs,
    mobileDevices,
    isLoadingTargets,
    isExtensionConnected,
    connectionStatus,
    connectedTarget,
    elements,
    selectedElementId,
    error: _connectionError,
    refreshTargets,
    connectToTab,
    connectToMobile,
    disconnect: _disconnect,
    refreshElements,
    selectElement,
    highlightElement,
    executeAction,
  } = useLiveBrowser();

  // Convert elements to ExternalElement format for UI Bridge panels
  // DiscoveredElement and ExternalElement are structurally compatible
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
      setSelectedTargetType("browser");
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
      setSelectedTargetType("mobile");
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

  // Capture UI Bridge context from connected target
  const _captureContext = useCallback(async () => {
    if (connectionStatus !== "connected") {
      onLog?.("warning", "Not connected to any target");
      return;
    }

    setIsCapturingContext(true);
    try {
      await refreshElements();
      const context: UIBridgeContext = {
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
      };
      setUiBridgeContext(context);
      onLog?.(
        "success",
        `Captured context: ${elements.length} elements from ${connectedTarget?.name}`,
      );
    } catch (error) {
      onLog?.("error", `Failed to capture context: ${error}`);
    } finally {
      setIsCapturingContext(false);
    }
  }, [connectionStatus, connectedTarget, elements, refreshElements, onLog]);

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
  // AI-Driven Flow Exploration Functions
  // =============================================================================

  const clearExploration = useCallback(() => {
    setExplorationLog([]);
    setMultiPageContext(null);
    setFlowError(null);
    setSelectedPageIndex(null);
  }, []);

  const captureCurrentPage = useCallback(async (): Promise<PageCapture | null> => {
    try {
      const response = await fetch(`${API_BASE}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "getElements",
          params: {},
        }),
      });

      if (!response.ok) {
        throw new Error("Failed to get elements");
      }

      const data = await response.json();
      if (!data.success) {
        throw new Error(data.error || "Failed to capture elements");
      }

      const tabResponse = await fetch(`${API_BASE}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "getActiveTab",
          params: {},
        }),
      });

      let tabInfo = {
        url: connectedTarget?.url || "unknown",
        title: connectedTarget?.name || "Unknown",
      };
      if (tabResponse.ok) {
        const tabData = await tabResponse.json();
        if (tabData.success && tabData.data) {
          tabInfo = {
            url: tabData.data.url || tabInfo.url,
            title: tabData.data.title || tabInfo.title,
          };
        }
      }

      return {
        url: tabInfo.url,
        title: tabInfo.title,
        capturedAt: new Date().toISOString(),
        stepIndex: 0,
        elements: data.data?.elements || [],
      };
    } catch (error) {
      console.error("[FlowCapture] Failed to capture page:", error);
      throw error;
    }
  }, [connectedTarget]);

  const executeClick = useCallback(async (elementId: string): Promise<void> => {
    const response = await fetch(`${API_BASE}/extension/command`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        action: "executeAction",
        params: {
          elementId,
          action: "click",
          params: {},
        },
      }),
    });

    if (!response.ok) {
      throw new Error("Failed to execute click");
    }

    const data = await response.json();
    if (!data.success) {
      throw new Error(data.error || "Click action failed");
    }
  }, []);

  const executeType = useCallback(async (elementId: string, text: string): Promise<void> => {
    const response = await fetch(`${API_BASE}/extension/command`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        action: "executeAction",
        params: {
          elementId,
          action: "fill",
          params: { text },
        },
      }),
    });

    if (!response.ok) {
      throw new Error("Failed to execute type");
    }

    const data = await response.json();
    if (!data.success) {
      throw new Error(data.error || "Type action failed");
    }
  }, []);

  const executeWait = useCallback(async (ms: number): Promise<void> => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  }, []);

  const exploreFlow = useCallback(async () => {
    if (!flowPrompt.trim()) {
      onLog?.("warning", "Please describe the navigation flow");
      return;
    }

    if (connectionStatus !== "connected") {
      onLog?.("warning", `Not connected to a browser tab (status: ${connectionStatus})`);
      return;
    }

    setIsExploring(true);
    setFlowError(null);
    setMultiPageContext(null);
    setExplorationLog([]);

    const captures: PageCapture[] = [];
    const maxSteps = 10;

    try {
      onLog?.("info", "Starting AI-driven exploration...");
      let stepNum = 0;

      stepNum++;
      setExplorationLog((prev) => [
        ...prev,
        {
          step: stepNum,
          action: "capture",
          description: "Capturing initial page",
          status: "running",
        },
      ]);

      const initialCapture = await captureCurrentPage();
      if (!initialCapture || initialCapture.elements.length === 0) {
        throw new Error("Failed to capture initial page elements");
      }
      initialCapture.stepIndex = 0;
      captures.push(initialCapture);

      setExplorationLog((prev) =>
        prev.map((l) =>
          l.step === stepNum
            ? {
                ...l,
                status: "completed",
                description: `Captured ${initialCapture.elements.length} elements from ${initialCapture.title}`,
              }
            : l,
        ),
      );
      onLog?.("success", `Captured initial page: ${initialCapture.elements.length} elements`);

      let done = false;
      while (!done && stepNum < maxSteps) {
        stepNum++;

        const currentCapture = await captureCurrentPage();
        if (!currentCapture) {
          throw new Error("Failed to capture current page for AI decision");
        }

        setExplorationLog((prev) => [
          ...prev,
          {
            step: stepNum,
            action: "ai_decision",
            description: "AI analyzing page...",
            status: "running",
          },
        ]);

        const aiResult = await invoke<{
          success: boolean;
          message?: string;
          data?: {
            action: "click" | "type" | "wait" | "done";
            element_id?: string;
            element_description?: string;
            text?: string;
            wait_ms?: number;
            reason: string;
            should_capture_after?: boolean;
          };
        }>("explore_flow_step", {
          input: {
            user_prompt: flowPrompt,
            current_elements: currentCapture.elements,
            current_url: currentCapture.url,
            current_title: currentCapture.title,
            captured_pages: captures.map((c) => ({
              url: c.url,
              title: c.title,
              element_count: c.elements.length,
            })),
            step_number: stepNum,
          },
        });

        if (!aiResult.success || !aiResult.data) {
          throw new Error(aiResult.message || "AI exploration failed");
        }

        const aiAction = aiResult.data;

        setExplorationLog((prev) =>
          prev.map((l) =>
            l.step === stepNum
              ? { ...l, description: `AI: ${aiAction.reason}`, status: "completed" }
              : l,
          ),
        );

        if (aiAction.action === "done") {
          onLog?.("success", `AI completed: ${aiAction.reason}`);
          done = true;
          break;
        }

        stepNum++;
        const actionDesc =
          aiAction.action === "click"
            ? `Clicking: ${aiAction.element_description || aiAction.element_id}`
            : aiAction.action === "type"
              ? `Typing "${aiAction.text}" into ${aiAction.element_description || aiAction.element_id}`
              : `Waiting ${aiAction.wait_ms}ms`;

        setExplorationLog((prev) => [
          ...prev,
          { step: stepNum, action: aiAction.action, description: actionDesc, status: "running" },
        ]);

        try {
          if (aiAction.action === "click" && aiAction.element_id) {
            await executeClick(aiAction.element_id);
            onLog?.("success", `Clicked: ${aiAction.element_description || aiAction.element_id}`);
            await executeWait(1500);
          } else if (aiAction.action === "type" && aiAction.element_id && aiAction.text) {
            await executeType(aiAction.element_id, aiAction.text);
            onLog?.("success", `Typed: "${aiAction.text}"`);
          } else if (aiAction.action === "wait") {
            await executeWait(aiAction.wait_ms || 2000);
            onLog?.("success", `Waited ${aiAction.wait_ms || 2000}ms`);
          }

          setExplorationLog((prev) =>
            prev.map((l) => (l.step === stepNum ? { ...l, status: "completed" } : l)),
          );

          if (aiAction.should_capture_after) {
            stepNum++;
            setExplorationLog((prev) => [
              ...prev,
              {
                step: stepNum,
                action: "capture",
                description: "Capturing page after action",
                status: "running",
              },
            ]);

            await executeWait(1000);

            const newCapture = await captureCurrentPage();
            if (newCapture && newCapture.elements.length > 0) {
              const isDuplicate = captures.some((c) => {
                if (c.url !== newCapture.url) return false;
                const countDiff = Math.abs(c.elements.length - newCapture.elements.length);
                const threshold = Math.max(c.elements.length, newCapture.elements.length) * 0.1;
                return countDiff <= threshold;
              });

              if (isDuplicate) {
                setExplorationLog((prev) =>
                  prev.map((l) =>
                    l.step === stepNum
                      ? { ...l, status: "completed", description: "Skipped duplicate capture" }
                      : l,
                  ),
                );
              } else {
                newCapture.stepIndex = captures.length;
                captures.push(newCapture);

                setExplorationLog((prev) =>
                  prev.map((l) =>
                    l.step === stepNum
                      ? {
                          ...l,
                          status: "completed",
                          description: `Captured ${newCapture.elements.length} elements from ${newCapture.title}`,
                        }
                      : l,
                  ),
                );
                onLog?.(
                  "success",
                  `Captured page: ${newCapture.elements.length} elements from ${newCapture.title}`,
                );
              }
            }
          }
        } catch (actionError) {
          const errorMsg = actionError instanceof Error ? actionError.message : String(actionError);
          setExplorationLog((prev) =>
            prev.map((l) => (l.step === stepNum ? { ...l, status: "error", error: errorMsg } : l)),
          );
          throw new Error(`Action failed: ${errorMsg}`);
        }
      }

      if (captures.length > 0) {
        const totalElements = captures.reduce((sum, c) => sum + c.elements.length, 0);
        setMultiPageContext({
          pages: captures,
          totalElements,
        });
        onLog?.(
          "success",
          `Exploration complete: ${captures.length} page(s), ${totalElements} elements`,
        );
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      setFlowError(errorMsg);
      setExplorationLog((prev) =>
        prev.map((l) => (l.status === "running" ? { ...l, status: "error", error: errorMsg } : l)),
      );
      onLog?.("error", `Exploration failed: ${errorMsg}`);
    } finally {
      setIsExploring(false);
      setExplorationLog((prev) =>
        prev.map((l) => (l.status === "running" ? { ...l, status: "completed" } : l)),
      );
    }
  }, [
    flowPrompt,
    connectionStatus,
    onLog,
    captureCurrentPage,
    executeClick,
    executeType,
    executeWait,
  ]);

  // Generate test and agentic step with AI
  const handleGenerate = useCallback(async () => {
    if (!instructions.trim()) {
      onLog?.("warning", "Please provide instructions for what the test should do");
      return;
    }

    setIsGenerating(true);
    setGenerationError(null);
    setGeneratedContent(null);

    try {
      const fullPrompt = `## Test Automation Instructions
${instructions}

## Expected Results (Assertions)
${expectedResults}

## Agentic Step Instructions (Code Changes to Fix Issues)
${agenticInstructions || "Fix any issues identified by the test failures."}`;

      let pageContextForAI: {
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
        pages?: Array<{
          url: string;
          title: string;
          elements: Array<{
            id: string;
            tagName: string;
            type: string;
            text?: string;
            label?: string;
            visible: boolean;
            enabled: boolean;
          }>;
        }>;
      } | null = null;

      if (multiPageContext && multiPageContext.pages.length > 0) {
        pageContextForAI = {
          pages: multiPageContext.pages.map((p) => ({
            url: p.url,
            title: p.title,
            elements: p.elements,
          })),
        };
      } else if (uiBridgeContext) {
        pageContextForAI = {
          url: uiBridgeContext.url,
          title: uiBridgeContext.title,
          elements: uiBridgeContext.elements,
        };
      }

      const result = await invoke<{
        success: boolean;
        message?: string;
        data?: {
          verification_test: string;
          agentic_step: string;
          test_name: string;
          agentic_name: string;
        };
      }>("generate_test_and_agentic_step", {
        input: {
          user_prompt: fullPrompt,
          page_context: pageContextForAI,
          context_ids:
            contextSelection.selectedIds.length > 0 ? contextSelection.selectedIds : null,
        },
      });

      if (result.success && result.data) {
        const content = {
          verificationTest: result.data.verification_test || "",
          agenticStep: result.data.agentic_step || "",
          testName: result.data.test_name || "generated_test",
          agenticName: result.data.agentic_name || "Generated Task",
        };
        setGeneratedContent(content);
        setRightPanelTab("output");
        onLog?.("success", "Successfully generated test and agentic step");
      } else {
        const errMsg = result.message || "Generation failed - no data returned";
        throw new Error(errMsg);
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      setGenerationError(errorMsg);
      onLog?.("error", `Generation failed: ${errorMsg}`);
    } finally {
      setIsGenerating(false);
    }
  }, [
    instructions,
    expectedResults,
    agenticInstructions,
    uiBridgeContext,
    multiPageContext,
    contextSelection.selectedIds,
    onLog,
  ]);

  // Save verification test to library
  const handleSaveTest = useCallback(async () => {
    if (!generatedContent) return;

    setIsSavingTest(true);
    try {
      const response = await fetch(`${API_BASE}/tests`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: generatedContent.testName,
          description: instructions.slice(0, 200),
          test_type: "python_script",
          python_code: generatedContent.verificationTest,
          category: "ai-generated",
          ai_generated: true,
          ai_generation_prompt: instructions,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        setSavedTestId(result.data.id);
        onLog?.("success", `Test saved: ${generatedContent.testName}`);
      } else {
        throw new Error(result.error || "Failed to save test");
      }
    } catch (error) {
      onLog?.("error", `Failed to save test: ${error}`);
    } finally {
      setIsSavingTest(false);
    }
  }, [generatedContent, instructions, onLog]);

  // Save agentic step to library
  const handleSaveTask = useCallback(async () => {
    if (!generatedContent) return;

    setIsSavingTask(true);
    try {
      const response = await fetch(`${API_BASE}/prompts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: generatedContent.agenticName,
          description: `Agentic step for: ${instructions.slice(0, 100)}`,
          content: generatedContent.agenticStep,
          category: "ai-generated",
          tags: ["ai-generated", "agentic"],
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        setSavedTaskId(result.data.id);
        onLog?.("success", `Task saved: ${generatedContent.agenticName}`);
      } else {
        throw new Error(result.error || "Failed to save task");
      }
    } catch (error) {
      onLog?.("error", `Failed to save task: ${error}`);
    } finally {
      setIsSavingTask(false);
    }
  }, [generatedContent, instructions, onLog]);

  // Copy to clipboard
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

  // Build tree from multi-page context
  const buildPageTree = useCallback((): TreeNode[] => {
    if (!multiPageContext) return [];
    return multiPageContext.pages.map((page, idx) => ({
      id: `page-${idx}`,
      name: `Page ${idx + 1}: ${page.title.slice(0, 30)}${page.title.length > 30 ? "..." : ""}`,
      type: "page" as const,
      elementCount: page.elements.length,
      children: page.elements.slice(0, 10).map((el, elIdx) => ({
        id: `page-${idx}-el-${elIdx}`,
        name: el.text?.slice(0, 30) || el.label?.slice(0, 30) || el.id,
        type: "element" as const,
        icon: (el.tagName.toLowerCase() === "input"
          ? "input"
          : el.tagName.toLowerCase() === "button"
            ? "button"
            : el.tagName.toLowerCase() === "img"
              ? "image"
              : "text") as "input" | "button" | "image" | "text",
      })),
    }));
  }, [multiPageContext]);

  const toggleNode = useCallback((nodeId: string) => {
    setExpandedNodes((prev) => {
      const next = new Set(prev);
      if (next.has(nodeId)) {
        next.delete(nodeId);
      } else {
        next.add(nodeId);
      }
      return next;
    });
  }, []);

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
                  <p className="text-[10px] mt-1">Install Qontinui DevTools extension</p>
                </div>
              )}
            </div>
          </div>
          <div className="border-t border-border px-4 py-2">
            <div className="flex items-center gap-2">
              <span
                className={cn(
                  "h-2 w-2 rounded-full",
                  isExtensionConnected ? "bg-brand-success animate-pulse" : "bg-text-muted",
                )}
              />
              <span className="text-xs text-text-muted">
                {isExtensionConnected ? "Extension Connected" : "Extension Not Connected"}
              </span>
              <Plug
                className={cn(
                  "ml-auto h-3 w-3",
                  isExtensionConnected ? "text-brand-success" : "text-text-muted",
                )}
              />
            </div>
          </div>
        </div>

        {/* Main Tabs */}
        <div className="flex gap-1 border-b border-border px-3 py-2">
          <button
            onClick={() => setLeftPanelTab("flow")}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              leftPanelTab === "flow"
                ? "bg-brand-primary/10 text-brand-primary"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            <Workflow className="h-3.5 w-3.5" />
            Flow Explorer
          </button>
          <button
            onClick={() => setLeftPanelTab("context")}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              leftPanelTab === "context"
                ? "bg-brand-primary/10 text-brand-primary"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            <Layers className="h-3.5 w-3.5" />
            Captured Pages
          </button>
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
          {leftPanelTab === "flow" && (
            <div className="p-4">
              <div className="mb-3 flex items-center gap-2">
                <div className="flex h-6 w-6 items-center justify-center rounded bg-brand-primary/10">
                  <Sparkles className="h-3.5 w-3.5 text-brand-primary" />
                </div>
                <span className="rounded-full bg-brand-primary/10 px-2 py-0.5 text-xs text-brand-primary">
                  AI-Powered
                </span>
                {(explorationLog.length > 0 || multiPageContext) && (
                  <button
                    onClick={clearExploration}
                    className="ml-auto text-[10px] text-text-muted hover:text-error"
                  >
                    Clear
                  </button>
                )}
              </div>

              {/* Timeline */}
              {explorationLog.length > 0 && (
                <div className="relative mb-4 rounded-lg border border-border bg-surface-canvas/50 p-3">
                  <div className="absolute left-6 top-4 bottom-4 w-px bg-border" />
                  <div className="space-y-2">
                    {explorationLog.map((step) => {
                      const IconComponent =
                        flowIconMap[step.action as keyof typeof flowIconMap] || Bot;
                      return (
                        <div key={step.step} className="relative flex items-start gap-3 pl-2">
                          <div
                            className={cn(
                              "relative z-10 flex h-6 w-6 items-center justify-center rounded-full border bg-surface-raised",
                              step.status === "running"
                                ? "border-brand-primary"
                                : step.status === "completed"
                                  ? "border-brand-success"
                                  : "border-error",
                            )}
                          >
                            {step.status === "running" ? (
                              <Loader2 className="h-3 w-3 animate-spin text-brand-primary" />
                            ) : (
                              <IconComponent
                                className={cn(
                                  "h-3 w-3",
                                  step.status === "completed" ? "text-brand-primary" : "text-error",
                                )}
                              />
                            )}
                          </div>
                          <div className="flex flex-1 items-center justify-between">
                            <div>
                              <span className="mr-1.5 text-xs font-medium text-text-muted">
                                {step.step}.
                              </span>
                              <span
                                className={cn(
                                  "text-xs",
                                  step.status === "error" ? "text-error" : "text-text-secondary",
                                )}
                              >
                                {step.description}
                              </span>
                            </div>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}

              {flowError && (
                <div className="mb-4 p-2 bg-error/10 border border-error/30 rounded-lg">
                  <div className="flex items-center gap-2 text-xs text-error">
                    <AlertCircle className="h-3.5 w-3.5" />
                    <span>{flowError}</span>
                  </div>
                </div>
              )}

              {/* Command Bar */}
              <div className="flex flex-col gap-2">
                <textarea
                  value={flowPrompt}
                  onChange={(e) => setFlowPrompt(e.target.value)}
                  placeholder="Describe navigation flow..."
                  rows={5}
                  disabled={isExploring}
                  className="w-full resize-none rounded-lg border border-border bg-surface-canvas px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:border-brand-primary/50 focus:outline-none disabled:opacity-50"
                />
                <button
                  onClick={exploreFlow}
                  disabled={isExploring || connectionStatus !== "connected" || !flowPrompt.trim()}
                  className="w-full flex items-center justify-center gap-1.5 rounded-lg bg-gradient-to-r from-brand-primary to-brand-primary/80 py-3 text-sm font-medium text-white hover:from-brand-primary/90 hover:to-brand-primary/70 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
                >
                  {isExploring ? (
                    <>
                      <Loader2 className="h-4 w-4 animate-spin" />
                      Exploring...
                    </>
                  ) : (
                    <>
                      <Sparkles className="h-4 w-4" />
                      Explore
                    </>
                  )}
                </button>
                {connectionStatus !== "connected" && !isExploring && (
                  <p className="text-[10px] text-warning text-center">Connect to a tab first</p>
                )}
              </div>

              {/* AI Context Selection */}
              <div className="mt-4 p-3 bg-surface-canvas/50 rounded-lg border border-border">
                <ContextSelector
                  selection={contextSelection}
                  onSelectionChange={setContextSelection}
                  taskPrompt={instructions}
                  compact={true}
                  disabled={isGenerating}
                />
              </div>
            </div>
          )}

          {leftPanelTab === "context" && (
            <div className="p-3">
              {multiPageContext && multiPageContext.pages.length > 0 ? (
                <div className="space-y-1">
                  {buildPageTree().map((node) => (
                    <TreeItem
                      key={node.id}
                      node={node}
                      expanded={expandedNodes.has(node.id)}
                      onToggle={() => toggleNode(node.id)}
                      onSelect={() => {
                        const pageIdx = parseInt(node.id.replace("page-", ""));
                        setSelectedPageIndex(pageIdx);
                      }}
                      isSelected={selectedPageIndex === parseInt(node.id.replace("page-", ""))}
                    />
                  ))}
                </div>
              ) : (
                <div className="text-center py-8 text-text-muted">
                  <Layers className="h-8 w-8 mx-auto mb-2 opacity-30" />
                  <p className="text-sm">No pages captured yet</p>
                  <p className="text-xs mt-1">Use Flow Explorer to capture pages</p>
                </div>
              )}
            </div>
          )}

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
              <button
                onClick={handleGenerate}
                disabled={isGenerating || !instructions.trim()}
                className="mt-4 w-full flex items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-brand-secondary to-brand-primary py-3 text-sm font-medium text-white hover:from-brand-secondary/90 hover:to-brand-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
              >
                {isGenerating ? (
                  <>
                    <Loader2 className="h-4 w-4 animate-spin" />
                    Generating...
                  </>
                ) : (
                  <>
                    <Sparkles className="h-4 w-4" />
                    Generate Assets
                  </>
                )}
              </button>

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
              {generatedContent ? (
                <>
                  {/* Sub-tabs */}
                  <div className="flex gap-1 border-b border-border px-3 py-2">
                    <button
                      onClick={() => setOutputSubTab("python")}
                      className={cn(
                        "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
                        outputSubTab === "python"
                          ? "bg-brand-success/10 text-brand-success"
                          : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
                      )}
                    >
                      <FileCode className="h-3.5 w-3.5" />
                      Python Test
                    </button>
                    <button
                      onClick={() => setOutputSubTab("agentic")}
                      className={cn(
                        "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
                        outputSubTab === "agentic"
                          ? "bg-brand-secondary/10 text-brand-secondary"
                          : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
                      )}
                    >
                      <FileCode className="h-3.5 w-3.5" />
                      Agentic Step
                    </button>
                  </div>

                  {/* Code Block */}
                  <div className="flex-1 overflow-auto p-3">
                    <pre className="h-full rounded-lg border border-border bg-surface-raised p-4 text-xs leading-relaxed overflow-auto">
                      <code
                        className={cn(
                          "font-mono",
                          outputSubTab === "python" ? "text-brand-success" : "text-brand-secondary",
                        )}
                      >
                        {outputSubTab === "python"
                          ? generatedContent.verificationTest || "(No test code generated)"
                          : generatedContent.agenticStep || "(No agentic step generated)"}
                      </code>
                    </pre>
                  </div>

                  {/* Actions */}
                  <div className="flex gap-2 border-t border-border p-3">
                    <button
                      onClick={outputSubTab === "python" ? handleSaveTest : handleSaveTask}
                      disabled={
                        outputSubTab === "python"
                          ? isSavingTest || !!savedTestId
                          : isSavingTask || !!savedTaskId
                      }
                      className={cn(
                        "flex-1 flex items-center justify-center gap-2 rounded-lg py-2 text-sm font-medium transition-colors",
                        (outputSubTab === "python" ? savedTestId : savedTaskId)
                          ? "bg-brand-success/20 text-brand-success"
                          : "bg-surface-hover text-text-primary hover:bg-surface-active",
                      )}
                    >
                      {(outputSubTab === "python" ? isSavingTest : isSavingTask) ? (
                        <Loader2 className="h-4 w-4 animate-spin" />
                      ) : (outputSubTab === "python" ? savedTestId : savedTaskId) ? (
                        <CheckCircle2 className="h-4 w-4" />
                      ) : (
                        <Save className="h-4 w-4" />
                      )}
                      {(outputSubTab === "python" ? savedTestId : savedTaskId)
                        ? "Saved"
                        : "Save to Library"}
                    </button>
                    <button
                      onClick={() =>
                        copyToClipboard(
                          outputSubTab === "python"
                            ? generatedContent.verificationTest
                            : generatedContent.agenticStep,
                          outputSubTab === "python" ? "Test code" : "Agentic prompt",
                        )
                      }
                      className="flex items-center justify-center gap-2 px-4 rounded-lg bg-surface-hover text-text-primary hover:bg-surface-active transition-colors"
                    >
                      <Copy className="h-4 w-4" />
                      Copy
                    </button>
                  </div>

                  {/* View in Library */}
                  {(savedTestId || savedTaskId) && onNavigateToLibrary && (
                    <div className="px-3 pb-3">
                      <button
                        onClick={onNavigateToLibrary}
                        className="w-full flex items-center justify-center gap-2 rounded-lg border border-border py-2 text-sm text-text-muted hover:text-text-primary hover:bg-surface-hover transition-colors"
                      >
                        <ExternalLink className="h-4 w-4" />
                        View in Library
                      </button>
                    </div>
                  )}
                </>
              ) : (
                <div className="flex-1 flex items-center justify-center">
                  <div className="text-center text-text-muted max-w-md">
                    <Bot className="w-12 h-12 mx-auto mb-3 opacity-30" />
                    <h3 className="text-base font-medium mb-1">No Output Yet</h3>
                    <p className="text-sm">
                      Fill in the test definition and click "Generate Assets" to create test code
                      and agentic steps.
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

interface TreeItemProps {
  node: TreeNode;
  expanded: boolean;
  onToggle: () => void;
  onSelect: () => void;
  isSelected: boolean;
  depth?: number;
}

function TreeItem({ node, expanded, onToggle, onSelect, isSelected, depth = 0 }: TreeItemProps) {
  const hasChildren = node.children && node.children.length > 0;

  return (
    <div>
      <button
        onClick={() => {
          if (hasChildren) onToggle();
          if (node.type === "page") onSelect();
        }}
        className={cn(
          "w-full flex items-center gap-2 rounded-md px-2 py-1.5 transition-colors hover:bg-surface-hover text-left",
          depth > 0 && "ml-4",
          isSelected && node.type === "page" && "bg-brand-primary/10",
        )}
      >
        {hasChildren ? (
          expanded ? (
            <ChevronDown className="h-3.5 w-3.5 text-text-muted" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5 text-text-muted" />
          )
        ) : (
          <span className="w-3.5" />
        )}

        {node.type === "page" ? (
          <Layout className="h-3.5 w-3.5 text-brand-primary" />
        ) : node.icon ? (
          (() => {
            const Icon = elementIconMap[node.icon];
            return <Icon className="h-3.5 w-3.5 text-text-muted" />;
          })()
        ) : null}

        <span
          className={cn(
            "text-sm flex-1 truncate",
            node.type === "page" ? "text-text-primary font-medium" : "text-text-muted",
          )}
        >
          {node.name}
        </span>

        {node.type === "page" && node.elementCount !== undefined && (
          <span className="text-xs text-brand-success">{node.elementCount} el</span>
        )}
      </button>

      {expanded && hasChildren && (
        <div>
          {node.children!.map((child) => (
            <TreeItem
              key={child.id}
              node={child}
              expanded={false}
              onToggle={() => {}}
              onSelect={() => {}}
              isSelected={false}
              depth={depth + 1}
            />
          ))}
        </div>
      )}
    </div>
  );
}

export default LivePageGeneratorTab;
