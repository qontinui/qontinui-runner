/**
 * AccessibilityExplorerPanel.tsx
 *
 * Main panel for accessibility tree exploration.
 * Features:
 * - "Capture Tree" button to trigger accessibility capture
 * - Connection status (CDP connected/disconnected)
 * - Tree viewer integration
 * - Node details panel
 * - Ref list for easy copying
 */

import { useState, useMemo, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Accessibility,
  RefreshCw,
  Wifi,
  WifiOff,
  Copy,
  Check,
  ChevronRight,
  ChevronDown,
  Settings2,
  AlertCircle,
  Info,
  Scan,
  Play,
  Unplug,
  MousePointer,
  Focus,
  Type,
  Chrome,
  FolderOpen,
  HelpCircle,
} from "lucide-react";
import { useAccessibilityTree } from "../../hooks/useAccessibilityTree";
import { AccessibilityTreeViewer } from "./AccessibilityTreeViewer";
import type { AccessibilityNode } from "../../types/accessibility";
import { getAccentColors, getStatusColors } from "@/design-system";

/**
 * Node details panel showing all properties of a selected node.
 */
function NodeDetailsPanel({
  node,
  onCopyRef,
  onClickRef,
  onFillRef,
  onFocusRef,
}: {
  node: AccessibilityNode;
  onCopyRef: (ref: string) => void;
  onClickRef: (ref: string) => Promise<{ success: boolean; error?: string }>;
  onFillRef: (ref: string, value: string) => Promise<{ success: boolean; error?: string }>;
  onFocusRef: (ref: string) => Promise<{ success: boolean; error?: string }>;
}) {
  const [copySuccess, setCopySuccess] = useState(false);
  const [actionResult, setActionResult] = useState<{ success: boolean; error?: string } | null>(
    null,
  );
  const [fillValue, setFillValue] = useState("");
  const [showFillInput, setShowFillInput] = useState(false);

  const handleCopy = () => {
    navigator.clipboard.writeText(node.ref);
    onCopyRef(node.ref);
    setCopySuccess(true);
    setTimeout(() => setCopySuccess(false), 1500);
  };

  const handleClick = async () => {
    setActionResult(null);
    const result = await onClickRef(node.ref);
    setActionResult(result);
    setTimeout(() => setActionResult(null), 3000);
  };

  const handleFocus = async () => {
    setActionResult(null);
    const result = await onFocusRef(node.ref);
    setActionResult(result);
    setTimeout(() => setActionResult(null), 3000);
  };

  const handleFill = async () => {
    if (!fillValue) return;
    setActionResult(null);
    const result = await onFillRef(node.ref, fillValue);
    setActionResult(result);
    setShowFillInput(false);
    setFillValue("");
    setTimeout(() => setActionResult(null), 3000);
  };

  const isTextInput = ["textbox", "searchbox", "combobox"].includes(node.role);

  return (
    <div className="border-t border-border p-3 space-y-3 bg-muted/20">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span
            className={`px-2 py-0.5 text-xs font-mono rounded ${getAccentColors("blue").text} bg-primary/10`}
          >
            {node.ref}
          </span>
          <span className="text-xs text-muted-foreground">{node.role}</span>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={handleCopy}
            className="p-1.5 rounded hover:bg-muted transition-colors"
            title="Copy ref"
          >
            {copySuccess ? (
              <Check className={`w-3.5 h-3.5 ${getStatusColors("success").icon}`} />
            ) : (
              <Copy className="w-3.5 h-3.5 text-muted-foreground" />
            )}
          </button>
        </div>
      </div>

      {/* Properties */}
      <div className="space-y-1.5 text-xs">
        {node.name && (
          <div className="flex gap-2">
            <span className="text-muted-foreground w-16 shrink-0">Name:</span>
            <span className="text-foreground break-all">{node.name}</span>
          </div>
        )}
        {node.value && (
          <div className="flex gap-2">
            <span className="text-muted-foreground w-16 shrink-0">Value:</span>
            <span className="text-foreground break-all">{node.value}</span>
          </div>
        )}
        {node.description && (
          <div className="flex gap-2">
            <span className="text-muted-foreground w-16 shrink-0">Desc:</span>
            <span className="text-foreground break-all">{node.description}</span>
          </div>
        )}
        <div className="flex gap-2">
          <span className="text-muted-foreground w-16 shrink-0">Interactive:</span>
          <span
            className={
              node.is_interactive ? getAccentColors("green").text : "text-muted-foreground"
            }
          >
            {node.is_interactive ? "Yes" : "No"}
          </span>
        </div>
        {node.bounds && (
          <div className="flex gap-2">
            <span className="text-muted-foreground w-16 shrink-0">Bounds:</span>
            <span className="text-foreground font-mono">
              ({Math.round(node.bounds.x)}, {Math.round(node.bounds.y)}){" "}
              {Math.round(node.bounds.width)}x{Math.round(node.bounds.height)}
            </span>
          </div>
        )}
        {node.state && (
          <div className="flex gap-2">
            <span className="text-muted-foreground w-16 shrink-0">State:</span>
            <span className="text-foreground font-mono text-[10px]">
              {Object.entries(node.state)
                .filter(([, v]) => v)
                .map(([k]) => k.replace("is_", ""))
                .join(", ") || "none"}
            </span>
          </div>
        )}
      </div>

      {/* Actions */}
      {node.is_interactive && (
        <div className="pt-2 border-t border-border/50">
          <div className="flex items-center gap-2 flex-wrap">
            <button
              onClick={handleClick}
              className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs bg-primary/10 hover:bg-primary/20 text-primary rounded transition-colors"
            >
              <MousePointer className="w-3 h-3" />
              Click
            </button>
            <button
              onClick={handleFocus}
              className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs bg-muted hover:bg-muted/80 text-foreground rounded transition-colors"
            >
              <Focus className="w-3 h-3" />
              Focus
            </button>
            {isTextInput && (
              <button
                onClick={() => setShowFillInput(!showFillInput)}
                className={`flex items-center gap-1.5 px-2.5 py-1.5 text-xs rounded transition-colors ${
                  showFillInput
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted hover:bg-muted/80 text-foreground"
                }`}
              >
                <Type className="w-3 h-3" />
                Fill
              </button>
            )}
          </div>

          {/* Fill input */}
          {showFillInput && (
            <div className="mt-2 flex items-center gap-2">
              <input
                type="text"
                value={fillValue}
                onChange={(e) => setFillValue(e.target.value)}
                placeholder="Text to type..."
                className="flex-1 px-2 py-1.5 text-xs bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-primary/50"
                onKeyDown={(e) => {
                  if (e.key === "Enter") handleFill();
                  if (e.key === "Escape") setShowFillInput(false);
                }}
                autoFocus
              />
              <button
                onClick={handleFill}
                disabled={!fillValue}
                className="px-2.5 py-1.5 text-xs bg-primary text-primary-foreground rounded disabled:opacity-50 transition-colors"
              >
                Type
              </button>
            </div>
          )}

          {/* Action result feedback */}
          {actionResult && (
            <div
              className={`mt-2 px-2 py-1.5 rounded text-xs ${
                actionResult.success
                  ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                  : `${getStatusColors("error").bg} ${getStatusColors("error").text}`
              }`}
            >
              {actionResult.success ? "Action completed" : actionResult.error || "Action failed"}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Ref list panel for quick copying of interactive refs.
 */
function RefListPanel({
  nodes,
  onCopyRef,
  onCopyAll,
}: {
  nodes: Array<{ ref: string; role: string; name?: string }>;
  onCopyRef: (ref: string) => void;
  onCopyAll: () => void;
}) {
  const [copiedRef, setCopiedRef] = useState<string | null>(null);
  const [copiedAll, setCopiedAll] = useState(false);

  const handleCopy = (ref: string) => {
    navigator.clipboard.writeText(ref);
    onCopyRef(ref);
    setCopiedRef(ref);
    setTimeout(() => setCopiedRef(null), 1000);
  };

  const handleCopyAll = () => {
    onCopyAll();
    setCopiedAll(true);
    setTimeout(() => setCopiedAll(false), 1500);
  };

  return (
    <div className="border-t border-border">
      <div className="flex items-center justify-between px-3 py-2 bg-muted/30">
        <span className="text-xs font-medium text-muted-foreground">
          Interactive Refs ({nodes.length})
        </span>
        <button
          onClick={handleCopyAll}
          className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted rounded transition-colors"
        >
          {copiedAll ? (
            <Check className={`w-3 h-3 ${getStatusColors("success").icon}`} />
          ) : (
            <Copy className="w-3 h-3" />
          )}
          Copy All
        </button>
      </div>
      <div className="max-h-40 overflow-y-auto p-2 space-y-1">
        {nodes.map((node) => (
          <button
            key={node.ref}
            onClick={() => handleCopy(node.ref)}
            className="w-full flex items-center gap-2 px-2 py-1 text-xs hover:bg-muted/50 rounded transition-colors text-left"
          >
            <span className={`font-mono ${getAccentColors("green").text}`}>{node.ref}</span>
            <span className="text-muted-foreground">{node.role}</span>
            {node.name && <span className="text-foreground truncate flex-1">{node.name}</span>}
            {copiedRef === node.ref && (
              <Check className={`w-3 h-3 ${getStatusColors("success").icon}`} />
            )}
          </button>
        ))}
      </div>
    </div>
  );
}

/**
 * Info popup component for explaining Chrome DevTools requirement.
 */
function ChromeInfoPopup({ onClose }: { onClose: () => void }) {
  const popupRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (popupRef.current && !popupRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, [onClose]);

  return (
    <div
      ref={popupRef}
      className="absolute left-0 top-full mt-2 z-50 w-80 p-4 bg-popover border border-border rounded-lg shadow-lg"
    >
      <div className="space-y-3">
        <div className="flex items-start gap-2">
          <Info className="w-4 h-4 text-primary shrink-0 mt-0.5" />
          <div>
            <h4 className="font-medium text-sm">Why Remote Debugging?</h4>
            <p className="text-xs text-muted-foreground mt-1">
              Chrome DevTools Protocol (CDP) allows Qontinui to inspect and interact with web pages
              programmatically. This requires Chrome to be launched with remote debugging enabled.
            </p>
          </div>
        </div>

        <div className="border-t border-border/50 pt-3">
          <h5 className="text-xs font-medium mb-2">Features that require CDP:</h5>
          <ul className="space-y-1.5 text-xs text-muted-foreground">
            <li className="flex items-start gap-2">
              <span className="text-primary">•</span>
              <span>
                <strong>Accessibility Tree Capture</strong> - Read page structure and element refs
              </span>
            </li>
            <li className="flex items-start gap-2">
              <span className="text-primary">•</span>
              <span>
                <strong>Click/Fill/Focus by Ref</strong> - Interact with elements using @e1, @e2
                refs
              </span>
            </li>
            <li className="flex items-start gap-2">
              <span className="text-primary">•</span>
              <span>
                <strong>AI Context Generation</strong> - Create page summaries for AI analysis
              </span>
            </li>
            <li className="flex items-start gap-2">
              <span className="text-primary">•</span>
              <span>
                <strong>Element Bounds Detection</strong> - Get precise element coordinates
              </span>
            </li>
          </ul>
        </div>

        <div className="border-t border-border/50 pt-3 space-y-2">
          <p className="text-[10px] text-muted-foreground">
            <strong>Separate Profile:</strong> A dedicated browser profile is used, so this Chrome
            instance runs independently from your regular browser. You don't need to close existing
            Chrome windows.
          </p>
          <p className="text-[10px] text-muted-foreground">
            <strong>Tip:</strong> The debug Chrome will open empty. Navigate to the page you want to
            inspect, then click "Auto Connect" or "Refresh Tree".
          </p>
        </div>
      </div>
    </div>
  );
}

interface AccessibilityExplorerPanelProps {
  /** Optional task run ID for filtering */
  taskRunId?: string;
}

/**
 * Main accessibility explorer panel.
 */
interface ChromeAvailability {
  available: boolean;
  path: string | null;
  cdp_port: number;
}

interface AccessibilitySettingsData {
  chrome_path: string | null;
  cdp_port: number;
  auto_detected_path: string | null;
}

export function AccessibilityExplorerPanel({
  taskRunId: _taskRunId,
}: AccessibilityExplorerPanelProps) {
  const {
    snapshot,
    aiContext,
    isLoading,
    error,
    connection,
    availablePorts,
    isScanning,
    captureTree,
    autoConnect,
    disconnect,
    scanPorts,
    clickRef,
    fillRef,
    focusRef,
  } = useAccessibilityTree();

  const [selectedNode, setSelectedNode] = useState<AccessibilityNode | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [showRefList, setShowRefList] = useState(true);
  const [showAIContext, setShowAIContext] = useState(false);
  const [cdpPort, setCdpPort] = useState(9222);
  const [copiedContext, setCopiedContext] = useState(false);

  // Chrome launch state
  const [isLaunchingChrome, setIsLaunchingChrome] = useState(false);
  const [chromeAvailability, setChromeAvailability] = useState<ChromeAvailability | null>(null);
  const [chromePath, setChromePath] = useState<string>("");
  const [launchError, setLaunchError] = useState<string | null>(null);
  const [launchSuccess, setLaunchSuccess] = useState(false);
  const [showChromeInfo, setShowChromeInfo] = useState(false);

  // Check Chrome availability on mount
  useEffect(() => {
    const checkChrome = async () => {
      try {
        const result = await invoke<{ success: boolean; data?: ChromeAvailability }>(
          "check_chrome_available",
        );
        if (result.success && result.data) {
          setChromeAvailability(result.data);
          if (result.data.cdp_port) {
            setCdpPort(result.data.cdp_port);
          }
        }
      } catch (err) {
        console.error("Failed to check Chrome availability:", err);
      }
    };

    const loadSettings = async () => {
      try {
        const result = await invoke<{ success: boolean; data?: AccessibilitySettingsData }>(
          "get_accessibility_settings",
        );
        if (result.success && result.data) {
          if (result.data.chrome_path) {
            setChromePath(result.data.chrome_path);
          } else if (result.data.auto_detected_path) {
            setChromePath(result.data.auto_detected_path);
          }
          if (result.data.cdp_port) {
            setCdpPort(result.data.cdp_port);
          }
        }
      } catch (err) {
        console.error("Failed to load accessibility settings:", err);
      }
    };

    checkChrome();
    loadSettings();
  }, []);

  // Launch Chrome with remote debugging
  const handleLaunchChrome = async () => {
    setIsLaunchingChrome(true);
    setLaunchError(null);
    setLaunchSuccess(false);

    try {
      const result = await invoke<{ success: boolean; message?: string; data?: { port: number } }>(
        "launch_chrome_debug",
        { port: cdpPort },
      );

      if (result.success) {
        setLaunchSuccess(true);
        setTimeout(() => setLaunchSuccess(false), 3000);
        // Wait a moment for Chrome to start, then try to connect
        setTimeout(async () => {
          await scanPorts();
        }, 2000);
      } else {
        setLaunchError(result.message || "Failed to launch Chrome");
      }
    } catch (err) {
      setLaunchError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsLaunchingChrome(false);
    }
  };

  // Save Chrome path setting
  const handleSaveChromePath = async () => {
    try {
      await invoke("save_accessibility_settings", {
        chromePath: chromePath || null,
        cdpPort,
      });
      // Refresh availability check
      const result = await invoke<{ success: boolean; data?: ChromeAvailability }>(
        "check_chrome_available",
      );
      if (result.success && result.data) {
        setChromeAvailability(result.data);
      }
    } catch (err) {
      console.error("Failed to save accessibility settings:", err);
    }
  };

  // Extract interactive nodes for ref list
  const interactiveNodes = useMemo(() => {
    if (!snapshot?.root) return [];

    const nodes: Array<{ ref: string; role: string; name?: string }> = [];
    const collect = (node: AccessibilityNode) => {
      if (node.is_interactive) {
        nodes.push({ ref: node.ref, role: node.role, name: node.name });
      }
      node.children?.forEach(collect);
    };
    collect(snapshot.root);
    return nodes;
  }, [snapshot]);

  const handleCapture = async () => {
    await captureTree({ cdp_port: cdpPort });
  };

  const handleAutoConnect = async () => {
    await autoConnect();
  };

  const handleDisconnect = async () => {
    await disconnect();
    setSelectedNode(null);
  };

  const handleCopyRef = useCallback((ref: string) => {
    console.log("Copied ref:", ref);
  }, []);

  const handleCopyAllRefs = useCallback(() => {
    const refs = interactiveNodes.map((n) => n.ref).join("\n");
    navigator.clipboard.writeText(refs);
  }, [interactiveNodes]);

  const handleCopyAIContext = useCallback(() => {
    if (aiContext) {
      navigator.clipboard.writeText(aiContext);
      setCopiedContext(true);
      setTimeout(() => setCopiedContext(false), 1500);
    }
  }, [aiContext]);

  const handleClickAction = useCallback(
    async (ref: string) => {
      const result = await clickRef(ref);
      if (!result.success) {
        console.error("Click failed:", result.error);
      }
    },
    [clickRef],
  );

  const handleFillAction = useCallback(
    async (ref: string) => {
      const value = prompt("Enter text to type:");
      if (value) {
        const result = await fillRef(ref, value);
        if (!result.success) {
          console.error("Fill failed:", result.error);
        }
      }
    },
    [fillRef],
  );

  const handleFocusAction = useCallback(
    async (ref: string) => {
      const result = await focusRef(ref);
      if (!result.success) {
        console.error("Focus failed:", result.error);
      }
    },
    [focusRef],
  );

  return (
    <div data-ui-id="a11y-explorer-panel" className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-border flex-shrink-0">
        <div className="flex items-center gap-2">
          <Accessibility className="w-5 h-5 text-primary" />
          <div>
            <h3 className="font-medium text-sm">Accessibility Explorer</h3>
            <p className="text-xs text-muted-foreground">
              Explore and interact with the accessibility tree
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {/* Connection status */}
          <div
            className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs ${
              connection.isConnected
                ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                : "bg-muted text-muted-foreground"
            }`}
          >
            {connection.isConnected ? (
              <>
                <Wifi className="w-3 h-3" />
                <span>Connected</span>
                {connection.port && <span className="opacity-70">:{connection.port}</span>}
              </>
            ) : (
              <>
                <WifiOff className="w-3 h-3" />
                <span>Disconnected</span>
              </>
            )}
          </div>
          <button
            data-ui-id="a11y-settings-btn"
            onClick={() => setShowSettings(!showSettings)}
            className={`p-1.5 rounded transition-colors ${
              showSettings ? "bg-primary/20 text-primary" : "hover:bg-muted text-muted-foreground"
            }`}
          >
            <Settings2 className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Settings Panel */}
      {showSettings && (
        <div className="p-3 border-b border-border bg-muted/20 space-y-3">
          {/* Chrome Path Setting */}
          <div className="space-y-2">
            <label className="text-xs font-medium text-muted-foreground flex items-center gap-1.5">
              <Chrome className="w-3 h-3" />
              Chrome Path
            </label>
            <div className="flex items-center gap-2">
              <input
                type="text"
                value={chromePath}
                onChange={(e) => setChromePath(e.target.value)}
                placeholder="Auto-detect or enter path..."
                className="flex-1 px-2 py-1.5 text-xs bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-primary/50"
              />
              <button
                onClick={handleSaveChromePath}
                className="px-2 py-1.5 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
                title="Save Chrome path"
              >
                Save
              </button>
            </div>
            {chromeAvailability && (
              <p className="text-[10px] text-muted-foreground">
                {chromeAvailability.available
                  ? `Detected: ${chromeAvailability.path}`
                  : "Chrome not found. Please enter the path manually."}
              </p>
            )}
          </div>

          {/* CDP Port Setting */}
          <div className="flex items-center gap-3">
            <label className="text-xs text-muted-foreground">CDP Port:</label>
            <input
              type="number"
              value={cdpPort}
              onChange={(e) => setCdpPort(parseInt(e.target.value) || 9222)}
              className="w-20 px-2 py-1 text-xs bg-background border border-border rounded"
            />
            <button
              onClick={scanPorts}
              disabled={isScanning}
              className="flex items-center gap-1.5 px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
            >
              <Scan className={`w-3 h-3 ${isScanning ? "animate-spin" : ""}`} />
              Scan Ports
            </button>
          </div>
          {availablePorts.length > 0 && (
            <div className="flex items-center gap-2 flex-wrap">
              <span className="text-xs text-muted-foreground">Available:</span>
              {availablePorts.map((port) => (
                <button
                  key={port}
                  onClick={() => setCdpPort(port)}
                  className={`px-2 py-0.5 text-xs rounded transition-colors ${
                    cdpPort === port
                      ? "bg-primary text-primary-foreground"
                      : "bg-muted hover:bg-muted/80"
                  }`}
                >
                  {port}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Action Bar */}
      <div className="flex items-center gap-2 p-3 border-b border-border flex-shrink-0">
        {/* Launch Chrome Button with Info - always visible */}
        <div className="relative flex items-center gap-1">
          <button
            data-ui-id="a11y-launch-chrome-btn"
            onClick={handleLaunchChrome}
            disabled={isLaunchingChrome}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-l transition-colors ${
              launchSuccess
                ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                : "bg-muted hover:bg-muted/80"
            } disabled:opacity-50`}
            title="Launch Chrome with remote debugging enabled"
          >
            <Chrome className={`w-3.5 h-3.5 ${isLaunchingChrome ? "animate-pulse" : ""}`} />
            {launchSuccess ? "Launched!" : "Launch Chrome"}
          </button>
          <button
            onClick={() => setShowChromeInfo(!showChromeInfo)}
            className={`p-1.5 rounded-r transition-colors ${
              showChromeInfo
                ? "bg-primary/20 text-primary"
                : "bg-muted hover:bg-muted/80 text-muted-foreground hover:text-foreground"
            }`}
            title="Why is this needed?"
          >
            <HelpCircle className="w-3.5 h-3.5" />
          </button>

          {/* Info Popup */}
          {showChromeInfo && <ChromeInfoPopup onClose={() => setShowChromeInfo(false)} />}
        </div>

        <div className="w-px h-5 bg-border" />

        {connection.isConnected ? (
          <>
            <button
              data-ui-id="a11y-refresh-tree-btn"
              onClick={handleCapture}
              disabled={isLoading}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50 transition-colors"
            >
              <RefreshCw className={`w-3.5 h-3.5 ${isLoading ? "animate-spin" : ""}`} />
              Refresh Tree
            </button>
            <button
              data-ui-id="a11y-disconnect-btn"
              onClick={handleDisconnect}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
            >
              <Unplug className="w-3.5 h-3.5" />
              Disconnect
            </button>
          </>
        ) : (
          <>
            <button
              data-ui-id="a11y-auto-connect-btn"
              onClick={handleAutoConnect}
              disabled={isLoading}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50 transition-colors"
            >
              <Play className={`w-3.5 h-3.5 ${isLoading ? "animate-spin" : ""}`} />
              Auto Connect
            </button>
            <button
              data-ui-id="a11y-connect-port-btn"
              onClick={handleCapture}
              disabled={isLoading}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-muted hover:bg-muted/80 rounded disabled:opacity-50 transition-colors"
            >
              <RefreshCw className={`w-3.5 h-3.5 ${isLoading ? "animate-spin" : ""}`} />
              Connect to Port {cdpPort}
            </button>
          </>
        )}

        <div className="flex-1" />

        {/* Toggle buttons */}
        <button
          data-ui-id="a11y-refs-toggle-btn"
          onClick={() => setShowRefList(!showRefList)}
          className={`flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors ${
            showRefList ? "bg-primary/20 text-primary" : "hover:bg-muted text-muted-foreground"
          }`}
        >
          {showRefList ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
          Refs
        </button>
        <button
          data-ui-id="a11y-ai-context-toggle-btn"
          onClick={() => setShowAIContext(!showAIContext)}
          className={`flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors ${
            showAIContext ? "bg-primary/20 text-primary" : "hover:bg-muted text-muted-foreground"
          }`}
        >
          {showAIContext ? (
            <ChevronDown className="w-3 h-3" />
          ) : (
            <ChevronRight className="w-3 h-3" />
          )}
          AI Context
        </button>
      </div>

      {/* Error Display */}
      {error && (
        <div
          className={`mx-3 mt-3 p-3 rounded-lg ${getStatusColors("error").bg} flex items-start gap-2`}
        >
          <AlertCircle className={`w-4 h-4 ${getStatusColors("error").icon} shrink-0 mt-0.5`} />
          <div>
            <p className={`text-xs ${getStatusColors("error").text}`}>{error}</p>
          </div>
        </div>
      )}

      {/* Chrome Launch Error */}
      {launchError && (
        <div
          className={`mx-3 mt-3 p-3 rounded-lg ${getStatusColors("error").bg} flex items-start gap-2`}
        >
          <Chrome className={`w-4 h-4 ${getStatusColors("error").icon} shrink-0 mt-0.5`} />
          <div>
            <p className={`text-xs ${getStatusColors("error").text}`}>
              Failed to launch Chrome: {launchError}
            </p>
            <p className="text-[10px] text-muted-foreground mt-1">
              Click the settings icon to configure Chrome path manually.
            </p>
          </div>
        </div>
      )}

      {/* Content */}
      <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
        {snapshot?.root ? (
          <>
            {/* Tree Viewer */}
            <div className="flex-1 min-h-0 overflow-hidden">
              <AccessibilityTreeViewer
                root={snapshot.root}
                selectedRef={selectedNode?.ref ?? null}
                onSelectNode={setSelectedNode}
                onCopyRef={handleCopyRef}
                onClickAction={handleClickAction}
                onFillAction={handleFillAction}
                onFocusAction={handleFocusAction}
              />
            </div>

            {/* Selected Node Details */}
            {selectedNode && (
              <NodeDetailsPanel
                node={selectedNode}
                onCopyRef={handleCopyRef}
                onClickRef={clickRef}
                onFillRef={fillRef}
                onFocusRef={focusRef}
              />
            )}

            {/* Ref List */}
            {showRefList && interactiveNodes.length > 0 && (
              <RefListPanel
                nodes={interactiveNodes}
                onCopyRef={handleCopyRef}
                onCopyAll={handleCopyAllRefs}
              />
            )}

            {/* AI Context */}
            {showAIContext && aiContext && (
              <div className="border-t border-border">
                <div className="flex items-center justify-between px-3 py-2 bg-muted/30">
                  <span className="text-xs font-medium text-muted-foreground">AI Context</span>
                  <button
                    onClick={handleCopyAIContext}
                    className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted rounded transition-colors"
                  >
                    {copiedContext ? (
                      <Check className={`w-3 h-3 ${getStatusColors("success").icon}`} />
                    ) : (
                      <Copy className="w-3 h-3" />
                    )}
                    Copy
                  </button>
                </div>
                <pre className="max-h-40 overflow-auto p-3 text-[10px] font-mono text-muted-foreground bg-muted/10 whitespace-pre-wrap">
                  {aiContext}
                </pre>
              </div>
            )}
          </>
        ) : (
          /* Empty State */
          <div className="flex-1 flex flex-col items-center justify-center p-8 text-center">
            <Accessibility className="w-12 h-12 text-muted-foreground/30 mb-4" />
            <h3 className="text-lg font-medium mb-2">No Accessibility Tree</h3>
            <p className="text-sm text-muted-foreground max-w-md mb-4">
              Connect to a browser via CDP to capture and explore its accessibility tree. Start a
              browser with remote debugging enabled:
            </p>
            <code className="text-xs font-mono bg-muted px-3 py-2 rounded mb-4">
              chrome --remote-debugging-port=9222
            </code>
            <div className="flex items-start gap-2 text-xs text-muted-foreground max-w-md">
              <Info className="w-4 h-4 shrink-0 mt-0.5" />
              <p>
                The accessibility tree provides refs (@e1, @e2, etc.) that can be used to interact
                with elements programmatically. Interactive elements are highlighted in green.
              </p>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default AccessibilityExplorerPanel;
