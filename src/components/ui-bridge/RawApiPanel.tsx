/**
 * Raw API Panel
 *
 * Postman-like interface for testing UI Bridge HTTP API commands.
 * Allows sending arbitrary commands and viewing responses.
 */

import { useState, useCallback } from "react";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import {
  Play,
  Trash2,
  Copy,
  Check,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  Clock,
  Terminal,
} from "lucide-react";
import type { CommandResult } from "../../types/ui-bridge-types";

interface RawApiPanelProps {
  onSendCommand: <T = unknown>(
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<CommandResult<T>>;
  lastResult: CommandResult | null;
  commandHistory: Array<{
    id: number;
    timestamp: number;
    action: string;
    params?: Record<string, unknown>;
    result: CommandResult;
  }>;
  onClearHistory: () => void;
  disabled?: boolean;
}

// Predefined commands for quick access
const PRESET_COMMANDS = [
  { action: "getElements", params: {}, description: "Get all UI Bridge elements" },
  { action: "getSnapshot", params: {}, description: "Get full UI snapshot" },
  { action: "getComponents", params: {}, description: "Get UI components" },
  { action: "discover", params: {}, description: "Discover available elements" },
  {
    action: "executeAction",
    params: { elementId: "", action: "click" },
    description: "Execute action on element",
  },
  { action: "aiSearch", params: { query: "" }, description: "AI-powered element search" },
  { action: "aiExecute", params: { instruction: "" }, description: "AI-powered action execution" },
];

export function RawApiPanel({
  onSendCommand,
  lastResult,
  commandHistory,
  onClearHistory,
  disabled = false,
}: RawApiPanelProps) {
  const [action, setAction] = useState("getElements");
  const [paramsJson, setParamsJson] = useState("{}");
  const [isExecuting, setIsExecuting] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  const [copiedId, setCopiedId] = useState<number | null>(null);

  // Validate JSON params (derived during render — no setState needed)
  let parsedParams: Record<string, unknown> | null = null;
  let paramsError: string | null = null;
  try {
    parsedParams = JSON.parse(paramsJson) as Record<string, unknown>;
  } catch (err) {
    paramsError = err instanceof Error ? err.message : "Invalid JSON";
  }

  // Execute command
  const handleExecute = useCallback(async () => {
    if (!parsedParams || disabled || isExecuting) return;

    setIsExecuting(true);
    try {
      await onSendCommand(action, parsedParams);
    } finally {
      setIsExecuting(false);
    }
  }, [action, parsedParams, disabled, isExecuting, onSendCommand]);

  // Load preset command
  const handleLoadPreset = useCallback((preset: (typeof PRESET_COMMANDS)[number]) => {
    setAction(preset.action);
    setParamsJson(JSON.stringify(preset.params, null, 2));
  }, []);

  // Copy result to clipboard
  const handleCopyResult = useCallback(async (result: CommandResult, id?: number) => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(result, null, 2));
      setCopiedId(id ?? -1);
      setTimeout(() => setCopiedId(null), 2000);
    } catch {
      // Ignore errors
    }
  }, []);

  // Format timestamp
  const formatTime = (timestamp: number) => {
    return new Date(timestamp).toLocaleTimeString();
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-2">
          <Terminal className="w-4 h-4 text-primary" />
          <span className="font-medium">Raw API</span>
        </div>
        <div className="text-xs text-muted-foreground">SDK API</div>
      </div>

      {/* Presets */}
      <div className="mb-3">
        <div className="text-xs text-muted-foreground mb-1">Quick Commands:</div>
        <div className="flex flex-wrap gap-1">
          {PRESET_COMMANDS.slice(0, 4).map((preset) => (
            <button
              key={preset.action}
              className="px-2 py-1 text-xs bg-muted/30 hover:bg-muted/50 rounded transition-colors"
              onClick={() => handleLoadPreset(preset)}
              title={preset.description}
            >
              {preset.action}
            </button>
          ))}
          <div className="relative group">
            <button className="px-2 py-1 text-xs bg-muted/30 hover:bg-muted/50 rounded transition-colors">
              More...
            </button>
            <div className="absolute left-0 top-full mt-1 bg-card border border-border rounded-md shadow-lg z-10 hidden group-hover:block min-w-[180px]">
              {PRESET_COMMANDS.slice(4).map((preset) => (
                <button
                  key={preset.action}
                  className="w-full px-3 py-1.5 text-xs text-left hover:bg-muted/50 first:rounded-t-md last:rounded-b-md"
                  onClick={() => handleLoadPreset(preset)}
                >
                  <div className="font-mono">{preset.action}</div>
                  <div className="text-muted-foreground">{preset.description}</div>
                </button>
              ))}
            </div>
          </div>
        </div>
      </div>

      {/* Request builder */}
      <div className="space-y-2 mb-3">
        {/* Action input */}
        <div>
          <label className="text-xs text-muted-foreground mb-1 block">Action:</label>
          <input
            type="text"
            value={action}
            onChange={(e) => setAction(e.target.value)}
            placeholder="e.g., getElements"
            className="w-full px-2 py-1.5 text-sm font-mono bg-muted/30 border border-border/50 rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary"
            disabled={disabled || isExecuting}
          />
        </div>

        {/* Params input */}
        <div>
          <label className="text-xs text-muted-foreground mb-1 block">
            Params (JSON):
            {paramsError && (
              <span className="text-destructive ml-2">
                <AlertCircle className="w-3 h-3 inline mr-1" />
                {paramsError}
              </span>
            )}
          </label>
          <textarea
            value={paramsJson}
            onChange={(e) => setParamsJson(e.target.value)}
            placeholder="{}"
            rows={4}
            className={`w-full px-2 py-1.5 text-sm font-mono bg-muted/30 border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary resize-none ${
              paramsError ? "border-destructive/50" : "border-border/50"
            }`}
            disabled={disabled || isExecuting}
          />
        </div>

        {/* Execute button */}
        <Button
          onClick={handleExecute}
          disabled={disabled || isExecuting || !!paramsError}
          className="w-full"
        >
          {isExecuting ? (
            <>
              <Clock className="w-4 h-4 mr-2 animate-spin" />
              Executing...
            </>
          ) : (
            <>
              <Play className="w-4 h-4 mr-2" />
              Execute
            </>
          )}
        </Button>
      </div>

      {/* Last result */}
      {lastResult && (
        <div className="mb-3">
          <div className="flex items-center justify-between mb-1">
            <div className="flex items-center gap-2">
              <span className="text-xs text-muted-foreground">Last Result:</span>
              <Badge variant={lastResult.success ? "success" : "danger"}>
                {lastResult.success ? "Success" : "Failed"}
              </Badge>
              {lastResult.duration !== undefined && (
                <span className="text-xs text-muted-foreground">{lastResult.duration}ms</span>
              )}
            </div>
            <button
              className="text-xs text-muted-foreground hover:text-foreground flex items-center gap-1"
              onClick={() => handleCopyResult(lastResult)}
            >
              {copiedId === -1 ? (
                <>
                  <Check className="w-3 h-3" />
                  Copied
                </>
              ) : (
                <>
                  <Copy className="w-3 h-3" />
                  Copy
                </>
              )}
            </button>
          </div>
          <pre className="p-2 bg-muted/30 border border-border/50 rounded-md text-xs font-mono overflow-auto max-h-48 whitespace-pre-wrap">
            {JSON.stringify(lastResult, null, 2)}
          </pre>
        </div>
      )}

      {/* Command history */}
      <div className="flex-1 overflow-hidden flex flex-col">
        <button
          className="flex items-center justify-between w-full py-1 text-xs text-muted-foreground hover:text-foreground"
          onClick={() => setShowHistory(!showHistory)}
        >
          <span className="flex items-center gap-1">
            {showHistory ? (
              <ChevronDown className="w-3.5 h-3.5" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5" />
            )}
            History ({commandHistory.length})
          </span>
          {commandHistory.length > 0 && (
            <button
              className="text-muted-foreground hover:text-destructive p-1"
              onClick={(e) => {
                e.stopPropagation();
                onClearHistory();
              }}
              title="Clear history"
            >
              <Trash2 className="w-3 h-3" />
            </button>
          )}
        </button>

        {showHistory && (
          <div className="flex-1 overflow-auto space-y-1 mt-1">
            {commandHistory.length === 0 ? (
              <div className="text-xs text-muted-foreground text-center py-4">
                No commands executed yet
              </div>
            ) : (
              commandHistory.map((entry) => (
                <div key={entry.id} className="p-2 bg-muted/20 rounded-md text-xs group">
                  <div className="flex items-center justify-between mb-1">
                    <div className="flex items-center gap-2">
                      <Badge
                        variant={entry.result.success ? "success" : "danger"}
                        className="text-[10px] px-1 py-0"
                      >
                        {entry.result.success ? "OK" : "ERR"}
                      </Badge>
                      <span className="font-mono font-medium">{entry.action}</span>
                    </div>
                    <div className="flex items-center gap-2">
                      <span className="text-muted-foreground">{formatTime(entry.timestamp)}</span>
                      {entry.result.duration !== undefined && (
                        <span className="text-muted-foreground">{entry.result.duration}ms</span>
                      )}
                      <button
                        className="opacity-0 group-hover:opacity-100 text-muted-foreground hover:text-foreground transition-opacity"
                        onClick={() => handleCopyResult(entry.result, entry.id)}
                      >
                        {copiedId === entry.id ? (
                          <Check className="w-3 h-3" />
                        ) : (
                          <Copy className="w-3 h-3" />
                        )}
                      </button>
                    </div>
                  </div>
                  {entry.params && Object.keys(entry.params).length > 0 && (
                    <div className="text-muted-foreground font-mono truncate">
                      params: {JSON.stringify(entry.params)}
                    </div>
                  )}
                </div>
              ))
            )}
          </div>
        )}
      </div>
    </div>
  );
}

export default RawApiPanel;
