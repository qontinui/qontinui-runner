/**
 * McpCallStepEditor.tsx
 *
 * Modal component for editing MCP Call step configuration.
 * Supports server selection, tool selection, JSON arguments,
 * variable extractions, and assertions.
 */

import React, { useState, useEffect, useCallback } from "react";
import {
  X,
  Plus,
  Trash2,
  ChevronDown,
  ChevronRight,
  Wifi,
  Play,
  AlertCircle,
  Check,
  Loader2,
  RefreshCw,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import type {
  McpCallStep,
  ApiVariableExtraction,
  ApiAssertion,
} from "../../types/unified-workflow";
import type { McpServerConfig, McpToolInfo, McpServerStatus } from "../../types/mcp-config";

interface McpCallStepEditorProps {
  step: McpCallStep;
  open: boolean;
  onClose: () => void;
  onSave: (updates: Partial<McpCallStep>) => void;
}

const ASSERTION_TYPES = [
  { value: "json_path", label: "JSON Path" },
  { value: "body_contains", label: "Response Contains" },
  { value: "response_time", label: "Response Time" },
];

const OPERATORS = [
  { value: "equals", label: "Equals" },
  { value: "contains", label: "Contains" },
  { value: "matches", label: "Matches (Regex)" },
  { value: "greater_than", label: "Greater Than" },
  { value: "less_than", label: "Less Than" },
];

interface McpResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

export function McpCallStepEditor({ step, open, onClose, onSave }: McpCallStepEditorProps) {
  // Local form state
  const [name, setName] = useState(step.name);
  const [serverId, setServerId] = useState(step.server_id || "");
  const [serverName, setServerName] = useState(step.server_name || "");
  const [toolName, setToolName] = useState(step.tool_name || "");
  const [argumentsText, setArgumentsText] = useState(
    step.arguments ? JSON.stringify(step.arguments, null, 2) : "{}",
  );
  const [timeoutSeconds, setTimeoutSeconds] = useState((step.timeout_seconds || 30).toString());
  const [failOnError, setFailOnError] = useState(step.fail_on_error ?? true);
  const [extractions, setExtractions] = useState<ApiVariableExtraction[]>(step.extractions || []);
  const [assertions, setAssertions] = useState<ApiAssertion[]>(step.assertions || []);
  const [runOnSubsequent, setRunOnSubsequent] = useState(
    step.run_on_subsequent_iterations ?? false,
  );

  // Data state
  const [servers, setServers] = useState<McpServerConfig[]>([]);
  const [serverStatuses, setServerStatuses] = useState<McpServerStatus[]>([]);
  const [tools, setTools] = useState<McpToolInfo[]>([]);
  const [isLoadingServers, setIsLoadingServers] = useState(false);
  const [isLoadingTools, setIsLoadingTools] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);

  // UI state
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [showExtractions, setShowExtractions] = useState(extractions.length > 0);
  const [showAssertions, setShowAssertions] = useState(assertions.length > 0);
  const [testResult, setTestResult] = useState<{
    success: boolean;
    message: string;
    data?: unknown;
  } | null>(null);
  const [isTesting, setIsTesting] = useState(false);
  const [argumentsError, setArgumentsError] = useState<string | null>(null);

  // Validate JSON arguments
  useEffect(() => {
    try {
      JSON.parse(argumentsText);
      setArgumentsError(null);
    } catch {
      setArgumentsError("Invalid JSON");
    }
  }, [argumentsText]);

  // Load servers on mount
  const loadServers = useCallback(async () => {
    setIsLoadingServers(true);
    try {
      const [serversResult, statusResult] = await Promise.all([
        invoke<McpResponse<McpServerConfig[]>>("list_mcp_servers"),
        invoke<McpResponse<McpServerStatus[]>>("get_mcp_servers_status"),
      ]);

      if (serversResult.success && serversResult.data) {
        setServers(serversResult.data.filter((s) => s.enabled));
      }
      if (statusResult.success && statusResult.data) {
        setServerStatuses(statusResult.data);
      }
    } catch (err) {
      console.error("Failed to load MCP servers:", err);
    } finally {
      setIsLoadingServers(false);
    }
  }, []);

  useEffect(() => {
    if (open) {
      loadServers();
    }
  }, [open, loadServers]);

  // Load tools when server is selected
  const loadTools = useCallback(async (sId: string) => {
    if (!sId) {
      setTools([]);
      return;
    }

    setIsLoadingTools(true);
    try {
      const result = await invoke<McpResponse<McpToolInfo[]>>("list_mcp_server_tools", {
        serverId: sId,
      });
      if (result.success && result.data) {
        setTools(result.data);
      } else {
        setTools([]);
      }
    } catch (err) {
      console.error("Failed to load MCP tools:", err);
      setTools([]);
    } finally {
      setIsLoadingTools(false);
    }
  }, []);

  // Load tools when server changes
  useEffect(() => {
    if (serverId) {
      loadTools(serverId);
    }
  }, [serverId, loadTools]);

  // Connect to server
  const handleConnect = async () => {
    if (!serverId) return;

    setIsConnecting(true);
    try {
      const result = await invoke<McpResponse<McpToolInfo[]>>("connect_mcp_server", {
        serverId,
      });

      if (result.success && result.data) {
        setTools(result.data);
        // Refresh status
        const statusResult = await invoke<McpResponse<McpServerStatus[]>>("get_mcp_servers_status");
        if (statusResult.success && statusResult.data) {
          setServerStatuses(statusResult.data);
        }
      }
    } catch (err) {
      console.error("Failed to connect to MCP server:", err);
    } finally {
      setIsConnecting(false);
    }
  };

  // Handle save
  const handleSave = () => {
    let parsedArgs: Record<string, unknown> | undefined;
    try {
      parsedArgs = JSON.parse(argumentsText);
      if (Object.keys(parsedArgs).length === 0) {
        parsedArgs = undefined;
      }
    } catch {
      parsedArgs = undefined;
    }

    const updates: Partial<McpCallStep> = {
      name,
      server_id: serverId,
      server_name: serverName,
      tool_name: toolName,
      arguments: parsedArgs,
      timeout_seconds: parseInt(timeoutSeconds) || 30,
      fail_on_error: failOnError,
      extractions: extractions.length > 0 ? extractions : undefined,
      assertions: assertions.length > 0 ? assertions : undefined,
      run_on_subsequent_iterations: runOnSubsequent,
    };

    onSave(updates);
    onClose();
  };

  // Handle server selection
  const handleServerSelect = (newServerId: string) => {
    setServerId(newServerId);
    const server = servers.find((s) => s.id === newServerId);
    setServerName(server?.name || "");
    setToolName(""); // Reset tool when server changes
    setTools([]);
  };

  // Handle tool selection
  const handleToolSelect = (newToolName: string) => {
    setToolName(newToolName);

    // Populate arguments template from tool schema
    const tool = tools.find((t) => t.name === newToolName);
    if (tool?.inputSchema?.properties) {
      const template: Record<string, unknown> = {};
      const props = tool.inputSchema.properties;
      for (const [key, schema] of Object.entries(props)) {
        if (schema.default !== undefined) {
          template[key] = schema.default;
        } else if (schema.type === "string") {
          template[key] = "";
        } else if (schema.type === "number" || schema.type === "integer") {
          template[key] = 0;
        } else if (schema.type === "boolean") {
          template[key] = false;
        } else if (schema.type === "array") {
          template[key] = [];
        } else if (schema.type === "object") {
          template[key] = {};
        }
      }
      setArgumentsText(JSON.stringify(template, null, 2));
    }
  };

  // Extraction management
  const addExtraction = () => {
    setExtractions([...extractions, { variable_name: "", json_path: "" }]);
    setShowExtractions(true);
  };

  const updateExtraction = (index: number, field: keyof ApiVariableExtraction, value: string) => {
    const updated = [...extractions];
    updated[index] = { ...updated[index], [field]: value };
    setExtractions(updated);
  };

  const removeExtraction = (index: number) => {
    setExtractions(extractions.filter((_, i) => i !== index));
  };

  // Assertion management
  const addAssertion = () => {
    setAssertions([...assertions, { type: "json_path", expected: "" }]);
    setShowAssertions(true);
  };

  const updateAssertion = (index: number, field: keyof ApiAssertion, value: string | number) => {
    const updated = [...assertions];
    updated[index] = { ...updated[index], [field]: value } as ApiAssertion;
    setAssertions(updated);
  };

  const removeAssertion = (index: number) => {
    setAssertions(assertions.filter((_, i) => i !== index));
  };

  // Test the tool call
  const handleTestCall = async () => {
    if (!serverId || !toolName) return;

    let parsedArgs: Record<string, unknown> = {};
    try {
      parsedArgs = JSON.parse(argumentsText);
    } catch {
      setTestResult({ success: false, message: "Invalid JSON arguments" });
      return;
    }

    setIsTesting(true);
    setTestResult(null);

    try {
      const result = await invoke<
        McpResponse<{
          success: boolean;
          content?: unknown;
          error?: string;
          response_type: string;
          duration_ms: number;
        }>
      >("call_mcp_tool", {
        serverId,
        toolName,
        arguments: parsedArgs,
      });

      if (result.success && result.data) {
        const data = result.data;
        if (data.success) {
          setTestResult({
            success: true,
            message: `Success (${data.duration_ms}ms)`,
            data: data.content,
          });
        } else {
          setTestResult({
            success: false,
            message: data.error || "Tool call failed",
          });
        }
      } else {
        setTestResult({
          success: false,
          message: result.error || "Request failed",
        });
      }
    } catch (err) {
      setTestResult({
        success: false,
        message: err instanceof Error ? err.message : "Request failed",
      });
    } finally {
      setIsTesting(false);
    }
  };

  // Get server status
  const getServerStatus = (sId: string): McpServerStatus | undefined => {
    return serverStatuses.find((s) => s.serverId === sId);
  };

  // Validation
  const isValid = name.trim() && serverId && toolName && !argumentsError;
  const serverStatus = getServerStatus(serverId);
  const isConnected = serverStatus?.connected ?? false;

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />

      {/* Dialog */}
      <div className="relative bg-zinc-800 border border-zinc-700 rounded-lg shadow-xl w-full max-w-2xl mx-4 max-h-[90vh] overflow-hidden flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-zinc-700">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded bg-purple-500/10">
              <Wifi className="w-5 h-5 text-purple-400" />
            </div>
            <h3 className="text-lg font-semibold text-zinc-100">Edit MCP Call</h3>
          </div>
          <button
            onClick={onClose}
            className="p-1 text-zinc-400 hover:text-zinc-200 transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {/* Step Name */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Step Name</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="MCP Call"
              className="w-full px-3 py-2 bg-zinc-900 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-purple-500/50"
            />
          </div>

          {/* Server Selection */}
          <div>
            <div className="flex items-center justify-between mb-1">
              <label className="text-sm font-medium text-zinc-400">MCP Server</label>
              <button
                onClick={loadServers}
                disabled={isLoadingServers}
                className="text-xs text-purple-400 hover:text-purple-300 flex items-center gap-1"
              >
                <RefreshCw className={`w-3 h-3 ${isLoadingServers ? "animate-spin" : ""}`} />
                Refresh
              </button>
            </div>
            <div className="flex gap-2">
              <select
                value={serverId}
                onChange={(e) => handleServerSelect(e.target.value)}
                className="flex-1 px-3 py-2 bg-zinc-900 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-purple-500/50"
              >
                <option value="">Select a server...</option>
                {servers.map((server) => {
                  const status = getServerStatus(server.id);
                  return (
                    <option key={server.id} value={server.id}>
                      {server.name} {status?.connected ? "(Connected)" : ""}
                    </option>
                  );
                })}
              </select>
              {serverId && !isConnected && (
                <button
                  onClick={handleConnect}
                  disabled={isConnecting}
                  className="px-3 py-2 bg-purple-600 hover:bg-purple-500 text-white rounded-md text-sm font-medium disabled:opacity-50 disabled:cursor-not-allowed transition-colors flex items-center gap-1"
                >
                  {isConnecting ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Wifi className="w-4 h-4" />
                  )}
                  Connect
                </button>
              )}
            </div>
            {serverId && (
              <p className="text-xs text-zinc-500 mt-1">
                {isConnected ? (
                  <span className="text-green-400">Connected - {tools.length} tools available</span>
                ) : (
                  <span className="text-yellow-500">
                    Not connected - click Connect to load tools
                  </span>
                )}
              </p>
            )}
          </div>

          {/* Tool Selection */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Tool</label>
            <select
              value={toolName}
              onChange={(e) => handleToolSelect(e.target.value)}
              disabled={!isConnected || isLoadingTools}
              className="w-full px-3 py-2 bg-zinc-900 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-purple-500/50 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <option value="">Select a tool...</option>
              {tools.map((tool) => (
                <option key={tool.name} value={tool.name}>
                  {tool.name}
                </option>
              ))}
            </select>
            {toolName && (
              <p className="text-xs text-zinc-500 mt-1">
                {tools.find((t) => t.name === toolName)?.description || "No description"}
              </p>
            )}
          </div>

          {/* Arguments */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Arguments (JSON)
              <span className="text-zinc-500 font-normal ml-2">supports {"{{variables}}"}</span>
            </label>
            <textarea
              value={argumentsText}
              onChange={(e) => setArgumentsText(e.target.value)}
              placeholder='{"key": "value"}'
              rows={6}
              className={`w-full px-3 py-2 bg-zinc-900 border rounded-md text-zinc-200 placeholder-zinc-500 font-mono text-sm resize-y focus:outline-none focus:ring-2 ${
                argumentsError
                  ? "border-red-500 focus:ring-red-500/50"
                  : "border-zinc-700 focus:ring-purple-500/50"
              }`}
            />
            {argumentsError && <p className="text-xs text-red-400 mt-1">{argumentsError}</p>}
          </div>

          {/* Quick Actions */}
          <div className="flex gap-2">
            <button
              onClick={handleTestCall}
              disabled={!isConnected || !toolName || !!argumentsError || isTesting}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md bg-purple-600 hover:bg-purple-500 text-white disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              <Play className="w-4 h-4" />
              {isTesting ? "Testing..." : "Test Call"}
            </button>
          </div>

          {/* Test Result */}
          {testResult && (
            <div
              className={`p-3 rounded-md text-sm ${
                testResult.success ? "bg-green-500/10 text-green-400" : "bg-red-500/10 text-red-400"
              }`}
            >
              <div className="flex items-start gap-2">
                {testResult.success ? (
                  <Check className="w-4 h-4 mt-0.5 flex-shrink-0" />
                ) : (
                  <AlertCircle className="w-4 h-4 mt-0.5 flex-shrink-0" />
                )}
                <div className="flex-1 min-w-0">
                  <p className="font-medium">{testResult.message}</p>
                  {testResult.data && (
                    <pre className="mt-2 text-xs bg-zinc-900/50 p-2 rounded overflow-auto max-h-40 whitespace-pre-wrap break-words">
                      {typeof testResult.data === "string"
                        ? testResult.data
                        : JSON.stringify(testResult.data, null, 2)}
                    </pre>
                  )}
                </div>
              </div>
            </div>
          )}

          {/* Variable Extractions */}
          <div className="border border-zinc-700 rounded-md overflow-hidden">
            <button
              onClick={() => setShowExtractions(!showExtractions)}
              className="w-full flex items-center justify-between p-3 bg-zinc-850 hover:bg-zinc-800 transition-colors"
            >
              <span className="text-sm font-medium text-zinc-300">
                Variable Extractions ({extractions.length})
              </span>
              {showExtractions ? (
                <ChevronDown className="w-4 h-4 text-zinc-500" />
              ) : (
                <ChevronRight className="w-4 h-4 text-zinc-500" />
              )}
            </button>
            {showExtractions && (
              <div className="p-3 border-t border-zinc-700 space-y-3">
                <p className="text-xs text-zinc-500">
                  Extract values from the response using JSON paths. Use {"{{variable_name}}"} in
                  subsequent steps.
                </p>
                {extractions.map((ext, index) => (
                  <div key={index} className="flex gap-2 items-start">
                    <div className="flex-1">
                      <input
                        type="text"
                        value={ext.variable_name}
                        onChange={(e) => updateExtraction(index, "variable_name", e.target.value)}
                        placeholder="variable_name"
                        className="w-full px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-1 focus:ring-purple-500/50"
                      />
                    </div>
                    <div className="flex-1">
                      <input
                        type="text"
                        value={ext.json_path}
                        onChange={(e) => updateExtraction(index, "json_path", e.target.value)}
                        placeholder="$.data.id"
                        className="w-full px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 font-mono focus:outline-none focus:ring-1 focus:ring-purple-500/50"
                      />
                    </div>
                    <button
                      onClick={() => removeExtraction(index)}
                      className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </div>
                ))}
                <button
                  onClick={addExtraction}
                  className="flex items-center gap-1 text-xs text-purple-400 hover:text-purple-300"
                >
                  <Plus className="w-3 h-3" />
                  Add Extraction
                </button>
              </div>
            )}
          </div>

          {/* Assertions */}
          <div className="border border-zinc-700 rounded-md overflow-hidden">
            <button
              onClick={() => setShowAssertions(!showAssertions)}
              className="w-full flex items-center justify-between p-3 bg-zinc-850 hover:bg-zinc-800 transition-colors"
            >
              <span className="text-sm font-medium text-zinc-300">
                Assertions ({assertions.length})
              </span>
              {showAssertions ? (
                <ChevronDown className="w-4 h-4 text-zinc-500" />
              ) : (
                <ChevronRight className="w-4 h-4 text-zinc-500" />
              )}
            </button>
            {showAssertions && (
              <div className="p-3 border-t border-zinc-700 space-y-3">
                <p className="text-xs text-zinc-500">
                  Assert response values. Step fails if any assertion fails.
                </p>
                {assertions.map((assertion, index) => (
                  <div key={index} className="flex gap-2 items-start flex-wrap">
                    <select
                      value={assertion.type}
                      onChange={(e) => updateAssertion(index, "type", e.target.value)}
                      className="px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 focus:outline-none"
                    >
                      {ASSERTION_TYPES.map((t) => (
                        <option key={t.value} value={t.value}>
                          {t.label}
                        </option>
                      ))}
                    </select>
                    {assertion.type === "json_path" && (
                      <input
                        type="text"
                        value={assertion.json_path || ""}
                        onChange={(e) => updateAssertion(index, "json_path", e.target.value)}
                        placeholder="$.path"
                        className="w-24 px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 font-mono focus:outline-none"
                      />
                    )}
                    <select
                      value={assertion.operator || "equals"}
                      onChange={(e) => updateAssertion(index, "operator", e.target.value)}
                      className="px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 focus:outline-none"
                    >
                      {OPERATORS.map((op) => (
                        <option key={op.value} value={op.value}>
                          {op.label}
                        </option>
                      ))}
                    </select>
                    <input
                      type="text"
                      value={String(assertion.expected)}
                      onChange={(e) => updateAssertion(index, "expected", e.target.value)}
                      placeholder="expected value"
                      className="flex-1 min-w-[100px] px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 placeholder-zinc-500 focus:outline-none"
                    />
                    <button
                      onClick={() => removeAssertion(index)}
                      className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </div>
                ))}
                <button
                  onClick={addAssertion}
                  className="flex items-center gap-1 text-xs text-purple-400 hover:text-purple-300"
                >
                  <Plus className="w-3 h-3" />
                  Add Assertion
                </button>
              </div>
            )}
          </div>

          {/* Advanced Settings */}
          <div className="border border-zinc-700 rounded-md overflow-hidden">
            <button
              onClick={() => setShowAdvanced(!showAdvanced)}
              className="w-full flex items-center justify-between p-3 bg-zinc-850 hover:bg-zinc-800 transition-colors"
            >
              <span className="text-sm font-medium text-zinc-300">Advanced Settings</span>
              {showAdvanced ? (
                <ChevronDown className="w-4 h-4 text-zinc-500" />
              ) : (
                <ChevronRight className="w-4 h-4 text-zinc-500" />
              )}
            </button>
            {showAdvanced && (
              <div className="p-3 border-t border-zinc-700 space-y-3">
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <label className="block text-xs text-zinc-500 mb-1">Timeout (seconds)</label>
                    <input
                      type="number"
                      value={timeoutSeconds}
                      onChange={(e) => setTimeoutSeconds(e.target.value)}
                      min={1}
                      max={300}
                      className="w-full px-2 py-1.5 bg-zinc-900 border border-zinc-700 rounded text-sm text-zinc-200 focus:outline-none focus:ring-1 focus:ring-purple-500/50"
                    />
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    id="failOnError"
                    checked={failOnError}
                    onChange={(e) => setFailOnError(e.target.checked)}
                    className="w-4 h-4 rounded border-zinc-600 bg-zinc-900 text-purple-500 focus:ring-purple-500/50"
                  />
                  <label htmlFor="failOnError" className="text-sm text-zinc-300">
                    Fail workflow on error
                  </label>
                </div>
                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    id="runOnSubsequent"
                    checked={runOnSubsequent}
                    onChange={(e) => setRunOnSubsequent(e.target.checked)}
                    className="w-4 h-4 rounded border-zinc-600 bg-zinc-900 text-purple-500 focus:ring-purple-500/50"
                  />
                  <label htmlFor="runOnSubsequent" className="text-sm text-zinc-300">
                    Run on subsequent iterations
                  </label>
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Footer */}
        <div className="flex gap-3 p-4 border-t border-zinc-700">
          <button
            onClick={onClose}
            className="flex-1 px-4 py-2 bg-zinc-700 text-zinc-200 rounded-md font-medium hover:bg-zinc-600 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={!isValid}
            className="flex-1 px-4 py-2 bg-purple-600 text-white rounded-md font-medium hover:bg-purple-500 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            Save Changes
          </button>
        </div>
      </div>
    </div>
  );
}
