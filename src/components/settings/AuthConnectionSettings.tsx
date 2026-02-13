/**
 * AuthConnectionSettings.tsx
 *
 * Connection settings component that uses the new authentication system.
 * WebSocket auto-connection is now handled by useWebSocketAutoConnect hook
 * at the App level to ensure it runs regardless of which tab is active.
 */

import { useState, useEffect, useRef } from "react";
import { X, Wifi, Cloud, LogOut, User, Tag, ChevronDown, Check } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { useAuth } from "../AuthProvider";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { ConnectionInfo, Project } from "../../types/auth";

const RUNNER_NAME_STORAGE_KEY = "qontinui-runner-name";

interface WebSocketState {
  connected: boolean;
  connecting: boolean;
  error: string | null;
  connectionInfo: ConnectionInfo | null;
  connect: () => Promise<void>;
  disconnect: () => Promise<void>;
}

interface AuthConnectionSettingsProps {
  onLog: (level: "success" | "error" | "info", message: string) => void;
  projects: Project[];
  selectedProjectId: string | null;
  onProjectSelect: (projectId: string | null) => void;
  onLoadProjects: () => Promise<void>;
  webSocketState: WebSocketState;
}

export function AuthConnectionSettings({
  onLog,
  projects,
  selectedProjectId,
  onProjectSelect,
  onLoadProjects,
  webSocketState,
}: AuthConnectionSettingsProps) {
  const auth = useAuth();

  // Destructure webSocket state from props
  const { connected, connecting, error, connectionInfo, connect, disconnect } = webSocketState;

  // Project dropdown state
  const [showProjectDropdown, setShowProjectDropdown] = useState(false);
  const projectDropdownRef = useRef<HTMLDivElement>(null);

  // Runner name state - persisted in localStorage
  const [runnerName, setRunnerName] = useState<string>(() => {
    return localStorage.getItem(RUNNER_NAME_STORAGE_KEY) || "";
  });

  // Persist runner name to localStorage and dispatch event for StatusIndicator
  const handleRunnerNameChange = (name: string) => {
    setRunnerName(name);
    localStorage.setItem(RUNNER_NAME_STORAGE_KEY, name);
    // Dispatch custom event so StatusIndicator can update
    window.dispatchEvent(new CustomEvent("runner-name-changed", { detail: name }));
  };

  // Load projects when authenticated
  useEffect(() => {
    if (auth.authStatus?.authenticated) {
      onLoadProjects();
    }
  }, [auth.authStatus?.authenticated, onLoadProjects]);

  // Close project dropdown when clicking outside
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        projectDropdownRef.current &&
        !projectDropdownRef.current.contains(event.target as Node)
      ) {
        setShowProjectDropdown(false);
      }
    };

    if (showProjectDropdown) {
      document.addEventListener("mousedown", handleClickOutside);
    }
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
    };
  }, [showProjectDropdown]);

  const handleLogout = async () => {
    try {
      // Disconnect first if connected
      if (connected) {
        await disconnect();
      }

      // Then logout
      await auth.logout();
      onLog("success", "Logged out successfully");
    } catch (err) {
      console.error("[AUTH_CONNECTION] Logout failed:", err);
      onLog("error", `Logout failed: ${err}`);
    }
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Connection"
        description="Connect your runner to qontinui.io for real-time monitoring and cloud storage."
        icon={<Wifi className="w-6 h-6" />}
      />

      {/* User Info */}
      {auth.authStatus?.user && (
        <div
          className="rounded-lg bg-card/50 p-4 flex items-center justify-between"
          data-ui-id="settings-authconnection-user-section"
        >
          <div className="flex items-center gap-3">
            <div className="w-8 h-8 rounded-full bg-primary/20 flex items-center justify-center">
              <User className="w-4 h-4 text-primary" />
            </div>
            <div>
              <div
                data-content-role="label"
                data-content-label="user email"
                className="text-sm font-medium"
              >
                {auth.authStatus.user.email}
              </div>
              {auth.authStatus.user.name && (
                <div
                  data-content-role="label"
                  data-content-label="user name"
                  className="text-xs text-muted-foreground"
                >
                  {auth.authStatus.user.name}
                </div>
              )}
            </div>
          </div>
          <button
            onClick={handleLogout}
            className="flex items-center gap-1.5 px-3 py-1.5 bg-destructive/10 hover:bg-destructive/20 text-destructive text-xs rounded-md transition-colors"
            data-ui-id="settings-authconnection-signout-btn"
          >
            <LogOut className="w-3.5 h-3.5" />
            Sign Out
          </button>
        </div>
      )}

      {/* Error message */}
      {error && (
        <div className={`p-3 ${getStatusColors("error").bg} rounded-lg flex items-start gap-2`}>
          <X className={`w-4 h-4 ${getStatusColors("error").icon} shrink-0 mt-0.5`} />
          <span className={`${getStatusColors("error").text} text-xs`}>{error}</span>
        </div>
      )}

      {/* Runner Name */}
      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-authconnection-runner-identity-section"
      >
        <div className="flex items-center gap-2">
          <Tag className="w-4 h-4 text-primary" />
          <h4 className="font-medium text-sm">Runner Identity</h4>
        </div>

        <div className="space-y-2">
          <label className="block">
            <div className="text-sm font-medium mb-1">Runner Name</div>
            <div className="text-xs text-muted-foreground mb-2">
              Give this runner a name to identify it on qontinui.io (e.g., "My Laptop", "Work
              Desktop")
            </div>
            <input
              type="text"
              value={runnerName}
              onChange={(e) => handleRunnerNameChange(e.target.value)}
              placeholder="My Runner"
              className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
              data-ui-id="settings-authconnection-runner-name-input"
            />
          </label>
        </div>
      </div>

      {/* Project Selection */}
      {projects.length > 0 && (
        <div
          className="space-y-4 rounded-lg bg-card/50 p-4"
          data-ui-id="settings-authconnection-project-section"
        >
          <div className="flex items-center gap-2">
            <Cloud className="w-4 h-4 text-primary" />
            <h4 className="font-medium text-sm">Select Project</h4>
          </div>

          <div className="space-y-2">
            <div className="text-sm font-medium mb-1">Project</div>
            <div className="text-xs text-muted-foreground mb-2">
              Choose which project this runner should connect to. This selection is used across all
              features that require a project.
            </div>
            <div className="relative" ref={projectDropdownRef}>
              <button
                type="button"
                onClick={() => !connected && setShowProjectDropdown(!showProjectDropdown)}
                className={`w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md flex items-center justify-between gap-2 text-left transition-colors ${
                  connected ? "opacity-60 cursor-default" : "hover:bg-muted cursor-pointer"
                }`}
                data-ui-id="settings-authconnection-project-select-btn"
              >
                <span className={selectedProjectId ? "" : "text-muted-foreground"}>
                  {selectedProjectId
                    ? projects.find((p) => p.id === selectedProjectId)?.name ||
                      "Select a project..."
                    : "Select a project..."}
                </span>
                {!connected && (
                  <ChevronDown
                    className={`w-3.5 h-3.5 flex-shrink-0 transition-transform ${showProjectDropdown ? "rotate-180" : ""}`}
                  />
                )}
              </button>

              {showProjectDropdown && !connected && (
                <div className="absolute z-20 w-full mt-1 bg-card/95 backdrop-blur rounded-lg shadow-lg max-h-60 overflow-y-auto">
                  {projects.map((project) => (
                    <button
                      key={project.id}
                      type="button"
                      onClick={() => {
                        onProjectSelect(project.id);
                        setShowProjectDropdown(false);
                      }}
                      className="w-full px-3 py-2 text-sm text-left hover:bg-muted/50 transition-colors flex items-center justify-between gap-2 first:rounded-t-lg last:rounded-b-lg"
                    >
                      <span>{project.name}</span>
                      {selectedProjectId === project.id && (
                        <Check className="w-3.5 h-3.5 text-primary" />
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>
            {connected && (
              <div className="text-xs text-muted-foreground mt-2">
                Disconnect to change projects.
              </div>
            )}
          </div>
        </div>
      )}

      {/* Connection Controls */}
      <div
        className="space-y-4 rounded-lg bg-card/50 p-4"
        data-ui-id="settings-authconnection-connection-section"
      >
        <div className="flex items-center gap-2">
          <Wifi className="w-4 h-4 text-primary" />
          <h4 className="font-medium text-sm">Connection Status</h4>
          {connected && (
            <span
              data-content-role="status"
              data-content-label="connection connected"
              className={`flex items-center gap-1.5 ${getStatusColors("success").text} text-xs`}
            >
              <span
                className={`inline-block w-1.5 h-1.5 ${getStatusColors("success").icon.replace("text-", "bg-")} rounded-full animate-pulse`}
              ></span>
              Connected
            </span>
          )}
        </div>

        {connectionInfo && (
          <div className="space-y-1.5 text-xs p-3 bg-muted/30 rounded-lg">
            <div className="flex justify-between">
              <span
                data-content-role="label"
                data-content-label="device id label"
                className="text-muted-foreground"
              >
                Device ID:
              </span>
              <span
                data-content-role="body-text"
                data-content-label="device id value"
                className="font-mono"
              >
                {connectionInfo.device_id.slice(0, 8)}...
              </span>
            </div>
            <div className="flex justify-between">
              <span
                data-content-role="label"
                data-content-label="websocket url label"
                className="text-muted-foreground"
              >
                WebSocket URL:
              </span>
              <span
                data-content-role="body-text"
                data-content-label="websocket url value"
                className="font-mono text-[10px]"
              >
                {connectionInfo.websocket_url.slice(0, 40)}...
              </span>
            </div>
            <div className="flex justify-between">
              <span
                data-content-role="label"
                data-content-label="websocket status label"
                className="text-muted-foreground"
              >
                WebSocket:
              </span>
              <span
                data-content-role="status"
                data-content-label="websocket status"
                className={
                  connected ? getStatusColors("success").text : getAccentColors("orange").text
                }
              >
                {connected ? "Connected" : "Disconnected"}
              </span>
            </div>
          </div>
        )}

        <div className="flex gap-2 pt-3">
          {!connected ? (
            <button
              onClick={connect}
              disabled={connecting || !connectionInfo || !selectedProjectId}
              className="px-4 py-1.5 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md text-sm font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
              data-ui-id="settings-authconnection-connect-btn"
            >
              {connecting ? (
                <>
                  <div className="w-3.5 h-3.5 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
                  Connecting...
                </>
              ) : (
                <>
                  <Wifi className="w-3.5 h-3.5" />
                  Connect
                </>
              )}
            </button>
          ) : (
            <button
              onClick={disconnect}
              className="px-3 py-1.5 bg-destructive text-destructive-foreground rounded-md text-sm hover:bg-destructive/90 transition-colors"
              data-ui-id="settings-authconnection-disconnect-btn"
            >
              Disconnect
            </button>
          )}
        </div>

        {!selectedProjectId && projects.length > 0 && (
          <div className={`p-2.5 ${getAccentColors("orange").bg} rounded-lg`}>
            <div className={`text-xs ${getAccentColors("orange").text}`}>
              Please select a project before connecting.
            </div>
          </div>
        )}

        {projects.length === 0 && (
          <div className={`p-2.5 ${getAccentColors("blue").bg} rounded-lg`}>
            <div className={`text-xs ${getAccentColors("blue").text}`}>
              No projects found. Create a project on qontinui.io first.
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
