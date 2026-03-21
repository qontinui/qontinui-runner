/**
 * McpSettings.tsx
 *
 * Settings for MCP (Model Context Protocol) server configuration.
 * Allows users to configure external MCP servers that the runner can call.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Server,
  Plus,
  Trash2,
  Edit2,
  Power,
  PowerOff,
  RefreshCw,
  ChevronDown,
  ChevronRight,
  Terminal,
  Globe,
  Wrench,
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";
import type {
  McpServerConfig,
  McpServerStatus,
  McpToolInfo,
  McpTransportType,
} from "../../types/mcp-config";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  error?: string;
}

interface McpSettingsProps {
  onLog: LogFunction;
}

interface ServerFormData {
  name: string;
  description: string;
  transport: McpTransportType;
  // Stdio
  command: string;
  args: string;
  cwd: string;
  // HTTP
  url: string;
  headers: string;
  // Common
  enabled: boolean;
  autoStart: boolean;
  timeout_seconds: number;
}

const defaultFormData: ServerFormData = {
  name: "",
  description: "",
  transport: "stdio",
  command: "",
  args: "",
  cwd: "",
  url: "",
  headers: "",
  enabled: true,
  autoStart: false,
  timeout_seconds: 30,
};

export function McpSettings({ onLog }: McpSettingsProps) {
  const [servers, setServers] = useState<McpServerConfig[]>([]);
  const [serverStatus, setServerStatus] = useState<Map<string, McpServerStatus>>(new Map());
  const [loading, setLoading] = useState(true);
  const [editingServer, setEditingServer] = useState<McpServerConfig | null>(null);
  const [showAddForm, setShowAddForm] = useState(false);
  const [formData, setFormData] = useState<ServerFormData>(defaultFormData);
  const [saving, setSaving] = useState(false);
  const [expandedServer, setExpandedServer] = useState<string | null>(null);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [deleteConfirm, setDeleteConfirm] = useState<McpServerConfig | null>(null);

  useEffect(() => {
    loadServers();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadServers = async () => {
    setLoading(true);
    try {
      const result = await invoke<TauriResult<McpServerConfig[]>>("list_mcp_servers");
      if (result?.success && result.data) {
        setServers(result.data);
        // Load status for all servers
        await loadAllStatus();
      }
    } catch (err) {
      console.error("Failed to load MCP servers:", err);
      onLog("error", `Failed to load MCP servers: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const loadAllStatus = async () => {
    try {
      const result = await invoke<TauriResult<McpServerStatus[]>>("get_mcp_servers_status");
      if (result?.success && result.data) {
        const statusMap = new Map<string, McpServerStatus>();
        for (const status of result.data) {
          statusMap.set(status.serverId, status);
        }
        setServerStatus(statusMap);
      }
    } catch (err) {
      console.error("Failed to load server status:", err);
    }
  };

  const handleConnect = async (serverId: string) => {
    setConnecting(serverId);
    try {
      const result = await invoke<TauriResult<McpToolInfo[]>>("connect_mcp_server", {
        serverId,
      });
      if (result?.success) {
        onLog("success", `Connected to MCP server with ${result.data?.length || 0} tools`);
        await loadAllStatus();
      } else {
        onLog("error", result?.error || "Failed to connect");
      }
    } catch (err) {
      onLog("error", `Connection failed: ${err}`);
    } finally {
      setConnecting(null);
    }
  };

  const handleDisconnect = async (serverId: string) => {
    setConnecting(serverId);
    try {
      const result = await invoke<TauriResult<void>>("disconnect_mcp_server", {
        serverId,
      });
      if (result?.success) {
        onLog("info", "Disconnected from MCP server");
        await loadAllStatus();
      } else {
        onLog("error", result?.error || "Failed to disconnect");
      }
    } catch (err) {
      onLog("error", `Disconnect failed: ${err}`);
    } finally {
      setConnecting(null);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      // Parse args and headers
      const args = formData.args ? formData.args.split("\n").filter((a) => a.trim()) : [];
      let headers: Record<string, string> = {};
      if (formData.headers) {
        try {
          headers = JSON.parse(formData.headers);
        } catch {
          // Try parsing as key=value lines
          const lines = formData.headers.split("\n").filter((l) => l.trim());
          for (const line of lines) {
            const [key, ...valueParts] = line.split(":");
            if (key && valueParts.length > 0) {
              headers[key.trim()] = valueParts.join(":").trim();
            }
          }
        }
      }

      const input = {
        name: formData.name,
        description: formData.description || null,
        transport: formData.transport,
        command: formData.transport === "stdio" ? formData.command || null : null,
        args: formData.transport === "stdio" && args.length > 0 ? args : null,
        cwd: formData.transport === "stdio" && formData.cwd ? formData.cwd : null,
        url: formData.transport === "http" ? formData.url || null : null,
        headers: formData.transport === "http" && Object.keys(headers).length > 0 ? headers : null,
        enabled: formData.enabled,
        autoStart: formData.autoStart,
        timeoutSeconds: formData.timeout_seconds,
      };

      let result;
      if (editingServer) {
        result = await invoke<TauriResult<McpServerConfig>>("update_mcp_server", {
          serverId: editingServer.id,
          input,
        });
      } else {
        result = await invoke<TauriResult<McpServerConfig>>("create_mcp_server", {
          input,
        });
      }

      if (result?.success) {
        onLog("success", editingServer ? "Server updated" : "Server created");
        setFormData(defaultFormData);
        setEditingServer(null);
        setShowAddForm(false);
        await loadServers();
      } else {
        onLog("error", result?.error || "Failed to save server");
      }
    } catch (err) {
      onLog("error", `Failed to save: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (server: McpServerConfig) => {
    try {
      const result = await invoke<TauriResult<void>>("delete_mcp_server", {
        serverId: server.id,
      });
      if (result?.success) {
        onLog("success", `Deleted server: ${server.name}`);
        setDeleteConfirm(null);
        await loadServers();
      } else {
        onLog("error", result?.error || "Failed to delete server");
      }
    } catch (err) {
      onLog("error", `Failed to delete: ${err}`);
    }
  };

  const openEditForm = (server: McpServerConfig) => {
    setEditingServer(server);
    setFormData({
      name: server.name,
      description: server.description || "",
      transport: server.transport,
      command: server.command || "",
      args: server.args?.join("\n") || "",
      cwd: server.cwd || "",
      url: server.url || "",
      headers: server.headers ? JSON.stringify(server.headers, null, 2) : "",
      enabled: server.enabled,
      autoStart: server.autoStart,
      timeout_seconds: server.timeout_seconds,
    });
    setShowAddForm(true);
  };

  const cancelForm = () => {
    setFormData(defaultFormData);
    setEditingServer(null);
    setShowAddForm(false);
  };

  const renderServerCard = (server: McpServerConfig) => {
    const status = serverStatus.get(server.id);
    const isExpanded = expandedServer === server.id;
    const isConnecting = connecting === server.id;

    return (
      <div key={server.id} className="rounded-lg border border-border bg-card/50 overflow-hidden">
        {/* Server Header */}
        <div className="p-3 flex items-center gap-3">
          {/* Expand button */}
          <button
            onClick={() => setExpandedServer(isExpanded ? null : server.id)}
            className="p-1 hover:bg-muted/50 rounded"
          >
            {isExpanded ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )}
          </button>

          {/* Transport icon */}
          <div className="p-1.5 bg-muted/50 rounded">
            {server.transport === "stdio" ? (
              <Terminal className="w-4 h-4 text-primary" />
            ) : (
              <Globe className="w-4 h-4 text-primary" />
            )}
          </div>

          {/* Server info */}
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <span
                data-content-role="label"
                data-content-label="server name"
                className="font-medium text-sm truncate"
              >
                {server.name}
              </span>
              {!server.enabled && (
                <span
                  data-content-role="badge"
                  data-content-label="disabled badge"
                  className="text-[10px] px-1.5 py-0.5 bg-muted rounded text-muted-foreground"
                >
                  Disabled
                </span>
              )}
              {server.autoStart && (
                <span
                  data-content-role="badge"
                  data-content-label="auto-start badge"
                  className="text-[10px] px-1.5 py-0.5 bg-primary/10 rounded text-primary"
                >
                  Auto-start
                </span>
              )}
            </div>
            <div
              data-content-role="body-text"
              data-content-label="server endpoint"
              className="text-xs text-muted-foreground truncate"
            >
              {server.transport === "stdio" ? server.command : server.url || "No URL"}
            </div>
          </div>

          {/* Connection status */}
          <div className="flex items-center gap-1.5">
            {status?.connected ? (
              <span
                data-content-role="status"
                data-content-label="server connected"
                className="flex items-center gap-1 text-xs text-green-500"
              >
                <span className="w-2 h-2 rounded-full bg-green-500 animate-pulse" />
                Connected
              </span>
            ) : status?.error ? (
              <span
                data-content-role="status"
                data-content-label="server error"
                className="flex items-center gap-1 text-xs text-red-500"
                title={status.error}
              >
                <span className="w-2 h-2 rounded-full bg-red-500" />
                Error
              </span>
            ) : (
              <span
                data-content-role="status"
                data-content-label="server disconnected"
                className="flex items-center gap-1 text-xs text-muted-foreground"
              >
                <span className="w-2 h-2 rounded-full bg-muted" />
                Disconnected
              </span>
            )}
          </div>

          {/* Actions */}
          <div className="flex items-center gap-1">
            {status?.connected ? (
              <button
                onClick={() => handleDisconnect(server.id)}
                disabled={isConnecting}
                className="p-1.5 hover:bg-muted/50 rounded text-muted-foreground hover:text-foreground"
                title="Disconnect"
              >
                {isConnecting ? (
                  <RefreshCw className="w-4 h-4 animate-spin" />
                ) : (
                  <PowerOff className="w-4 h-4" />
                )}
              </button>
            ) : (
              <button
                onClick={() => handleConnect(server.id)}
                disabled={isConnecting || !server.enabled}
                className="p-1.5 hover:bg-muted/50 rounded text-muted-foreground hover:text-green-500 disabled:opacity-50"
                title="Connect"
              >
                {isConnecting ? (
                  <RefreshCw className="w-4 h-4 animate-spin" />
                ) : (
                  <Power className="w-4 h-4" />
                )}
              </button>
            )}
            <button
              onClick={() => openEditForm(server)}
              className="p-1.5 hover:bg-muted/50 rounded text-muted-foreground hover:text-foreground"
              title="Edit"
            >
              <Edit2 className="w-4 h-4" />
            </button>
            <button
              onClick={() => setDeleteConfirm(server)}
              className="p-1.5 hover:bg-muted/50 rounded text-muted-foreground hover:text-red-500"
              title="Delete"
            >
              <Trash2 className="w-4 h-4" />
            </button>
          </div>
        </div>

        {/* Expanded content - Tools */}
        {isExpanded && (
          <div className="border-t border-border p-3 bg-muted/20">
            {status?.connected && status.tools && status.tools.length > 0 ? (
              <div className="space-y-2">
                <div
                  data-content-role="label"
                  data-content-label="available tools count"
                  className="text-xs font-medium text-muted-foreground"
                >
                  Available Tools ({status.tools.length})
                </div>
                <div className="grid gap-2">
                  {status.tools.map((tool) => (
                    <div
                      key={tool.name}
                      className="p-2 bg-background/50 rounded border border-border/50"
                    >
                      <div className="flex items-center gap-2">
                        <Wrench className="w-3.5 h-3.5 text-primary" />
                        <span
                          data-content-role="label"
                          data-content-label="tool name"
                          className="font-mono text-xs"
                        >
                          {tool.name}
                        </span>
                      </div>
                      {tool.description && (
                        <p className="text-[10px] text-muted-foreground mt-1 ml-5">
                          {tool.description}
                        </p>
                      )}
                    </div>
                  ))}
                </div>
              </div>
            ) : status?.connected ? (
              <div className="text-xs text-muted-foreground">No tools available</div>
            ) : status?.error ? (
              <div className="text-xs text-red-500">{status.error}</div>
            ) : (
              <div className="text-xs text-muted-foreground">Connect to see available tools</div>
            )}

            {server.description && (
              <div className="mt-3 pt-3 border-t border-border/50">
                <div className="text-xs text-muted-foreground">{server.description}</div>
              </div>
            )}
          </div>
        )}
      </div>
    );
  };

  const renderForm = () => (
    <div className="space-y-4 rounded-lg bg-card/50 p-4">
      <h4 className="font-medium text-sm">{editingServer ? "Edit Server" : "Add New Server"}</h4>

      {/* Name */}
      <div className="space-y-1.5">
        <label className="text-xs font-medium">Name</label>
        <input
          type="text"
          value={formData.name}
          onChange={(e) => setFormData((f) => ({ ...f, name: e.target.value }))}
          placeholder="My MCP Server"
          className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
        />
      </div>

      {/* Description */}
      <div className="space-y-1.5">
        <label className="text-xs font-medium">Description (optional)</label>
        <input
          type="text"
          value={formData.description}
          onChange={(e) => setFormData((f) => ({ ...f, description: e.target.value }))}
          placeholder="What this server provides"
          className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
        />
      </div>

      {/* Transport */}
      <div className="space-y-1.5">
        <label className="text-xs font-medium">Transport</label>
        <div className="flex gap-2">
          <label
            className={`flex-1 flex items-center gap-2 p-3 rounded-lg cursor-pointer transition-colors ${
              formData.transport === "stdio"
                ? "bg-primary/10 border border-primary/50"
                : "bg-muted/30 hover:bg-muted/50 border border-transparent"
            }`}
          >
            <input
              type="radio"
              name="transport"
              value="stdio"
              checked={formData.transport === "stdio"}
              onChange={() => setFormData((f) => ({ ...f, transport: "stdio" }))}
              className="accent-primary"
            />
            <Terminal className="w-4 h-4" />
            <div>
              <div className="text-sm font-medium">Stdio</div>
              <div className="text-[10px] text-muted-foreground">Run as subprocess</div>
            </div>
          </label>
          <label
            className={`flex-1 flex items-center gap-2 p-3 rounded-lg cursor-pointer transition-colors ${
              formData.transport === "http"
                ? "bg-primary/10 border border-primary/50"
                : "bg-muted/30 hover:bg-muted/50 border border-transparent"
            }`}
          >
            <input
              type="radio"
              name="transport"
              value="http"
              checked={formData.transport === "http"}
              onChange={() => setFormData((f) => ({ ...f, transport: "http" }))}
              className="accent-primary"
            />
            <Globe className="w-4 h-4" />
            <div>
              <div className="text-sm font-medium">HTTP</div>
              <div className="text-[10px] text-muted-foreground">Connect to server</div>
            </div>
          </label>
        </div>
      </div>

      {/* Stdio fields */}
      {formData.transport === "stdio" && (
        <div className="space-y-3 p-3 bg-muted/20 rounded-lg">
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Command</label>
            <input
              type="text"
              value={formData.command}
              onChange={(e) => setFormData((f) => ({ ...f, command: e.target.value }))}
              placeholder="npx @modelcontextprotocol/server-filesystem"
              className="w-full px-2.5 py-1.5 text-sm bg-background/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50 font-mono"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Arguments (one per line)</label>
            <textarea
              value={formData.args}
              onChange={(e) => setFormData((f) => ({ ...f, args: e.target.value }))}
              placeholder="--path&#10;/home/user"
              rows={3}
              className="w-full px-2.5 py-1.5 text-sm bg-background/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50 font-mono resize-none"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Working Directory (optional)</label>
            <input
              type="text"
              value={formData.cwd}
              onChange={(e) => setFormData((f) => ({ ...f, cwd: e.target.value }))}
              placeholder="/home/user/project"
              className="w-full px-2.5 py-1.5 text-sm bg-background/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50 font-mono"
            />
          </div>
          <div className="p-2 bg-amber-500/10 rounded text-xs text-amber-600 dark:text-amber-400">
            Note: Stdio transport is not yet fully implemented. HTTP is recommended.
          </div>
        </div>
      )}

      {/* HTTP fields */}
      {formData.transport === "http" && (
        <div className="space-y-3 p-3 bg-muted/20 rounded-lg">
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Server URL</label>
            <input
              type="text"
              value={formData.url}
              onChange={(e) => setFormData((f) => ({ ...f, url: e.target.value }))}
              placeholder="http://localhost:3000"
              className="w-full px-2.5 py-1.5 text-sm bg-background/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50 font-mono"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-medium">Headers (JSON or key: value per line)</label>
            <textarea
              value={formData.headers}
              onChange={(e) => setFormData((f) => ({ ...f, headers: e.target.value }))}
              placeholder='Authorization: Bearer token&#10;Or: {"Authorization": "Bearer token"}'
              rows={3}
              className="w-full px-2.5 py-1.5 text-sm bg-background/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50 font-mono resize-none"
            />
          </div>
        </div>
      )}

      {/* Common settings */}
      <div className="space-y-3">
        <div className="space-y-1.5">
          <label className="text-xs font-medium">Timeout (seconds)</label>
          <input
            type="number"
            value={formData.timeout_seconds}
            onChange={(e) =>
              setFormData((f) => ({ ...f, timeout_seconds: parseInt(e.target.value) || 30 }))
            }
            min={1}
            max={300}
            className="w-24 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
          />
        </div>

        <label className="flex items-center gap-3 cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
          <input
            type="checkbox"
            checked={formData.enabled}
            onChange={(e) => setFormData((f) => ({ ...f, enabled: e.target.checked }))}
            className="accent-primary"
          />
          <div>
            <div className="text-sm font-medium">Enabled</div>
            <div className="text-[10px] text-muted-foreground">Server can be connected</div>
          </div>
        </label>

        <label className="flex items-center gap-3 cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
          <input
            type="checkbox"
            checked={formData.autoStart}
            onChange={(e) => setFormData((f) => ({ ...f, autoStart: e.target.checked }))}
            className="accent-primary"
          />
          <div>
            <div className="text-sm font-medium">Auto-start</div>
            <div className="text-[10px] text-muted-foreground">
              Connect automatically when runner launches
            </div>
          </div>
        </label>
      </div>

      {/* Form actions */}
      <div className="flex justify-end gap-2 pt-2">
        <button
          onClick={cancelForm}
          className="px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground"
        >
          Cancel
        </button>
        <button
          onClick={handleSave}
          disabled={saving || !formData.name}
          className={`px-4 py-1.5 rounded-md text-sm font-medium transition-colors ${
            !saving && formData.name
              ? "bg-primary text-primary-foreground hover:bg-primary/90"
              : "bg-muted text-muted-foreground cursor-not-allowed"
          }`}
        >
          {saving ? "Saving..." : editingServer ? "Update" : "Create"}
        </button>
      </div>
    </div>
  );

  const renderDeleteConfirm = () => {
    if (!deleteConfirm) return null;
    return (
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
        <div className="bg-card border border-border rounded-lg shadow-xl p-6 max-w-sm mx-4">
          <h3 className="text-lg font-semibold mb-2">Delete Server</h3>
          <p className="text-sm text-muted-foreground mb-4">
            Are you sure you want to delete "{deleteConfirm.name}"? This cannot be undone.
          </p>
          <div className="flex justify-end gap-2">
            <button
              onClick={() => setDeleteConfirm(null)}
              className="px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground"
            >
              Cancel
            </button>
            <button
              onClick={() => handleDelete(deleteConfirm)}
              className="px-4 py-1.5 rounded-md text-sm font-medium bg-red-500 text-white hover:bg-red-600"
            >
              Delete
            </button>
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="MCP Servers"
        description="Configure external MCP (Model Context Protocol) servers that the runner can call to extend its capabilities."
        icon={<Server className="w-6 h-6" />}
      />

      {/* Server list */}
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <h4 className="font-medium text-sm">Configured Servers</h4>
          <button
            onClick={() => {
              setFormData(defaultFormData);
              setEditingServer(null);
              setShowAddForm(true);
            }}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90"
          >
            <Plus className="w-4 h-4" />
            Add Server
          </button>
        </div>

        {loading ? (
          <div className="flex items-center justify-center py-8">
            <RefreshCw className="w-5 h-5 animate-spin text-muted-foreground" />
          </div>
        ) : servers.length === 0 && !showAddForm ? (
          <div className="text-center py-8 text-muted-foreground">
            <Server className="w-12 h-12 mx-auto mb-3 opacity-30" />
            <p className="text-sm">No MCP servers configured</p>
            <p className="text-xs mt-1">Add a server to extend runner capabilities</p>
          </div>
        ) : (
          <div className="space-y-2">{servers.map(renderServerCard)}</div>
        )}
      </div>

      {/* Add/Edit form */}
      {showAddForm && renderForm()}

      {/* Info box */}
      <div className="p-3 bg-primary/5 rounded-lg">
        <div className="text-xs text-muted-foreground">
          <strong className="text-foreground">What is MCP?</strong> The Model Context Protocol (MCP)
          allows the runner to call external tools and services. You can add MCP servers for
          databases, file systems, APIs, or custom tools. When connected, their tools become
          available as workflow steps.
        </div>
      </div>

      {renderDeleteConfirm()}
    </div>
  );
}
