/**
 * MCP Server Tool Picker
 *
 * Modal dialog for selecting an MCP server and tool to call as a workflow step.
 * Two-step selection: first pick a server, then pick a tool from that server.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Search,
  X,
  Check as CheckIcon,
  Server,
  Wrench,
  ArrowLeft,
  Plug,
  AlertCircle,
  Loader2,
} from "lucide-react";
import type { WorkflowPhase } from "../../types/unified-workflow";

// MCP Server type
interface McpServerConfig {
  id: string;
  name: string;
  description?: string;
  server_type: string; // "stdio" | "http" | "websocket"
  command?: string;
  url?: string;
  enabled: boolean;
  auto_start: boolean;
  tags?: string[];
}

// MCP Tool type
interface McpToolInfo {
  name: string;
  description?: string;
  input_schema?: Record<string, unknown>;
}

// Response type from Tauri
interface McpResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

interface McpServerToolPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (server: McpServerConfig, tool: McpToolInfo, phase: WorkflowPhase) => void;
  phase: WorkflowPhase;
}

type PickerMode = "servers" | "tools";

export function McpServerToolPicker({
  isOpen,
  onClose,
  onSelect,
  phase,
}: McpServerToolPickerProps) {
  const [mode, setMode] = useState<PickerMode>("servers");
  const [servers, setServers] = useState<McpServerConfig[]>([]);
  const [tools, setTools] = useState<McpToolInfo[]>([]);
  const [selectedServer, setSelectedServer] = useState<McpServerConfig | null>(null);
  const [selectedToolName, setSelectedToolName] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Fetch MCP servers
  useEffect(() => {
    if (!isOpen) return;

    const fetchServers = async () => {
      setIsLoading(true);
      setError(null);
      try {
        const response = await invoke<McpResponse<McpServerConfig[]>>("list_mcp_servers");
        if (response.success && response.data) {
          // Only show enabled servers
          setServers(response.data.filter((s) => s.enabled));
        } else {
          setServers([]);
          if (response.error) {
            setError(response.error);
          }
        }
      } catch (err) {
        console.error("Failed to fetch MCP servers:", err);
        setError("Failed to fetch MCP servers");
      } finally {
        setIsLoading(false);
      }
    };

    fetchServers();
  }, [isOpen]);

  // Reset state when modal opens. The resets are deferred via a microtask so
  // the effect body itself doesn't call setState synchronously.
  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (cancelled) return;
      setMode("servers");
      setSelectedServer(null);
      setSelectedToolName(null);
      setTools([]);
      setSearchQuery("");
      setError(null);
    });
    return () => {
      cancelled = true;
    };
  }, [isOpen]);

  // Connect to server and fetch tools
  const handleServerSelect = useCallback(async (server: McpServerConfig) => {
    setSelectedServer(server);
    setIsConnecting(true);
    setError(null);
    setTools([]);

    try {
      // First try to connect to the server
      const connectResponse = await invoke<McpResponse<McpToolInfo[]>>("connect_mcp_server", {
        serverId: server.id,
      });

      if (connectResponse.success && connectResponse.data) {
        setTools(connectResponse.data);
        setMode("tools");
        setSearchQuery("");
      } else {
        // If connection failed, try to list tools from cache
        const toolsResponse = await invoke<McpResponse<McpToolInfo[]>>("list_mcp_server_tools", {
          serverId: server.id,
        });

        if (toolsResponse.success && toolsResponse.data) {
          setTools(toolsResponse.data);
          setMode("tools");
          setSearchQuery("");
        } else {
          setError(connectResponse.error || toolsResponse.error || "Failed to get tools");
        }
      }
    } catch (err) {
      console.error("Failed to connect to MCP server:", err);
      setError(`Failed to connect to server: ${err}`);
    } finally {
      setIsConnecting(false);
    }
  }, []);

  // Go back to server selection
  const handleBack = useCallback(() => {
    setMode("servers");
    setSelectedServer(null);
    setSelectedToolName(null);
    setTools([]);
    setSearchQuery("");
    setError(null);
  }, []);

  // Handle final selection
  const handleSelect = useCallback(() => {
    if (selectedServer && selectedToolName) {
      const tool = tools.find((t) => t.name === selectedToolName);
      if (tool) {
        onSelect(selectedServer, tool, phase);
        onClose();
      }
    }
  }, [selectedServer, selectedToolName, tools, onSelect, phase, onClose]);

  // Handle double-click on tool
  const handleToolDoubleClick = useCallback(
    (tool: McpToolInfo) => {
      if (selectedServer) {
        onSelect(selectedServer, tool, phase);
        onClose();
      }
    },
    [selectedServer, onSelect, phase, onClose],
  );

  // Filter items based on search
  const filteredServers = servers.filter((server) => {
    const matchesSearch =
      !searchQuery ||
      server.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      server.description?.toLowerCase().includes(searchQuery.toLowerCase());
    return matchesSearch;
  });

  const filteredTools = tools.filter((tool) => {
    const matchesSearch =
      !searchQuery ||
      tool.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      tool.description?.toLowerCase().includes(searchQuery.toLowerCase());
    return matchesSearch;
  });

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="bg-card border border-border rounded-lg shadow-2xl w-[700px] max-w-[90vw] max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-border">
          <div className="flex items-center gap-3">
            {mode === "tools" && (
              <button
                onClick={handleBack}
                className="p-1.5 hover:bg-muted/80 rounded-md transition-colors"
              >
                <ArrowLeft className="w-4 h-4" />
              </button>
            )}
            <div>
              <h2 className="text-lg font-semibold">
                {mode === "servers"
                  ? "Select MCP Server"
                  : `Select Tool from ${selectedServer?.name}`}
              </h2>
              <p className="text-sm text-muted-foreground">
                {mode === "servers"
                  ? `Choose an MCP server to call as a ${phase} step`
                  : "Choose a tool to call from this server"}
              </p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 hover:bg-muted/80 rounded-md transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-border">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder={mode === "servers" ? "Search servers..." : "Search tools..."}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 text-sm bg-muted border border-border rounded-md
                       focus:outline-hidden focus:ring-2 focus:ring-cyan-500/50"
              autoFocus
            />
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading || isConnecting ? (
            <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
              <Loader2 className="w-8 h-8 animate-spin mb-2" />
              <span>{isConnecting ? "Connecting to server..." : "Loading..."}</span>
            </div>
          ) : error ? (
            <div className="text-center py-8">
              <AlertCircle className="w-8 h-8 text-amber-400 mx-auto mb-2" />
              <p className="text-muted-foreground">{error}</p>
              {mode === "servers" && (
                <p className="text-xs text-muted-foreground mt-2">
                  Configure MCP servers in Settings → MCP Servers.
                </p>
              )}
            </div>
          ) : mode === "servers" ? (
            // Server List
            filteredServers.length === 0 ? (
              <div className="text-center py-8 text-muted-foreground">
                {servers.length === 0
                  ? "No MCP servers configured. Add servers in Settings → MCP Servers."
                  : "No servers match your search."}
              </div>
            ) : (
              <div className="grid grid-cols-1 gap-2">
                {filteredServers.map((server) => (
                  <button
                    key={server.id}
                    onClick={() => handleServerSelect(server)}
                    className="relative flex items-start gap-3 p-3 rounded-lg text-left transition-all
                             bg-muted/50 border-2 border-transparent hover:bg-muted hover:border-border"
                  >
                    <div className="p-2 rounded-md bg-card text-indigo-400">
                      <Server className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{server.name}</span>
                        <span className="text-xs px-1.5 py-0.5 bg-muted/80 text-muted-foreground rounded">
                          {server.server_type}
                        </span>
                      </div>
                      {server.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-1">
                          {server.description}
                        </p>
                      )}
                      <div className="flex items-center gap-2 mt-1.5 text-xs text-muted-foreground">
                        {server.command && (
                          <span className="font-mono truncate max-w-[300px]">{server.command}</span>
                        )}
                        {server.url && (
                          <span className="font-mono truncate max-w-[300px]">{server.url}</span>
                        )}
                      </div>
                    </div>
                    <Plug className="w-4 h-4 text-muted-foreground" />
                  </button>
                ))}
              </div>
            )
          ) : // Tool List
          filteredTools.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              {tools.length === 0
                ? "No tools available from this server."
                : "No tools match your search."}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredTools.map((tool) => {
                const isSelected = selectedToolName === tool.name;

                return (
                  <button
                    key={tool.name}
                    onClick={() => setSelectedToolName(tool.name)}
                    onDoubleClick={() => handleToolDoubleClick(tool)}
                    className={`
                        relative flex items-start gap-3 p-3 rounded-lg text-left transition-all
                        ${
                          isSelected
                            ? "bg-cyan-500/15 border-2 border-cyan-500"
                            : "bg-muted/50 border-2 border-transparent hover:bg-muted hover:border-border"
                        }
                      `}
                  >
                    {isSelected && (
                      <div className="absolute top-2 right-2">
                        <CheckIcon className="w-4 h-4 text-cyan-400" />
                      </div>
                    )}
                    <div className="p-2 rounded-md bg-card text-amber-400">
                      <Wrench className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="font-medium font-mono">{tool.name}</div>
                      {tool.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-2">
                          {tool.description}
                        </p>
                      )}
                      {tool.input_schema && (
                        <div className="mt-1.5 text-xs text-muted-foreground">
                          <span className="text-muted-foreground">
                            {Object.keys(tool.input_schema.properties || {}).length} parameters
                          </span>
                        </div>
                      )}
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-border bg-muted/50">
          <span
            data-content-role="metric"
            data-content-label="available item count"
            className="text-sm text-muted-foreground"
          >
            {mode === "servers"
              ? `${filteredServers.length} server${filteredServers.length !== 1 ? "s" : ""} available`
              : `${filteredTools.length} tool${filteredTools.length !== 1 ? "s" : ""} available`}
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm rounded-md hover:bg-muted/80 transition-colors"
            >
              Cancel
            </button>
            {mode === "tools" && (
              <button
                onClick={handleSelect}
                disabled={!selectedToolName}
                className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md
                         transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                Add to Workflow
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
