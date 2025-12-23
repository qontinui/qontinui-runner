/**
 * AuthConnectionSettings.tsx
 *
 * Connection settings component that uses the new authentication system.
 * Automatically connects after login using device auth tokens.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { X, Wifi, Cloud, LogOut, User, Tag, ChevronDown, Check } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { useAuth } from "../AuthProvider";
import type { ConnectionInfo, Project } from "../../types/auth";

const RUNNER_NAME_STORAGE_KEY = "qontinui-runner-name";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface WebSocketConfig {
  enabled: boolean;
  url: string;
  token: string;
  project_id: string | null;
  runner_name: string | null;
}

interface AuthConnectionSettingsProps {
  onLog: (level: "success" | "error" | "info", message: string) => void;
  projects: Project[];
  selectedProjectId: string | null;
  onProjectSelect: (projectId: string | null) => void;
  onLoadProjects: () => Promise<void>;
}

export function AuthConnectionSettings({
  onLog,
  projects,
  selectedProjectId,
  onProjectSelect,
  onLoadProjects,
}: AuthConnectionSettingsProps) {
  const auth = useAuth();

  // Connection state
  const [connectionInfo, setConnectionInfo] = useState<ConnectionInfo | null>(null);
  const [connected, setConnected] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  // Load connection info and projects when authenticated
  useEffect(() => {
    if (auth.authStatus?.authenticated) {
      loadConnectionInfo();
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

  // Send heartbeat periodically when connected
  useEffect(() => {
    if (!connected || !selectedProjectId) {
      return;
    }

    // Send initial heartbeat immediately
    const sendHeartbeat = async () => {
      try {
        await invoke("send_device_heartbeat", { projectId: selectedProjectId });
        console.log("[AUTH_CONNECTION] Heartbeat sent");
      } catch (err) {
        console.error("[AUTH_CONNECTION] Heartbeat failed:", err);
        // Don't show error to user - heartbeat failures are non-critical
      }
    };

    sendHeartbeat();

    // Set up periodic heartbeat (every 30 seconds)
    const intervalId = setInterval(sendHeartbeat, 30000);

    return () => {
      clearInterval(intervalId);
    };
  }, [connected, selectedProjectId]);

  const loadConnectionInfo = async () => {
    try {
      const info = await invoke<ConnectionInfo>("get_connection_info");
      setConnectionInfo(info);
      console.log("[AUTH_CONNECTION] Connection info loaded:", info);
    } catch (err) {
      console.error("[AUTH_CONNECTION] Failed to load connection info:", err);
      setError(err as string);
    }
  };

  const handleConnect = useCallback(async () => {
    if (!connectionInfo) {
      setError("Connection information not available");
      return;
    }

    // Prevent multiple simultaneous connection attempts
    if (connecting) {
      console.log("[AUTH_CONNECTION] Connection already in progress, skipping");
      return;
    }

    // Reset the user disconnect flag when user manually connects
    userDisconnectedRef.current = false;

    setConnecting(true);
    setError(null);

    try {
      console.log("[AUTH_CONNECTION] Getting access token from AuthManager...");

      // Get access token from Rust AuthManager (stored in OS keychain)
      // We need to create a Rust command that returns the token
      // For now, we'll use a placeholder - the Rust backend will need to provide this
      const authManager = await import("@tauri-apps/api/core");
      let accessToken = "";

      try {
        // This command needs to be added to Rust backend
        accessToken = await authManager.invoke<string>("get_access_token_for_websocket");
      } catch (tokenErr) {
        console.error("[AUTH_CONNECTION] Failed to get access token:", tokenErr);
        throw new Error("Failed to retrieve authentication token. Please try logging in again.");
      }

      console.log("[AUTH_CONNECTION] Configuring WebSocket with auth token...");

      // Configure WebSocket with auth token
      const config: WebSocketConfig = {
        enabled: true,
        url: connectionInfo.websocket_url,
        token: accessToken,
        project_id: selectedProjectId,
        runner_name: runnerName || null,
      };
      const configResult = await invoke<TauriResult<null>>("configure_websocket", {
        config,
      });

      if (!configResult.success) {
        throw new Error(configResult.message || "Failed to configure WebSocket");
      }

      console.log("[AUTH_CONNECTION] Connecting...");

      // Connect WebSocket
      const connectResult = await invoke<TauriResult<null>>("connect_websocket");

      if (!connectResult.success) {
        throw new Error(connectResult.message || "Failed to connect WebSocket");
      }

      setConnected(true);
      onLog("success", "Connected to qontinui.io successfully");
      console.log("[AUTH_CONNECTION] Connected successfully");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      setError(errorMessage);
      onLog("error", `Connection failed: ${errorMessage}`);
      console.error("[AUTH_CONNECTION] Connection failed:", err);
    } finally {
      setConnecting(false);
    }
  }, [connectionInfo, selectedProjectId, onLog, connecting, runnerName]);

  // Auto-connect WebSocket when project is selected and not already connected
  // Use a ref to track if we've attempted auto-connect for this project
  const autoConnectAttemptedRef = useRef<string | null>(null);
  // Track if user manually disconnected to prevent auto-reconnect
  const userDisconnectedRef = useRef<boolean>(false);

  useEffect(() => {
    // Only auto-connect if:
    // 1. We have a selected project
    // 2. We have connection info
    // 3. Not already connected
    // 4. Not currently connecting
    // 5. Haven't already attempted for this project
    // 6. User didn't manually disconnect
    if (
      selectedProjectId &&
      connectionInfo &&
      !connected &&
      !connecting &&
      autoConnectAttemptedRef.current !== selectedProjectId &&
      !userDisconnectedRef.current
    ) {
      console.log("[AUTH_CONNECTION] Auto-connecting WebSocket for project:", selectedProjectId);
      autoConnectAttemptedRef.current = selectedProjectId;
      handleConnect();
    }
  }, [selectedProjectId, connectionInfo, connected, connecting, handleConnect]);

  // Reset auto-connect ref when project changes (but NOT on disconnect)
  useEffect(() => {
    // Only reset if we're connected - this prevents reset after disconnect
    // The ref will be reset when a new project is selected
    if (selectedProjectId && autoConnectAttemptedRef.current !== selectedProjectId) {
      // Project changed, reset the user disconnect flag
      userDisconnectedRef.current = false;
    }
  }, [selectedProjectId]);

  const handleDisconnect = async () => {
    try {
      // Mark that user manually disconnected to prevent auto-reconnect
      userDisconnectedRef.current = true;
      console.log("[AUTH_CONNECTION] User initiated disconnect - auto-reconnect disabled");

      const result = await invoke<TauriResult<null>>("disconnect_websocket");
      if (result && result.success) {
        setConnected(false);
        onLog("success", "Disconnected from qontinui.io");
      } else {
        // Reset the flag if disconnect failed
        userDisconnectedRef.current = false;
        onLog("error", `Failed to disconnect: ${result?.message || "Unknown error"}`);
      }
    } catch (err) {
      // Reset the flag if disconnect failed
      userDisconnectedRef.current = false;
      console.error("[AUTH_CONNECTION] Disconnect failed:", err);
      onLog("error", `Failed to disconnect: ${err}`);
    }
  };

  const handleLogout = async () => {
    try {
      // Disconnect first if connected
      if (connected) {
        await handleDisconnect();
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
        <div className="bg-card rounded-lg border border-border/50 p-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-full bg-primary/20 flex items-center justify-center">
              <User className="w-5 h-5 text-primary" />
            </div>
            <div>
              <div className="font-medium">{auth.authStatus.user.email}</div>
              {auth.authStatus.user.name && (
                <div className="text-sm text-muted-foreground">{auth.authStatus.user.name}</div>
              )}
            </div>
          </div>
          <button
            onClick={handleLogout}
            className="flex items-center gap-2 px-4 py-2 bg-destructive/10 hover:bg-destructive/20 text-destructive rounded-md transition-colors"
          >
            <LogOut className="w-4 h-4" />
            Sign Out
          </button>
        </div>
      )}

      {/* Error message */}
      {error && (
        <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
          <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
          <span className="text-red-400 text-sm">{error}</span>
        </div>
      )}

      {/* Runner Name */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Tag className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Runner Identity</h4>
        </div>

        <div className="space-y-2">
          <label className="block">
            <div className="font-medium mb-1">Runner Name</div>
            <div className="text-sm text-muted-foreground mb-3">
              Give this runner a name to identify it on qontinui.io (e.g., "My Laptop", "Work
              Desktop")
            </div>
            <input
              type="text"
              value={runnerName}
              onChange={(e) => handleRunnerNameChange(e.target.value)}
              placeholder="My Runner"
              className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
            />
          </label>
        </div>
      </div>

      {/* Project Selection */}
      {projects.length > 0 && (
        <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
          <div className="flex items-center gap-3">
            <Cloud className="w-5 h-5 text-primary" />
            <h4 className="font-semibold text-lg">Select Project</h4>
          </div>

          <div className="space-y-2">
            <div className="font-medium mb-1">Project</div>
            <div className="text-sm text-muted-foreground mb-3">
              Choose which project this runner should connect to. This selection is used across all
              features that require a project.
            </div>
            <div className="relative" ref={projectDropdownRef}>
              <button
                type="button"
                onClick={() => !connected && setShowProjectDropdown(!showProjectDropdown)}
                className={`w-full px-3 py-2 bg-input border border-border/50 rounded-md flex items-center justify-between gap-2 text-left transition-colors ${
                  connected ? "opacity-60 cursor-default" : "hover:border-border cursor-pointer"
                }`}
              >
                <span className={selectedProjectId ? "" : "text-muted-foreground"}>
                  {selectedProjectId
                    ? projects.find((p) => p.id === selectedProjectId)?.name ||
                      "Select a project..."
                    : "Select a project..."}
                </span>
                {!connected && (
                  <ChevronDown
                    className={`w-4 h-4 flex-shrink-0 transition-transform ${showProjectDropdown ? "rotate-180" : ""}`}
                  />
                )}
              </button>

              {showProjectDropdown && !connected && (
                <div className="absolute z-20 w-full mt-1 bg-card border border-border rounded-lg shadow-lg max-h-60 overflow-y-auto">
                  {projects.map((project) => (
                    <button
                      key={project.id}
                      type="button"
                      onClick={() => {
                        onProjectSelect(project.id);
                        setShowProjectDropdown(false);
                      }}
                      className="w-full px-3 py-2 text-left hover:bg-muted/50 transition-colors flex items-center justify-between gap-2 first:rounded-t-lg last:rounded-b-lg"
                    >
                      <span>{project.name}</span>
                      {selectedProjectId === project.id && (
                        <Check className="w-4 h-4 text-primary" />
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>
            {connected && (
              <div className="text-sm text-muted-foreground mt-2">
                Disconnect to change projects.
              </div>
            )}
          </div>
        </div>
      )}

      {/* Connection Controls */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Wifi className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Connection Status</h4>
          {connected && (
            <span className="flex items-center gap-2 text-green-600 text-sm">
              <span className="inline-block w-2 h-2 bg-green-600 rounded-full animate-pulse"></span>
              Connected
            </span>
          )}
        </div>

        {connectionInfo && (
          <div className="space-y-2 text-sm">
            <div className="flex justify-between">
              <span className="text-muted-foreground">Device ID:</span>
              <span className="font-mono">{connectionInfo.device_id.slice(0, 8)}...</span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">WebSocket URL:</span>
              <span className="font-mono text-xs">
                {connectionInfo.websocket_url.slice(0, 40)}...
              </span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">WebSocket:</span>
              <span className={connected ? "text-green-500" : "text-orange-500"}>
                {connected ? "Connected" : "Disconnected"}
              </span>
            </div>
          </div>
        )}

        <div className="flex gap-3 pt-4 border-t border-border/50">
          {!connected ? (
            <button
              onClick={handleConnect}
              disabled={connecting || !connectionInfo || !selectedProjectId}
              className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
            >
              {connecting ? (
                <>
                  <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
                  Connecting...
                </>
              ) : (
                <>
                  <Wifi className="w-4 h-4" />
                  Connect
                </>
              )}
            </button>
          ) : (
            <button
              onClick={handleDisconnect}
              className="px-4 py-2 bg-destructive text-destructive-foreground rounded-md hover:bg-destructive/90 transition-colors"
            >
              Disconnect
            </button>
          )}
        </div>

        {!selectedProjectId && projects.length > 0 && (
          <div className="p-3 bg-orange-500/10 border border-orange-500/30 rounded-lg">
            <div className="text-sm text-orange-300">
              Please select a project before connecting.
            </div>
          </div>
        )}

        {projects.length === 0 && (
          <div className="p-3 bg-blue-500/10 border border-blue-500/30 rounded-lg">
            <div className="text-sm text-blue-300">
              No projects found. Create a project on qontinui.io first.
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
