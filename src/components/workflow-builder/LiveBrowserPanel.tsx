/**
 * LiveBrowserPanel.tsx
 *
 * Live Browser Mode panel for connecting to open browser tabs, mobile devices,
 * or Tauri apps and generating verification tests + agentic prompts without
 * requiring a pre-built state machine configuration.
 *
 * Features:
 * - Target selection (browser tabs via extension, mobile via ADB)
 * - Live element discovery from the connected page
 * - Element picker for selecting elements to verify
 * - Manual semantic context entry for AI agentic prompts
 * - Playwright test generation from selected elements
 */

import React, { useState, useCallback, useEffect, useMemo } from "react";
import {
  X,
  Loader2,
  AlertCircle,
  RefreshCw,
  Globe,
  Smartphone,
  Monitor,
  ChevronDown,
  ChevronRight,
  Eye,
  MousePointer,
  Play,
  Plus,
  Check,
  Crosshair,
  Wifi,
  WifiOff,
  ExternalLink,
  Bot,
  FileCode,
  Zap,
} from "lucide-react";
import { useLiveBrowser, type DiscoveredElement } from "../../hooks/useLiveBrowser";
import type {
  UnifiedStep,
  WorkflowPhase,
  VerificationStep,
  PromptStep,
} from "../../types/unified-workflow";
import { generateStepId } from "../../types";

// =============================================================================
// Types
// =============================================================================

interface LiveBrowserPanelProps {
  isOpen: boolean;
  onClose: () => void;
  /** Function to add a step to the workflow */
  addStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
  /** Callback when steps are added */
  onStepsAdded?: (steps: {
    verificationSteps: VerificationStep[];
    agenticStep: PromptStep | null;
  }) => void;
}

interface SelectedElementForTest {
  element: DiscoveredElement;
  assertions: ElementAssertion[];
}

interface ElementAssertion {
  type: "visible" | "enabled" | "text" | "value" | "checked";
  expected?: string | boolean;
  enabled: boolean;
}

// =============================================================================
// Test Generator Utilities
// =============================================================================

/**
 * Generate a Playwright locator for an element
 */
function generateLocator(element: DiscoveredElement): string {
  if (element.id) {
    return `page.locator('[data-ui-id="${element.id}"]')`;
  }
  if (element.label) {
    return `page.getByLabel('${element.label.replace(/'/g, "\\'")}')`;
  }
  if (element.text && element.type === "button") {
    return `page.getByRole('button', { name: '${element.text.replace(/'/g, "\\'")}' })`;
  }
  if (element.text && element.type === "link") {
    return `page.getByRole('link', { name: '${element.text.replace(/'/g, "\\'")}' })`;
  }
  return `page.locator('${element.tagName}').filter({ hasText: '${(element.text || "").slice(0, 30).replace(/'/g, "\\'")}' })`;
}

/**
 * Generate Playwright test code for selected elements
 */
function generatePlaywrightTest(
  selectedElements: SelectedElementForTest[],
  testName: string,
  pageUrl?: string,
): string {
  const lines: string[] = [
    `import { test, expect } from '@playwright/test';`,
    ``,
    `test('${testName}', async ({ page }) => {`,
  ];

  if (pageUrl) {
    lines.push(`  // Navigate to the page`);
    lines.push(`  await page.goto('${pageUrl}');`);
    lines.push(``);
  }

  for (const { element, assertions } of selectedElements) {
    const locator = generateLocator(element);
    const elementName =
      element.label || element.id || element.text?.slice(0, 20) || element.tagName;

    lines.push(`  // Verify: ${elementName}`);
    lines.push(`  const ${element.id?.replace(/-/g, "_") || "element"} = ${locator};`);

    for (const assertion of assertions) {
      if (!assertion.enabled) continue;

      switch (assertion.type) {
        case "visible":
          lines.push(
            `  await expect(${element.id?.replace(/-/g, "_") || "element"}).toBeVisible();`,
          );
          break;
        case "enabled":
          lines.push(
            `  await expect(${element.id?.replace(/-/g, "_") || "element"}).toBeEnabled();`,
          );
          break;
        case "text":
          if (assertion.expected) {
            lines.push(
              `  await expect(${element.id?.replace(/-/g, "_") || "element"}).toHaveText('${String(assertion.expected).replace(/'/g, "\\'")}');`,
            );
          }
          break;
        case "value":
          if (assertion.expected !== undefined) {
            lines.push(
              `  await expect(${element.id?.replace(/-/g, "_") || "element"}).toHaveValue('${String(assertion.expected).replace(/'/g, "\\'")}');`,
            );
          }
          break;
        case "checked":
          if (assertion.expected === true) {
            lines.push(
              `  await expect(${element.id?.replace(/-/g, "_") || "element"}).toBeChecked();`,
            );
          } else {
            lines.push(
              `  await expect(${element.id?.replace(/-/g, "_") || "element"}).not.toBeChecked();`,
            );
          }
          break;
      }
    }
    lines.push(``);
  }

  lines.push(`});`);
  return lines.join("\n");
}

/**
 * Generate verification steps for the unified workflow
 */
function generateVerificationSteps(
  selectedElements: SelectedElementForTest[],
  pageUrl?: string,
): VerificationStep[] {
  const steps: VerificationStep[] = [];

  // Create a single Playwright test step with all assertions
  if (selectedElements.length > 0) {
    const testCode = generatePlaywrightTest(selectedElements, "Live Browser Verification", pageUrl);

    steps.push({
      id: generateStepId(),
      type: "test",
      phase: "verification",
      name: "Verify page elements",
      test_type: "playwright",
      code: testCode,
      is_critical: true,
    } as UnifiedStep);
  }

  return steps;
}

/**
 * Generate agentic prompt with user context
 */
function generateAgenticPrompt(
  userDescription: string,
  userInstructions: string,
  selectedElements: SelectedElementForTest[],
  pageUrl?: string,
): PromptStep {
  const elementDescriptions = selectedElements
    .map((e) => {
      const el = e.element;
      return `- ${el.label || el.id || el.tagName}: ${el.type}${el.text ? ` (text: "${el.text.slice(0, 50)}")` : ""}`;
    })
    .join("\n");

  const prompt = `## Page Context
${userDescription}

## Target URL
${pageUrl || "Current browser tab"}

## Key Elements
${elementDescriptions || "No specific elements selected"}

## Instructions
${userInstructions || "Verify the page is in the expected state. If verification fails, investigate and fix the issue."}

## Recovery Actions
If the verification fails:
1. Refresh the page and retry
2. Check if elements are present but with different content
3. Report the discrepancy with screenshots`;

  return {
    id: generateStepId(),
    type: "prompt",
    name: "Live Browser Agentic Task",
    phase: "agentic",
    content: prompt,
    model: "claude-sonnet-4-20250514",
  } as PromptStep;
}

// =============================================================================
// Component
// =============================================================================

export function LiveBrowserPanel({
  isOpen,
  onClose,
  addStep,
  onStepsAdded,
}: LiveBrowserPanelProps) {
  const {
    connectionStatus,
    connectedTarget,
    error,
    browserTabs,
    mobileDevices,
    isLoadingTargets,
    elements,
    isLoadingElements,
    selectedElementId,
    refreshTargets,
    connectToTab,
    connectToMobile,
    disconnect,
    refreshElements,
    selectElement: _selectElement,
    highlightElement,
    enablePicker,
    isExtensionConnected,
  } = useLiveBrowser();

  // UI State
  const [activeTab, setActiveTab] = useState<"targets" | "elements" | "generate">("targets");
  const [expandedSections, setExpandedSections] = useState<Set<string>>(new Set(["browser"]));

  // Selected elements for test generation
  const [selectedForTest, setSelectedForTest] = useState<SelectedElementForTest[]>([]);

  // Manual context entry
  const [pageDescription, setPageDescription] = useState("");
  const [agenticInstructions, setAgenticInstructions] = useState("");

  // Generation options
  const [includeVerification, setIncludeVerification] = useState(true);
  const [includeAgentic, setIncludeAgentic] = useState(true);

  // Refresh targets on open
  useEffect(() => {
    if (isOpen) {
      refreshTargets();
    }
  }, [isOpen, refreshTargets]);

  // Auto-switch to elements tab when connected
  useEffect(() => {
    if (connectionStatus === "connected" && connectedTarget?.type === "browser") {
      setActiveTab("elements");
    }
  }, [connectionStatus, connectedTarget]);

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

  // Toggle element selection for test
  const toggleElementForTest = useCallback((element: DiscoveredElement) => {
    setSelectedForTest((prev) => {
      const existing = prev.find((e) => e.element.id === element.id);
      if (existing) {
        return prev.filter((e) => e.element.id !== element.id);
      }

      // Create default assertions based on element type
      const assertions: ElementAssertion[] = [
        { type: "visible", enabled: true },
        { type: "enabled", enabled: element.type !== "custom" },
      ];

      if (element.text) {
        assertions.push({ type: "text", expected: element.text, enabled: true });
      }
      if (element.value !== undefined) {
        assertions.push({ type: "value", expected: element.value, enabled: true });
      }
      if (element.checked !== undefined) {
        assertions.push({ type: "checked", expected: element.checked, enabled: true });
      }

      return [...prev, { element, assertions }];
    });
  }, []);

  // Update assertion for an element
  const _updateAssertion = useCallback(
    (elementId: string, assertionType: string, updates: Partial<ElementAssertion>) => {
      setSelectedForTest((prev) =>
        prev.map((e) => {
          if (e.element.id !== elementId) return e;
          return {
            ...e,
            assertions: e.assertions.map((a) =>
              a.type === assertionType ? { ...a, ...updates } : a,
            ),
          };
        }),
      );
    },
    [],
  );

  // Check if an element is selected for test
  const isElementSelected = useCallback(
    (elementId: string) => selectedForTest.some((e) => e.element.id === elementId),
    [selectedForTest],
  );

  // Generate and add steps to workflow
  const handleGenerate = useCallback(() => {
    const verificationSteps = includeVerification
      ? generateVerificationSteps(selectedForTest, connectedTarget?.url)
      : [];

    const agenticStep = includeAgentic
      ? generateAgenticPrompt(
          pageDescription,
          agenticInstructions,
          selectedForTest,
          connectedTarget?.url,
        )
      : null;

    // Add verification steps
    for (const step of verificationSteps) {
      addStep(step, "verification");
    }

    // Add agentic step
    if (agenticStep) {
      addStep(agenticStep, "agentic");
    }

    // Callback
    onStepsAdded?.({ verificationSteps, agenticStep });

    onClose();
  }, [
    includeVerification,
    includeAgentic,
    selectedForTest,
    pageDescription,
    agenticInstructions,
    connectedTarget,
    addStep,
    onStepsAdded,
    onClose,
  ]);

  // Preview code
  const previewCode = useMemo(() => {
    if (selectedForTest.length === 0) return "";
    return generatePlaywrightTest(
      selectedForTest,
      "Live Browser Verification",
      connectedTarget?.url,
    );
  }, [selectedForTest, connectedTarget]);

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" onClick={onClose} />

      {/* Panel */}
      <div className="relative w-full max-w-4xl max-h-[90vh] bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-700 flex-shrink-0">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded-lg bg-cyan-500/20">
              <Zap className="w-5 h-5 text-cyan-400" />
            </div>
            <div>
              <h2 className="text-lg font-semibold text-zinc-100">Live Browser Mode</h2>
              <p className="text-sm text-zinc-400">
                Add verification steps from a live browser page
              </p>
            </div>
          </div>

          {/* Connection Status */}
          <div className="flex items-center gap-4">
            {connectedTarget && (
              <div className="flex items-center gap-2 px-3 py-1.5 bg-green-500/10 border border-green-500/30 rounded-lg">
                <div className="w-2 h-2 rounded-full bg-green-500 animate-pulse" />
                <span className="text-sm text-green-400">{connectedTarget.name}</span>
                <button
                  onClick={disconnect}
                  className="p-1 hover:bg-green-500/20 rounded transition-colors"
                  title="Disconnect"
                >
                  <X className="w-3 h-3 text-green-400" />
                </button>
              </div>
            )}

            <button
              onClick={onClose}
              className="p-1.5 rounded-md text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800 transition-colors"
            >
              <X className="w-5 h-5" />
            </button>
          </div>
        </div>

        {/* Tabs */}
        <div className="flex border-b border-zinc-700 flex-shrink-0">
          {[
            { id: "targets", label: "Targets", icon: Globe },
            {
              id: "elements",
              label: "Elements",
              icon: MousePointer,
              disabled: connectionStatus !== "connected",
            },
            {
              id: "generate",
              label: "Generate",
              icon: FileCode,
              disabled: selectedForTest.length === 0,
            },
          ].map((tab) => (
            <button
              key={tab.id}
              onClick={() => !tab.disabled && setActiveTab(tab.id as typeof activeTab)}
              disabled={tab.disabled}
              className={`flex items-center gap-2 px-4 py-3 text-sm font-medium transition-colors ${
                activeTab === tab.id
                  ? "text-cyan-400 border-b-2 border-cyan-400 -mb-px"
                  : tab.disabled
                    ? "text-zinc-600 cursor-not-allowed"
                    : "text-zinc-400 hover:text-zinc-200"
              }`}
            >
              <tab.icon className="w-4 h-4" />
              {tab.label}
              {tab.id === "elements" && elements.length > 0 && (
                <span className="text-xs px-1.5 py-0.5 rounded bg-zinc-700 text-zinc-300">
                  {elements.length}
                </span>
              )}
              {tab.id === "generate" && selectedForTest.length > 0 && (
                <span className="text-xs px-1.5 py-0.5 rounded bg-cyan-500/20 text-cyan-400">
                  {selectedForTest.length}
                </span>
              )}
            </button>
          ))}
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-6">
          {/* Error Display */}
          {error && (
            <div className="flex items-start gap-3 p-4 mb-4 bg-red-500/10 border border-red-500/30 rounded-lg">
              <AlertCircle className="w-5 h-5 text-red-400 flex-shrink-0 mt-0.5" />
              <p className="text-red-400 text-sm">{error}</p>
            </div>
          )}

          {/* Targets Tab */}
          {activeTab === "targets" && (
            <div className="space-y-4">
              {/* Extension Status */}
              <div
                className={`flex items-center gap-3 p-3 rounded-lg ${
                  isExtensionConnected
                    ? "bg-green-500/10 border border-green-500/30"
                    : "bg-amber-500/10 border border-amber-500/30"
                }`}
              >
                {isExtensionConnected ? (
                  <Wifi className="w-5 h-5 text-green-400" />
                ) : (
                  <WifiOff className="w-5 h-5 text-amber-400" />
                )}
                <div className="flex-1">
                  <p
                    className={
                      isExtensionConnected
                        ? "text-green-400 font-medium"
                        : "text-amber-400 font-medium"
                    }
                  >
                    {isExtensionConnected ? "Extension Connected" : "Extension Not Connected"}
                  </p>
                  <p className="text-xs text-zinc-400">
                    {isExtensionConnected
                      ? "Qontinui DevTools extension is connected and ready"
                      : "Install the Qontinui DevTools extension from the extension folder"}
                  </p>
                </div>
                <button
                  onClick={refreshTargets}
                  disabled={isLoadingTargets}
                  className="p-2 hover:bg-white/10 rounded transition-colors"
                  title="Refresh targets"
                >
                  <RefreshCw
                    className={`w-4 h-4 text-zinc-400 ${isLoadingTargets ? "animate-spin" : ""}`}
                  />
                </button>
              </div>

              {/* Browser Tabs */}
              <div className="border border-zinc-700 rounded-lg overflow-hidden">
                <button
                  onClick={() => toggleSection("browser")}
                  className="w-full flex items-center gap-2 px-4 py-3 bg-zinc-800/50 hover:bg-zinc-800 transition-colors"
                >
                  {expandedSections.has("browser") ? (
                    <ChevronDown className="w-4 h-4 text-zinc-400" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-zinc-400" />
                  )}
                  <Globe className="w-4 h-4 text-blue-400" />
                  <span className="font-medium text-zinc-200">Browser Tabs</span>
                  <span className="text-xs text-zinc-500 ml-auto">
                    {browserTabs.length} available
                  </span>
                </button>

                {expandedSections.has("browser") && (
                  <div className="divide-y divide-zinc-800">
                    {isLoadingTargets ? (
                      <div className="flex items-center gap-2 px-4 py-8 justify-center text-zinc-500">
                        <Loader2 className="w-4 h-4 animate-spin" />
                        <span>Loading tabs...</span>
                      </div>
                    ) : browserTabs.length === 0 ? (
                      <div className="px-4 py-8 text-center text-zinc-500">
                        <p>No browser tabs found</p>
                        <p className="text-xs mt-1">
                          Make sure the extension is installed and enabled
                        </p>
                      </div>
                    ) : (
                      browserTabs.map((tab) => (
                        <div
                          key={tab.id}
                          className={`flex items-center gap-3 px-4 py-3 hover:bg-zinc-800/50 transition-colors ${
                            connectedTarget?.type === "browser" && connectedTarget?.id === tab.id
                              ? "bg-cyan-500/10"
                              : ""
                          }`}
                        >
                          {tab.favIconUrl ? (
                            <img src={tab.favIconUrl} className="w-4 h-4" alt="" />
                          ) : (
                            <Globe className="w-4 h-4 text-zinc-500" />
                          )}
                          <div className="flex-1 min-w-0">
                            <p className="text-sm text-zinc-200 truncate">{tab.title}</p>
                            <p className="text-xs text-zinc-500 truncate">{tab.url}</p>
                          </div>
                          {connectedTarget?.type === "browser" && connectedTarget?.id === tab.id ? (
                            <span className="text-xs text-cyan-400 px-2 py-1 rounded bg-cyan-500/20">
                              Connected
                            </span>
                          ) : (
                            <button
                              onClick={() => connectToTab(tab.id)}
                              disabled={connectionStatus === "connecting"}
                              className="flex items-center gap-1 px-3 py-1.5 text-xs font-medium bg-cyan-600 hover:bg-cyan-500 text-white rounded transition-colors disabled:opacity-50"
                            >
                              {connectionStatus === "connecting" ? (
                                <Loader2 className="w-3 h-3 animate-spin" />
                              ) : (
                                <Play className="w-3 h-3" />
                              )}
                              Connect
                            </button>
                          )}
                        </div>
                      ))
                    )}
                  </div>
                )}
              </div>

              {/* Mobile Devices */}
              <div className="border border-zinc-700 rounded-lg overflow-hidden">
                <button
                  onClick={() => toggleSection("mobile")}
                  className="w-full flex items-center gap-2 px-4 py-3 bg-zinc-800/50 hover:bg-zinc-800 transition-colors"
                >
                  {expandedSections.has("mobile") ? (
                    <ChevronDown className="w-4 h-4 text-zinc-400" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-zinc-400" />
                  )}
                  <Smartphone className="w-4 h-4 text-green-400" />
                  <span className="font-medium text-zinc-200">Mobile Devices</span>
                  <span className="text-xs text-zinc-500 ml-auto">
                    {mobileDevices.length} available
                  </span>
                </button>

                {expandedSections.has("mobile") && (
                  <div className="divide-y divide-zinc-800">
                    {mobileDevices.length === 0 ? (
                      <div className="px-4 py-8 text-center text-zinc-500">
                        <p>No mobile devices found</p>
                        <p className="text-xs mt-1">
                          Connect an Android device or start an emulator
                        </p>
                      </div>
                    ) : (
                      mobileDevices.map((device) => (
                        <div
                          key={device.device_id}
                          className={`flex items-center gap-3 px-4 py-3 hover:bg-zinc-800/50 transition-colors ${
                            connectedTarget?.type === "mobile" &&
                            connectedTarget?.id === device.device_id
                              ? "bg-green-500/10"
                              : ""
                          }`}
                        >
                          {device.device_type === "emulator" ? (
                            <Monitor className="w-4 h-4 text-zinc-500" />
                          ) : (
                            <Smartphone className="w-4 h-4 text-zinc-500" />
                          )}
                          <div className="flex-1 min-w-0">
                            <p className="text-sm text-zinc-200">
                              {device.model || device.device_id}
                            </p>
                            <p className="text-xs text-zinc-500">
                              {device.device_type} - {device.state}
                            </p>
                          </div>
                          {connectedTarget?.type === "mobile" &&
                          connectedTarget?.id === device.device_id ? (
                            <span className="text-xs text-green-400 px-2 py-1 rounded bg-green-500/20">
                              Connected
                            </span>
                          ) : (
                            <button
                              onClick={() => connectToMobile(device.device_id)}
                              disabled={
                                device.state !== "device" || connectionStatus === "connecting"
                              }
                              className="flex items-center gap-1 px-3 py-1.5 text-xs font-medium bg-green-600 hover:bg-green-500 text-white rounded transition-colors disabled:opacity-50"
                            >
                              {connectionStatus === "connecting" ? (
                                <Loader2 className="w-3 h-3 animate-spin" />
                              ) : (
                                <Play className="w-3 h-3" />
                              )}
                              Connect
                            </button>
                          )}
                        </div>
                      ))
                    )}
                  </div>
                )}
              </div>

              {/* Tauri Apps (Future) */}
              <div className="border border-zinc-700 rounded-lg overflow-hidden opacity-50">
                <div className="flex items-center gap-2 px-4 py-3 bg-zinc-800/50">
                  <Monitor className="w-4 h-4 text-purple-400" />
                  <span className="font-medium text-zinc-200">Tauri Apps</span>
                  <span className="text-xs text-zinc-500 ml-auto">Coming soon</span>
                </div>
              </div>
            </div>
          )}

          {/* Elements Tab */}
          {activeTab === "elements" && (
            <div className="space-y-4">
              {/* Toolbar */}
              <div className="flex items-center gap-2">
                <button
                  onClick={refreshElements}
                  disabled={isLoadingElements}
                  className="flex items-center gap-2 px-3 py-2 text-sm bg-zinc-800 hover:bg-zinc-700 rounded-lg transition-colors"
                >
                  <RefreshCw className={`w-4 h-4 ${isLoadingElements ? "animate-spin" : ""}`} />
                  Refresh
                </button>
                <button
                  onClick={enablePicker}
                  className="flex items-center gap-2 px-3 py-2 text-sm bg-cyan-600 hover:bg-cyan-500 text-white rounded-lg transition-colors"
                >
                  <Crosshair className="w-4 h-4" />
                  Pick Element
                </button>
                {selectedForTest.length > 0 && (
                  <button
                    onClick={() => setSelectedForTest([])}
                    className="flex items-center gap-2 px-3 py-2 text-sm text-red-400 hover:bg-red-500/10 rounded-lg transition-colors ml-auto"
                  >
                    <X className="w-4 h-4" />
                    Clear Selection ({selectedForTest.length})
                  </button>
                )}
              </div>

              {/* Elements List */}
              {isLoadingElements ? (
                <div className="flex items-center justify-center gap-2 py-12 text-zinc-500">
                  <Loader2 className="w-5 h-5 animate-spin" />
                  <span>Discovering elements...</span>
                </div>
              ) : elements.length === 0 ? (
                <div className="text-center py-12 text-zinc-500">
                  <MousePointer className="w-8 h-8 mx-auto mb-2 opacity-50" />
                  <p>No elements with data-ui-id found</p>
                  <p className="text-xs mt-1">
                    Elements need data-ui-id attributes to be discovered
                  </p>
                </div>
              ) : (
                <div className="space-y-2 max-h-96 overflow-y-auto">
                  {elements.map((element) => (
                    <div
                      key={element.id}
                      className={`flex items-center gap-3 p-3 rounded-lg border transition-colors cursor-pointer ${
                        isElementSelected(element.id)
                          ? "bg-cyan-500/10 border-cyan-500/50"
                          : selectedElementId === element.id
                            ? "bg-zinc-800/50 border-zinc-600"
                            : "bg-zinc-800/30 border-zinc-700 hover:bg-zinc-800/50"
                      }`}
                      onClick={() => toggleElementForTest(element)}
                      onMouseEnter={() => highlightElement(element.id)}
                    >
                      {/* Selection checkbox */}
                      <div
                        className={`w-5 h-5 rounded border-2 flex items-center justify-center transition-colors ${
                          isElementSelected(element.id)
                            ? "bg-cyan-500 border-cyan-500"
                            : "border-zinc-600"
                        }`}
                      >
                        {isElementSelected(element.id) && <Check className="w-3 h-3 text-white" />}
                      </div>

                      {/* Element info */}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="text-sm font-medium text-zinc-200">
                            {element.label || element.id || element.tagName}
                          </span>
                          <span className="text-xs px-1.5 py-0.5 rounded bg-zinc-700 text-zinc-400">
                            {element.type}
                          </span>
                          {!element.visible && (
                            <span className="text-xs px-1.5 py-0.5 rounded bg-amber-500/20 text-amber-400">
                              hidden
                            </span>
                          )}
                          {!element.enabled && (
                            <span className="text-xs px-1.5 py-0.5 rounded bg-red-500/20 text-red-400">
                              disabled
                            </span>
                          )}
                        </div>
                        {element.text && (
                          <p className="text-xs text-zinc-500 truncate mt-0.5">
                            "{element.text.slice(0, 60)}"
                          </p>
                        )}
                      </div>

                      {/* Actions */}
                      <div className="flex items-center gap-1">
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            highlightElement(element.id);
                          }}
                          className="p-1.5 hover:bg-zinc-700 rounded transition-colors"
                          title="Highlight in page"
                        >
                          <Eye className="w-4 h-4 text-zinc-400" />
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}

          {/* Generate Tab */}
          {activeTab === "generate" && (
            <div className="space-y-6">
              {/* Selected Elements Summary */}
              <div>
                <h3 className="text-sm font-medium text-zinc-400 mb-2">
                  Selected Elements ({selectedForTest.length})
                </h3>
                <div className="space-y-2 max-h-32 overflow-y-auto">
                  {selectedForTest.map(({ element }) => (
                    <div
                      key={element.id}
                      className="flex items-center gap-2 px-3 py-2 bg-zinc-800/50 rounded-lg"
                    >
                      <span className="text-sm text-zinc-200">
                        {element.label || element.id || element.tagName}
                      </span>
                      <span className="text-xs text-zinc-500">{element.type}</span>
                      <button
                        onClick={() => toggleElementForTest(element)}
                        className="ml-auto p-1 hover:bg-zinc-700 rounded transition-colors"
                      >
                        <X className="w-3 h-3 text-zinc-400" />
                      </button>
                    </div>
                  ))}
                </div>
              </div>

              {/* Manual Context Entry */}
              <div className="space-y-4">
                <h3 className="text-sm font-medium text-zinc-400 flex items-center gap-2">
                  <Bot className="w-4 h-4" />
                  Semantic Context for AI
                </h3>

                <div>
                  <label className="block text-xs text-zinc-500 mb-1">
                    Page Description (what this page is for)
                  </label>
                  <textarea
                    value={pageDescription}
                    onChange={(e) => setPageDescription(e.target.value)}
                    placeholder="e.g., This is the login page for the application. It should show username and password fields with a submit button."
                    className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-lg text-sm text-zinc-200 placeholder:text-zinc-600 resize-none"
                    rows={3}
                  />
                </div>

                <div>
                  <label className="block text-xs text-zinc-500 mb-1">
                    AI Instructions (what to do if verification fails)
                  </label>
                  <textarea
                    value={agenticInstructions}
                    onChange={(e) => setAgenticInstructions(e.target.value)}
                    placeholder="e.g., If the login button is not visible, scroll down. If there's an error message, capture it and report. Try refreshing the page if elements are missing."
                    className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-lg text-sm text-zinc-200 placeholder:text-zinc-600 resize-none"
                    rows={3}
                  />
                </div>
              </div>

              {/* Generation Options */}
              <div className="space-y-2">
                <h3 className="text-sm font-medium text-zinc-400">Generation Options</h3>

                <label className="flex items-center gap-3 p-3 bg-zinc-800/50 rounded-lg cursor-pointer hover:bg-zinc-800 transition-colors">
                  <input
                    type="checkbox"
                    checked={includeVerification}
                    onChange={(e) => setIncludeVerification(e.target.checked)}
                    className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-cyan-500"
                  />
                  <div>
                    <span className="text-sm text-zinc-200">
                      Include Playwright Verification Tests
                    </span>
                    <p className="text-xs text-zinc-500">
                      Generate test assertions for selected elements
                    </p>
                  </div>
                </label>

                <label className="flex items-center gap-3 p-3 bg-zinc-800/50 rounded-lg cursor-pointer hover:bg-zinc-800 transition-colors">
                  <input
                    type="checkbox"
                    checked={includeAgentic}
                    onChange={(e) => setIncludeAgentic(e.target.checked)}
                    className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-cyan-500"
                  />
                  <div>
                    <span className="text-sm text-zinc-200">Include Agentic Step</span>
                    <p className="text-xs text-zinc-500">
                      Add AI instruction with your semantic context
                    </p>
                  </div>
                </label>
              </div>

              {/* Code Preview */}
              {previewCode && (
                <div>
                  <h3 className="text-sm font-medium text-zinc-400 mb-2">Generated Test Preview</h3>
                  <pre className="p-4 bg-zinc-950 border border-zinc-800 rounded-lg text-xs text-zinc-300 overflow-x-auto max-h-48">
                    {previewCode}
                  </pre>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between gap-3 px-6 py-4 border-t border-zinc-700 flex-shrink-0">
          <div className="text-xs text-zinc-500">
            {connectedTarget && (
              <span>
                Connected to: {connectedTarget.name}
                {connectedTarget.url && (
                  <a
                    href={connectedTarget.url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="ml-2 text-cyan-400 hover:text-cyan-300"
                  >
                    <ExternalLink className="w-3 h-3 inline" />
                  </a>
                )}
              </span>
            )}
          </div>

          <div className="flex items-center gap-3">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm font-medium text-zinc-400 hover:text-zinc-200 transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleGenerate}
              disabled={selectedForTest.length === 0 || (!includeVerification && !includeAgentic)}
              className="flex items-center gap-2 px-4 py-2 text-sm font-medium bg-cyan-600 hover:bg-cyan-500 text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Plus className="w-4 h-4" />
              Add to Workflow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
