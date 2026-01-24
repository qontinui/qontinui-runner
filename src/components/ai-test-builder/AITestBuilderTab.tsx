/**
 * AITestBuilderTab.tsx
 *
 * Dedicated page for AI-powered test and agentic step generation.
 * Connects to live browser pages/apps, captures UI Bridge context,
 * and uses AI to generate:
 *   - Python verification test code
 *   - Agentic step prompts
 *
 * No element selection required - AI interprets user's natural language
 * instructions along with the current page context.
 */

import { useState, useCallback, useEffect } from "react";
import {
  Bot,
  Monitor,
  Smartphone,
  RefreshCw,
  Loader2,
  ChevronRight,
  Globe,
  CheckCircle2,
  AlertCircle,
  Sparkles,
  Save,
  FileText,
  Copy,
  ExternalLink,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useLiveBrowser, type BrowserTab, type MobileDevice } from "@/hooks/useLiveBrowser";

const API_BASE = "http://localhost:9876";

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

interface AITestBuilderTabProps {
  onLog?: (level: string, message: string) => void;
  onNavigateToLibrary?: () => void;
}

type TargetType = "browser" | "mobile" | "none";

export function AITestBuilderTab({ onLog, onNavigateToLibrary }: AITestBuilderTabProps) {
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
      // Build the generation prompt with context
      const contextDescription = uiBridgeContext
        ? `
Current Page Context:
- URL: ${uiBridgeContext.url || "Unknown"}
- Title: ${uiBridgeContext.title || "Unknown"}
- Elements on page: ${uiBridgeContext.elements?.length || 0}
${uiBridgeContext.elements?.slice(0, 20).map((el) => `  - ${el.tagName}${el.id ? `#${el.id}` : ""}: ${el.text || el.label || el.type || ""}`.slice(0, 100)).join("\n") || ""}
${(uiBridgeContext.elements?.length || 0) > 20 ? `  ... and ${(uiBridgeContext.elements?.length || 0) - 20} more elements` : ""}
`
        : "No page context available - generating based on instructions only.";

      const fullPrompt = `${instructions}

${expectedResults ? `Expected Results:\n${expectedResults}` : ""}

${contextDescription}`;

      // Call the AI generation endpoint
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
          page_context: uiBridgeContext
            ? {
                url: uiBridgeContext.url,
                title: uiBridgeContext.title,
                elements: uiBridgeContext.elements,
              }
            : null,
        },
      });

      if (result.success && result.data) {
        setGeneratedContent({
          verificationTest: result.data.verification_test,
          agenticStep: result.data.agentic_step,
          testName: result.data.test_name,
          agenticName: result.data.agentic_name,
        });
        onLog?.("success", "Successfully generated test and agentic step");
      } else {
        throw new Error(result.message || "Generation failed");
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      setGenerationError(errorMsg);
      onLog?.("error", `Generation failed: ${errorMsg}`);
    } finally {
      setIsGenerating(false);
    }
  }, [instructions, expectedResults, uiBridgeContext, onLog]);

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
          <div className="p-2 rounded-lg bg-gradient-to-br from-purple-500/20 to-blue-500/20">
            <Sparkles className="w-6 h-6 text-purple-400" />
          </div>
          <div>
            <h1 className="text-xl font-semibold">AI Test Builder</h1>
            <p className="text-sm text-neutral-400">
              Generate verification tests and agentic steps from natural language
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

              {/* Context Preview */}
              {uiBridgeContext && (
                <div className="p-3 bg-neutral-800/50 rounded-lg border border-neutral-700">
                  <h3 className="text-xs font-medium text-neutral-400 uppercase mb-2">
                    Page Context
                  </h3>
                  <p className="text-sm truncate">{uiBridgeContext.title}</p>
                  <p className="text-xs text-neutral-500 truncate">{uiBridgeContext.url}</p>
                  <p className="text-xs text-neutral-500 mt-1">
                    {uiBridgeContext.elements?.length || 0} elements detected
                  </p>
                </div>
              )}

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
                <pre className="p-4 text-sm font-mono overflow-x-auto max-h-80 overflow-y-auto">
                  <code>{generatedContent.verificationTest}</code>
                </pre>
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
                <pre className="p-4 text-sm whitespace-pre-wrap max-h-80 overflow-y-auto">
                  {generatedContent.agenticStep}
                </pre>
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

export default AITestBuilderTab;
