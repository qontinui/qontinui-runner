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
 */

import { useState, useCallback, useEffect } from "react";
import {
  Bot,
  Monitor,
  Smartphone,
  RefreshCw,
  Loader2,
  Globe,
  CheckCircle2,
  AlertCircle,
  Sparkles,
  Save,
  FileText,
  Copy,
  ExternalLink,
  MousePointer,
  Camera,
  Clock,
  Navigation,
  Type,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useLiveBrowser, type BrowserTab, type MobileDevice } from "@/hooks/useLiveBrowser";
import { ContextSelector } from "@/components/contexts/ContextSelector";
import type { ContextSelection } from "@/types/context";

// Use 127.0.0.1 instead of localhost to force IPv4 (runner only listens on IPv4)
const API_BASE = "http://127.0.0.1:9876";

// =============================================================================
// Flow Capture Types
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

export function LivePageGeneratorTab({ onLog, onNavigateToLibrary }: LivePageGeneratorTabProps) {
  // Target selection state
  const [selectedTargetType, setSelectedTargetType] = useState<TargetType>("none");
  const [selectedTabId, setSelectedTabId] = useState<number | null>(null);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string | null>(null);

  // Instructions state
  const [instructions, setInstructions] = useState("");
  const [expectedResults, setExpectedResults] = useState("");

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
  const [isCapturingContext, setIsCapturingContext] = useState(false);

  // Context selection for AI
  const [contextSelection, setContextSelection] = useState<ContextSelection>({
    autoDetect: true,
    selectedIds: [],
  });

  // AI-driven flow exploration state
  const [flowPrompt, setFlowPrompt] = useState("");
  const [isExploring, setIsExploring] = useState(false);
  const [explorationLog, setExplorationLog] = useState<Array<{
    step: number;
    action: string;
    description: string;
    status: "running" | "completed" | "error";
    error?: string;
  }>>([]);
  const [multiPageContext, setMultiPageContext] = useState<MultiPageContext | null>(null);
  const [flowError, setFlowError] = useState<string | null>(null);

  // Use the live browser hook
  const {
    browserTabs,
    mobileDevices,
    isLoadingTargets,
    isExtensionConnected,
    connectionStatus,
    connectedTarget,
    elements,
    refreshTargets,
    connectToTab,
    connectToMobile,
    disconnect,
    refreshElements,
  } = useLiveBrowser();

  // Refresh targets on mount
  useEffect(() => {
    refreshTargets();
  }, [refreshTargets]);

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
    [connectToTab, onLog]
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
    [connectToMobile, onLog]
  );

  // Capture UI Bridge context from connected target
  const captureContext = useCallback(async () => {
    if (connectionStatus !== "connected") {
      onLog?.("warning", "Not connected to any target");
      return;
    }

    setIsCapturingContext(true);
    try {
      // Get current elements and page info
      await refreshElements();

      // Build context from elements
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
      onLog?.("success", `Captured context: ${elements.length} elements from ${connectedTarget?.name}`);
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

  // Clear exploration results
  const clearExploration = useCallback(() => {
    setExplorationLog([]);
    setMultiPageContext(null);
    setFlowError(null);
  }, []);

  // Capture current page elements via UI Bridge
  const captureCurrentPage = useCallback(async (): Promise<PageCapture | null> => {
    try {
      // Refresh elements first
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

      // Get current tab info
      const tabResponse = await fetch(`${API_BASE}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "getActiveTab",
          params: {},
        }),
      });

      let tabInfo = { url: connectedTarget?.url || "unknown", title: connectedTarget?.name || "Unknown" };
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
        stepIndex: 0, // Will be set by caller
        elements: data.data?.elements || [],
      };
    } catch (error) {
      console.error("[FlowCapture] Failed to capture page:", error);
      throw error;
    }
  }, [connectedTarget]);

  // Execute a click action via UI Bridge
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

  // Execute a type action via UI Bridge
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

  // Wait for a specified duration
  const executeWait = useCallback(async (ms: number): Promise<void> => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  }, []);

  // AI-driven flow exploration - AI decides what to click based on the prompt
  const exploreFlow = useCallback(async () => {
    if (!flowPrompt.trim()) {
      onLog?.("warning", "Please describe the navigation flow");
      return;
    }

    if (connectionStatus !== "connected") {
      onLog?.("warning", "Not connected to a browser tab");
      return;
    }

    setIsExploring(true);
    setFlowError(null);
    setMultiPageContext(null);
    setExplorationLog([]);

    const captures: PageCapture[] = [];
    const maxSteps = 10; // Safety limit

    try {
      // Initial capture
      onLog?.("info", "Starting AI-driven exploration...");
      let stepNum = 0;

      // Capture initial page
      stepNum++;
      setExplorationLog((prev) => [
        ...prev,
        { step: stepNum, action: "capture", description: "Capturing initial page", status: "running" },
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
            ? { ...l, status: "completed", description: `Captured ${initialCapture.elements.length} elements from ${initialCapture.title}` }
            : l
        )
      );
      onLog?.("success", `Captured initial page: ${initialCapture.elements.length} elements`);

      // AI exploration loop
      let done = false;
      while (!done && stepNum < maxSteps) {
        stepNum++;

        // Get current page elements for AI decision
        const currentCapture = await captureCurrentPage();
        if (!currentCapture) {
          throw new Error("Failed to capture current page for AI decision");
        }

        setExplorationLog((prev) => [
          ...prev,
          { step: stepNum, action: "ai_decision", description: "AI analyzing page...", status: "running" },
        ]);

        // Call AI to decide next action
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
            captured_pages: captures.map((c) => ({ url: c.url, title: c.title, element_count: c.elements.length })),
            step_number: stepNum,
          },
        });

        if (!aiResult.success || !aiResult.data) {
          throw new Error(aiResult.message || "AI exploration failed");
        }

        const aiAction = aiResult.data;
        console.log("[FlowExplore] AI decision:", aiAction);

        // Update log with AI decision
        setExplorationLog((prev) =>
          prev.map((l) =>
            l.step === stepNum
              ? { ...l, description: `AI: ${aiAction.reason}`, status: "completed" }
              : l
          )
        );

        if (aiAction.action === "done") {
          onLog?.("success", `AI completed: ${aiAction.reason}`);
          done = true;
          break;
        }

        // Execute the AI's chosen action
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

            // Wait a bit for page to update
            await executeWait(1500);

          } else if (aiAction.action === "type" && aiAction.element_id && aiAction.text) {
            await executeType(aiAction.element_id, aiAction.text);
            onLog?.("success", `Typed: "${aiAction.text}"`);

          } else if (aiAction.action === "wait") {
            await executeWait(aiAction.wait_ms || 2000);
            onLog?.("success", `Waited ${aiAction.wait_ms || 2000}ms`);
          }

          setExplorationLog((prev) =>
            prev.map((l) => (l.step === stepNum ? { ...l, status: "completed" } : l))
          );

          // Capture after action if AI requested it
          if (aiAction.should_capture_after) {
            stepNum++;
            setExplorationLog((prev) => [
              ...prev,
              { step: stepNum, action: "capture", description: "Capturing page after action", status: "running" },
            ]);

            // Wait a bit more for content to load
            await executeWait(1000);

            const newCapture = await captureCurrentPage();
            if (newCapture && newCapture.elements.length > 0) {
              newCapture.stepIndex = captures.length;
              captures.push(newCapture);

              setExplorationLog((prev) =>
                prev.map((l) =>
                  l.step === stepNum
                    ? { ...l, status: "completed", description: `Captured ${newCapture.elements.length} elements from ${newCapture.title}` }
                    : l
                )
              );
              onLog?.("success", `Captured page: ${newCapture.elements.length} elements from ${newCapture.title}`);
            }
          }

        } catch (actionError) {
          const errorMsg = actionError instanceof Error ? actionError.message : String(actionError);
          setExplorationLog((prev) =>
            prev.map((l) => (l.step === stepNum ? { ...l, status: "error", error: errorMsg } : l))
          );
          throw new Error(`Action failed: ${errorMsg}`);
        }
      }

      // Build multi-page context from captures
      if (captures.length > 0) {
        const totalElements = captures.reduce((sum, c) => sum + c.elements.length, 0);
        setMultiPageContext({
          pages: captures,
          totalElements,
        });
        onLog?.("success", `Exploration complete: ${captures.length} page(s), ${totalElements} elements`);
      }

    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      setFlowError(errorMsg);
      onLog?.("error", `Exploration failed: ${errorMsg}`);
    } finally {
      setIsExploring(false);
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
      // Build the user prompt (page context is passed separately for proper formatting)
      const fullPrompt = `${instructions}

${expectedResults ? `Expected Results:\n${expectedResults}` : ""}`;

      // Build page context - use multi-page context if available, otherwise single page
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
        // Multi-page context from flow capture
        console.log("[LivePageGenerator] Using multi-page context:", multiPageContext.pages.length, "pages");
        pageContextForAI = {
          pages: multiPageContext.pages.map((p) => ({
            url: p.url,
            title: p.title,
            elements: p.elements,
          })),
        };
      } else if (uiBridgeContext) {
        // Single page context
        console.log("[LivePageGenerator] Using single page context");
        pageContextForAI = {
          url: uiBridgeContext.url,
          title: uiBridgeContext.title,
          elements: uiBridgeContext.elements,
        };
      }

      // Call the AI generation endpoint
      console.log("[LivePageGenerator] Calling generate_test_and_agentic_step...");
      console.log("[LivePageGenerator] Context IDs:", contextSelection.selectedIds);
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
          context_ids: contextSelection.selectedIds.length > 0 ? contextSelection.selectedIds : null,
        },
      });

      console.log("[LivePageGenerator] Result:", JSON.stringify(result, null, 2));

      if (result.success && result.data) {
        const content = {
          verificationTest: result.data.verification_test || "",
          agenticStep: result.data.agentic_step || "",
          testName: result.data.test_name || "generated_test",
          agenticName: result.data.agentic_name || "Generated Task",
        };
        console.log("[LivePageGenerator] Setting generated content:", content);
        setGeneratedContent(content);
        onLog?.("success", "Successfully generated test and agentic step");
      } else {
        const errMsg = result.message || "Generation failed - no data returned";
        console.error("[LivePageGenerator] Generation failed:", errMsg, result);
        throw new Error(errMsg);
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      setGenerationError(errorMsg);
      onLog?.("error", `Generation failed: ${errorMsg}`);
    } finally {
      setIsGenerating(false);
    }
  }, [instructions, expectedResults, uiBridgeContext, multiPageContext, contextSelection.selectedIds, onLog]);

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
          content: generatedContent.verificationTest,
          category: "ai-generated",
          tags: ["ai-generated", "verification"],
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
    [onLog]
  );

  return (
    <div className="h-full flex flex-col bg-neutral-900">
      {/* Header */}
      <div className="flex-shrink-0 px-6 py-4 border-b border-neutral-700">
        <div className="flex items-center gap-3">
          <div className="p-2 rounded-lg bg-gradient-to-br from-cyan-500/20 to-blue-500/20">
            <Globe className="w-6 h-6 text-cyan-400" />
          </div>
          <div>
            <h1 className="text-xl font-semibold">Live Page Generator</h1>
            <p className="text-sm text-neutral-400">
              Connect to live pages via UI Bridge to generate tests and tasks
            </p>
          </div>
        </div>
      </div>

      <div className="flex-1 flex overflow-hidden">
        {/* Left Panel - Target Selection & Instructions */}
        <div className="w-[400px] flex-shrink-0 border-r border-neutral-700 flex flex-col">
          {/* Target Selection */}
          <div className="p-4 border-b border-neutral-700">
            <div className="flex items-center justify-between mb-3">
              <h2 className="font-medium flex items-center gap-2">
                <Monitor className="w-4 h-4" />
                Target Selection
              </h2>
              <button
                onClick={refreshTargets}
                disabled={isLoadingTargets}
                className="p-1.5 rounded hover:bg-neutral-800 transition-colors"
                title="Refresh targets"
              >
                <RefreshCw className={`w-4 h-4 ${isLoadingTargets ? "animate-spin" : ""}`} />
              </button>
            </div>

            {/* Extension Status */}
            <div
              className={`flex items-center gap-2 text-xs mb-3 px-2 py-1.5 rounded ${
                isExtensionConnected
                  ? "bg-green-500/10 text-green-400"
                  : "bg-yellow-500/10 text-yellow-400"
              }`}
            >
              {isExtensionConnected ? (
                <>
                  <CheckCircle2 className="w-3 h-3" />
                  Extension connected
                </>
              ) : (
                <>
                  <AlertCircle className="w-3 h-3" />
                  Extension not connected
                </>
              )}
            </div>

            {/* Browser Tabs */}
            {browserTabs.length > 0 && (
              <div className="mb-3">
                <h3 className="text-xs font-medium text-neutral-400 uppercase mb-2 flex items-center gap-1.5">
                  <Globe className="w-3 h-3" />
                  Browser Tabs
                </h3>
                <div className="space-y-1 max-h-40 overflow-y-auto">
                  {browserTabs.map((tab) => (
                    <button
                      key={tab.id}
                      onClick={() => handleSelectTab(tab)}
                      className={`w-full text-left p-2 rounded-lg text-sm transition-colors flex items-center gap-2 ${
                        selectedTabId === tab.id
                          ? "bg-purple-500/20 border border-purple-500/50"
                          : "bg-neutral-800 hover:bg-neutral-700"
                      }`}
                    >
                      {tab.favIconUrl && (
                        <img src={tab.favIconUrl} className="w-4 h-4 rounded" alt="" />
                      )}
                      <span className="truncate flex-1">{tab.title || tab.url}</span>
                      {selectedTabId === tab.id && (
                        <CheckCircle2 className="w-4 h-4 text-purple-400 flex-shrink-0" />
                      )}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {/* Mobile Devices */}
            {mobileDevices.length > 0 && (
              <div>
                <h3 className="text-xs font-medium text-neutral-400 uppercase mb-2 flex items-center gap-1.5">
                  <Smartphone className="w-3 h-3" />
                  Mobile Devices
                </h3>
                <div className="space-y-1 max-h-40 overflow-y-auto">
                  {mobileDevices.map((device) => (
                    <button
                      key={device.device_id}
                      onClick={() => handleSelectDevice(device)}
                      className={`w-full text-left p-2 rounded-lg text-sm transition-colors flex items-center gap-2 ${
                        selectedDeviceId === device.device_id
                          ? "bg-purple-500/20 border border-purple-500/50"
                          : "bg-neutral-800 hover:bg-neutral-700"
                      }`}
                    >
                      <Smartphone className="w-4 h-4" />
                      <span className="truncate flex-1">
                        {device.model || device.device_id}
                      </span>
                      <span className="text-xs text-neutral-500">
                        {device.device_type === "emulator" ? "Emulator" : "Physical"}
                      </span>
                      {selectedDeviceId === device.device_id && (
                        <CheckCircle2 className="w-4 h-4 text-purple-400 flex-shrink-0" />
                      )}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {/* No targets message */}
            {browserTabs.length === 0 && mobileDevices.length === 0 && !isLoadingTargets && (
              <div className="text-center py-4 text-neutral-400 text-sm">
                <p>No targets found.</p>
                <p className="text-xs mt-1">
                  Install the Qontinui DevTools extension or connect a mobile device.
                </p>
              </div>
            )}

            {/* Connected Target Info */}
            {connectionStatus === "connected" && connectedTarget && (
              <div className="mt-3 p-2 bg-green-500/10 rounded-lg border border-green-500/30">
                <div className="flex items-center gap-2 text-sm text-green-400">
                  <CheckCircle2 className="w-4 h-4" />
                  <span className="font-medium">Connected</span>
                </div>
                <p className="text-xs text-neutral-400 mt-1 truncate">
                  {connectedTarget.name}
                </p>
                <button
                  onClick={captureContext}
                  disabled={isCapturingContext}
                  className="mt-2 w-full flex items-center justify-center gap-2 px-3 py-1.5 bg-green-500/20 hover:bg-green-500/30 text-green-400 rounded text-xs transition-colors"
                >
                  {isCapturingContext ? (
                    <Loader2 className="w-3 h-3 animate-spin" />
                  ) : (
                    <RefreshCw className="w-3 h-3" />
                  )}
                  Refresh Context
                </button>
              </div>
            )}
          </div>

          {/* Instructions Input */}
          <div className="flex-1 p-4 overflow-y-auto">
            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium mb-2">
                  What should the test do?
                </label>
                <textarea
                  value={instructions}
                  onChange={(e) => setInstructions(e.target.value)}
                  placeholder="Example: Click the 'Start Extraction' button and wait for results to appear. The extraction should complete with more than 3 states..."
                  rows={6}
                  className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-purple-500 resize-y text-sm"
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-2">
                  Expected Results (optional)
                </label>
                <textarea
                  value={expectedResults}
                  onChange={(e) => setExpectedResults(e.target.value)}
                  placeholder="Example: The results table should show at least 3 rows. Each row should have a state name and status..."
                  rows={4}
                  className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-purple-500 resize-y text-sm"
                />
              </div>

              {/* AI Context Selection */}
              <div className="p-3 bg-neutral-800/50 rounded-lg border border-neutral-700">
                <ContextSelector
                  selection={contextSelection}
                  onSelectionChange={setContextSelection}
                  taskPrompt={instructions}
                  compact={false}
                  disabled={isGenerating}
                />
              </div>

              {/* AI-Driven Flow Exploration */}
              <div className="p-3 bg-neutral-800/50 rounded-lg border border-neutral-700">
                <div className="flex items-center justify-between mb-3">
                  <h3 className="text-xs font-medium text-neutral-400 uppercase flex items-center gap-2">
                    <Navigation className="w-3 h-3" />
                    Multi-Page Flow (AI-Driven)
                  </h3>
                  {(explorationLog.length > 0 || multiPageContext) && (
                    <button
                      onClick={clearExploration}
                      className="text-xs text-neutral-500 hover:text-red-400 transition-colors"
                    >
                      Clear
                    </button>
                  )}
                </div>

                <p className="text-xs text-neutral-500 mb-3">
                  Describe the navigation in natural language. AI will explore the pages and capture elements automatically.
                </p>

                {/* Flow Prompt Input */}
                <textarea
                  value={flowPrompt}
                  onChange={(e) => setFlowPrompt(e.target.value)}
                  placeholder="Example: Click the 'Start Extraction' button, wait for the Results tab to load, then capture that page too"
                  rows={3}
                  disabled={isExploring}
                  className="w-full px-3 py-2 bg-neutral-800 border border-neutral-600 rounded-lg focus:outline-none focus:border-cyan-500 resize-none text-sm mb-3"
                />

                {/* Explore Button */}
                <button
                  onClick={exploreFlow}
                  disabled={isExploring || connectionStatus !== "connected" || !flowPrompt.trim()}
                  className="w-full flex items-center justify-center gap-2 px-3 py-2 bg-cyan-500/20 hover:bg-cyan-500/30 disabled:opacity-50 disabled:cursor-not-allowed text-cyan-400 rounded-lg text-sm transition-colors"
                >
                  {isExploring ? (
                    <>
                      <Loader2 className="w-4 h-4 animate-spin" />
                      AI Exploring...
                    </>
                  ) : (
                    <>
                      <Sparkles className="w-4 h-4" />
                      Explore with AI
                    </>
                  )}
                </button>

                {/* Exploration Log */}
                {explorationLog.length > 0 && (
                  <div className="mt-3 space-y-1 max-h-40 overflow-y-auto">
                    {explorationLog.map((log) => (
                      <div
                        key={log.step}
                        className={`flex items-center gap-2 p-2 rounded text-xs ${
                          log.status === "running"
                            ? "bg-blue-500/20 border border-blue-500/50"
                            : log.status === "completed"
                            ? "bg-green-500/10"
                            : "bg-red-500/10"
                        }`}
                      >
                        <span className="text-neutral-500 w-4">{log.step}.</span>
                        {log.action === "capture" && <Camera className="w-3 h-3 text-cyan-400" />}
                        {log.action === "click" && <MousePointer className="w-3 h-3 text-purple-400" />}
                        {log.action === "type" && <Type className="w-3 h-3 text-green-400" />}
                        {log.action === "wait" && <Clock className="w-3 h-3 text-yellow-400" />}
                        {log.action === "ai_decision" && <Bot className="w-3 h-3 text-blue-400" />}
                        <span className="flex-1 truncate">{log.description}</span>
                        {log.status === "running" && <Loader2 className="w-3 h-3 animate-spin text-blue-400" />}
                        {log.status === "completed" && <CheckCircle2 className="w-3 h-3 text-green-400" />}
                        {log.status === "error" && <AlertCircle className="w-3 h-3 text-red-400" />}
                      </div>
                    ))}
                  </div>
                )}

                {/* Flow Error */}
                {flowError && (
                  <div className="mt-3 p-2 bg-red-500/10 border border-red-500/30 rounded text-xs text-red-400">
                    <p className="font-medium mb-1">Exploration failed</p>
                    <p>{flowError}</p>
                    <p className="mt-2 text-neutral-400">
                      Try refining your prompt to be more specific about which elements to interact with.
                    </p>
                  </div>
                )}
              </div>

              {/* Page Context Preview */}
              {multiPageContext && multiPageContext.pages.length > 0 ? (
                <div className="p-3 bg-green-500/10 rounded-lg border border-green-500/30">
                  <h3 className="text-xs font-medium text-green-400 uppercase mb-2 flex items-center gap-2">
                    <CheckCircle2 className="w-3 h-3" />
                    Multi-Page Context Captured
                  </h3>
                  <div className="space-y-2">
                    {multiPageContext.pages.map((page, i) => (
                      <div key={i} className="text-xs">
                        <p className="text-neutral-300 font-medium truncate">
                          {i + 1}. {page.title}
                        </p>
                        <p className="text-neutral-500 truncate">{page.url}</p>
                        <p className="text-green-400">{page.elements.length} elements</p>
                      </div>
                    ))}
                  </div>
                  <p className="text-xs text-green-400 mt-2 font-medium">
                    Total: {multiPageContext.totalElements} elements from {multiPageContext.pages.length} page(s)
                  </p>
                </div>
              ) : uiBridgeContext ? (
                <div className="p-3 bg-neutral-800/50 rounded-lg border border-neutral-700">
                  <h3 className="text-xs font-medium text-neutral-400 uppercase mb-2">
                    Current Page Context
                  </h3>
                  <p className="text-sm truncate">{uiBridgeContext.title}</p>
                  <p className="text-xs text-neutral-500 truncate">{uiBridgeContext.url}</p>
                  <p className="text-xs text-neutral-500 mt-1">
                    {uiBridgeContext.elements?.length || 0} elements detected
                  </p>
                  <p className="text-xs text-yellow-400 mt-2">
                    Tip: Use Flow Capture above to collect elements from multiple pages.
                  </p>
                </div>
              ) : null}

              {/* Generate Button */}
              <button
                onClick={handleGenerate}
                disabled={isGenerating || !instructions.trim()}
                className="w-full flex items-center justify-center gap-2 px-4 py-3 bg-gradient-to-r from-purple-500 to-blue-500 hover:from-purple-600 hover:to-blue-600 disabled:from-neutral-600 disabled:to-neutral-600 disabled:cursor-not-allowed text-white font-medium rounded-lg transition-all"
              >
                {isGenerating ? (
                  <>
                    <Loader2 className="w-5 h-5 animate-spin" />
                    Generating with AI...
                  </>
                ) : (
                  <>
                    <Bot className="w-5 h-5" />
                    Generate Test & Agentic Step
                  </>
                )}
              </button>
            </div>
          </div>
        </div>

        {/* Right Panel - Generated Content */}
        <div className="flex-1 flex flex-col overflow-hidden">
          {generationError && (
            <div className="m-4 p-4 bg-red-500/10 border border-red-500/30 rounded-lg">
              <div className="flex items-start gap-3">
                <AlertCircle className="w-5 h-5 text-red-400 flex-shrink-0 mt-0.5" />
                <div>
                  <h3 className="font-medium text-red-400">Generation Failed</h3>
                  <p className="text-sm text-neutral-400 mt-1">{generationError}</p>
                </div>
              </div>
            </div>
          )}

          {!generatedContent && !isGenerating && !generationError && (
            <div className="flex-1 flex items-center justify-center">
              <div className="text-center text-neutral-400 max-w-md">
                <Bot className="w-16 h-16 mx-auto mb-4 opacity-30" />
                <h3 className="text-lg font-medium mb-2">Ready to Generate</h3>
                <p className="text-sm">
                  Select a target, describe what your test should do, and let AI generate
                  the verification test and agentic step for you.
                </p>
              </div>
            </div>
          )}

          {isGenerating && (
            <div className="flex-1 flex items-center justify-center">
              <div className="text-center">
                <Loader2 className="w-12 h-12 mx-auto mb-4 animate-spin text-purple-400" />
                <h3 className="text-lg font-medium mb-2">Generating...</h3>
                <p className="text-sm text-neutral-400">
                  AI is creating your test and agentic step
                </p>
              </div>
            </div>
          )}

          {generatedContent && (
            <div className="flex-1 overflow-y-auto p-4 space-y-4">
              {/* Verification Test */}
              <div className="bg-neutral-800/50 rounded-lg border border-neutral-700">
                <div className="flex items-center justify-between px-4 py-3 border-b border-neutral-700">
                  <div className="flex items-center gap-2">
                    <FileText className="w-4 h-4 text-green-400" />
                    <h3 className="font-medium">Verification Test</h3>
                    <span className="text-xs text-neutral-500">
                      {generatedContent.testName}
                    </span>
                  </div>
                  <div className="flex items-center gap-2">
                    <button
                      onClick={() =>
                        copyToClipboard(generatedContent.verificationTest, "Test code")
                      }
                      className="p-1.5 rounded hover:bg-neutral-700 transition-colors"
                      title="Copy to clipboard"
                    >
                      <Copy className="w-4 h-4" />
                    </button>
                    <button
                      onClick={handleSaveTest}
                      disabled={isSavingTest || !!savedTestId}
                      className={`flex items-center gap-1.5 px-3 py-1.5 rounded text-sm transition-colors ${
                        savedTestId
                          ? "bg-green-500/20 text-green-400"
                          : "bg-neutral-700 hover:bg-neutral-600"
                      }`}
                    >
                      {isSavingTest ? (
                        <Loader2 className="w-3.5 h-3.5 animate-spin" />
                      ) : savedTestId ? (
                        <CheckCircle2 className="w-3.5 h-3.5" />
                      ) : (
                        <Save className="w-3.5 h-3.5" />
                      )}
                      {savedTestId ? "Saved" : "Save to Library"}
                    </button>
                  </div>
                </div>
                <div className="p-4 max-h-96 overflow-auto bg-neutral-900">
                  <pre className="text-sm font-mono text-green-300 whitespace-pre-wrap break-words">
                    {generatedContent.verificationTest || "(No test code generated)"}
                  </pre>
                </div>
              </div>

              {/* Agentic Step */}
              <div className="bg-neutral-800/50 rounded-lg border border-neutral-700">
                <div className="flex items-center justify-between px-4 py-3 border-b border-neutral-700">
                  <div className="flex items-center gap-2">
                    <Bot className="w-4 h-4 text-purple-400" />
                    <h3 className="font-medium">Agentic Step</h3>
                    <span className="text-xs text-neutral-500">
                      {generatedContent.agenticName}
                    </span>
                  </div>
                  <div className="flex items-center gap-2">
                    <button
                      onClick={() =>
                        copyToClipboard(generatedContent.agenticStep, "Agentic prompt")
                      }
                      className="p-1.5 rounded hover:bg-neutral-700 transition-colors"
                      title="Copy to clipboard"
                    >
                      <Copy className="w-4 h-4" />
                    </button>
                    <button
                      onClick={handleSaveTask}
                      disabled={isSavingTask || !!savedTaskId}
                      className={`flex items-center gap-1.5 px-3 py-1.5 rounded text-sm transition-colors ${
                        savedTaskId
                          ? "bg-green-500/20 text-green-400"
                          : "bg-neutral-700 hover:bg-neutral-600"
                      }`}
                    >
                      {isSavingTask ? (
                        <Loader2 className="w-3.5 h-3.5 animate-spin" />
                      ) : savedTaskId ? (
                        <CheckCircle2 className="w-3.5 h-3.5" />
                      ) : (
                        <Save className="w-3.5 h-3.5" />
                      )}
                      {savedTaskId ? "Saved" : "Save to Library"}
                    </button>
                  </div>
                </div>
                <div className="p-4 max-h-96 overflow-auto bg-neutral-900">
                  <pre className="text-sm whitespace-pre-wrap break-words text-purple-300">
                    {generatedContent.agenticStep || "(No agentic step generated)"}
                  </pre>
                </div>
              </div>

              {/* Actions */}
              {(savedTestId || savedTaskId) && (
                <div className="flex items-center justify-end gap-3">
                  {onNavigateToLibrary && (
                    <button
                      onClick={onNavigateToLibrary}
                      className="flex items-center gap-2 px-4 py-2 bg-neutral-700 hover:bg-neutral-600 rounded-lg text-sm transition-colors"
                    >
                      <ExternalLink className="w-4 h-4" />
                      View in Library
                    </button>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export default LivePageGeneratorTab;
