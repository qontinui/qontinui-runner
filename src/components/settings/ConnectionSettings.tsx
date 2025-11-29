import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, X, Camera, Wifi, Cloud, AlertCircle } from "lucide-react";
import { QRScannerDialog } from "../QRScannerDialog";
import { SectionHeader } from "./SectionHeader";
import type { WebSocketSettings, ConnectionInfo, LogFunction } from "./types";

interface ConnectionSettingsProps {
  onLog: LogFunction;
}

export function ConnectionSettings({ onLog }: ConnectionSettingsProps) {
  const [wsSettings, setWsSettings] = useState<WebSocketSettings>({
    enabled: false,
    url: "ws://localhost:8001",
    token: "",
    projectId: "",
    connected: false,
    backendUrl: import.meta.env.DEV ? "http://localhost:8000" : "https://qontinui.io/api",
    runnerName: "",
    cloudPermissionEnabled: false,
    sessionsLimit: null,
    sessionsUsed: 0,
    sessionsResetAt: null,
    sendToCloud: false,
    sendLogs: true,
    sendScreenshots: true,
    sendVideos: false,
  });

  // Quick Connect state
  const [connectionString, setConnectionString] = useState("");
  const [quickConnectError, setQuickConnectError] = useState<string | null>(null);
  const [quickConnectSuccess, setQuickConnectSuccess] = useState<string | null>(null);
  const [quickConnecting, setQuickConnecting] = useState(false);
  const [qrScannerOpen, setQrScannerOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Check cloud permission when token changes
  useEffect(() => {
    const checkCloudPermission = async () => {
      if (wsSettings.token) {
        try {
          const response = await fetch(
            `${wsSettings.backendUrl}/api/v1/users/me/automation-streaming`,
            {
              headers: {
                Authorization: `Bearer ${wsSettings.token}`,
              },
            },
          );
          if (response.ok) {
            const data = await response.json();
            setWsSettings((prev) => ({
              ...prev,
              cloudPermissionEnabled: data.enabled,
              sessionsLimit: data.sessions_limit,
              sessionsUsed: data.sessions_used,
              sessionsResetAt: data.sessions_reset_at,
            }));
          } else {
            setWsSettings((prev) => ({
              ...prev,
              cloudPermissionEnabled: false,
            }));
          }
        } catch (error) {
          console.error("Failed to check cloud permission:", error);
          setWsSettings((prev) => ({
            ...prev,
            cloudPermissionEnabled: false,
          }));
        }
      } else {
        setWsSettings((prev) => ({
          ...prev,
          cloudPermissionEnabled: false,
        }));
      }
    };

    checkCloudPermission();
  }, [wsSettings.token, wsSettings.backendUrl]);

  const parseConnectionString = (jsonString: string): ConnectionInfo => {
    try {
      const parsed = JSON.parse(jsonString);
      const requiredFields = ["version", "url", "token", "userId", "createdAt"];
      const missingFields = requiredFields.filter((field) => !(field in parsed));

      if (missingFields.length > 0) {
        throw new Error(
          `Connection string is missing required fields: ${missingFields.join(", ")}`,
        );
      }

      if (!parsed.url.startsWith("ws://") && !parsed.url.startsWith("wss://")) {
        throw new Error("Invalid WebSocket URL format (must start with ws:// or wss://)");
      }

      if (!parsed.token || parsed.token.trim() === "") {
        throw new Error("Token cannot be empty");
      }

      return {
        version: parsed.version,
        url: parsed.url,
        token: parsed.token,
        userId: parsed.userId,
        projectId: parsed.projectId || null,
        createdAt: parsed.createdAt,
        backendUrl: parsed.backendUrl || "http://localhost:8000",
      };
    } catch (err) {
      if (err instanceof SyntaxError) {
        throw new Error("Invalid connection string format (must be valid JSON)");
      }
      throw err;
    }
  };

  // Helper to wait for executor to be ready
  const waitForExecutorReady = async (maxWaitMs: number = 5000): Promise<boolean> => {
    const startTime = Date.now();
    while (Date.now() - startTime < maxWaitMs) {
      try {
        const status: any = await invoke("get_executor_status");
        // python_running is inside the data field of CommandResponse
        if (status?.data?.python_running) {
          return true;
        }
      } catch (err) {
        console.warn("Error checking executor status:", err);
      }
      // Wait 100ms before checking again
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    return false;
  };

  const handleQuickConnect = async (connectionStr?: string) => {
    setQuickConnectError(null);
    setQuickConnectSuccess(null);
    setQuickConnecting(true);

    const connStr = connectionStr || connectionString;
    console.log("[QUICK_CONNECT] Starting Quick Connect...");

    try {
      const connInfo = parseConnectionString(connStr);
      console.log("[QUICK_CONNECT] Parsed connection info:", {
        url: connInfo.url,
        backendUrl: connInfo.backendUrl,
        userId: connInfo.userId,
        projectId: connInfo.projectId,
        tokenLength: connInfo.token?.length,
        tokenPrefix: connInfo.token?.substring(0, 20) + "...",
      });

      // Wait for executor to be ready before proceeding
      console.log("[QUICK_CONNECT] Waiting for executor to be ready...");
      const executorReady = await waitForExecutorReady();
      console.log("[QUICK_CONNECT] Executor ready:", executorReady);
      if (!executorReady) {
        throw new Error("Python executor is not ready. Please wait a moment and try again.");
      }

      let cloudPermissionEnabled = false;
      const isJWT = connInfo.token?.startsWith("eyJ");
      console.log("[QUICK_CONNECT] Token type:", isJWT ? "JWT" : "Runner Token");

      try {
        if (isJWT) {
          // Token is a JWT - use Authorization header
          const jwtUrl = `${connInfo.backendUrl}/api/v1/users/me/automation-streaming`;
          console.log("[QUICK_CONNECT] Trying JWT auth endpoint:", jwtUrl);
          const permissionResponse = await fetch(jwtUrl, {
            headers: {
              Authorization: `Bearer ${connInfo.token}`,
            },
          });
          console.log("[QUICK_CONNECT] JWT auth response status:", permissionResponse.status);

          if (permissionResponse.ok) {
            const data = await permissionResponse.json();
            console.log("[QUICK_CONNECT] JWT auth response data:", data);
            cloudPermissionEnabled = data.enabled;
            console.log("[QUICK_CONNECT] cloudPermissionEnabled from JWT:", cloudPermissionEnabled);
          } else if (permissionResponse.status === 401) {
            // JWT is likely expired - try to decode and check
            try {
              const payload = JSON.parse(atob(connInfo.token.split(".")[1]));
              const expTime = payload.exp * 1000;
              const now = Date.now();
              if (expTime < now) {
                const expiredMinutes = Math.round((now - expTime) / 60000);
                throw new Error(
                  `Your session token has expired (${expiredMinutes} minutes ago). Please get a new connection string from qontinui.io/connect-runner.`,
                );
              }
            } catch (decodeErr) {
              if (decodeErr instanceof Error && decodeErr.message.includes("expired")) {
                throw decodeErr;
              }
            }
            throw new Error(
              "Authentication failed. Your token may be invalid or expired. Please get a new connection string from qontinui.io/connect-runner.",
            );
          } else {
            const errorText = await permissionResponse.text();
            console.log(
              "[QUICK_CONNECT] JWT auth failed with status",
              permissionResponse.status,
              ":",
              errorText,
            );
            throw new Error(
              `Authentication failed (${permissionResponse.status}). Please try again.`,
            );
          }
        } else {
          // Token is a runner token - use query param auth
          console.log("[QUICK_CONNECT] Trying runner token auth...");
          const testUrl = `${connInfo.backendUrl}/api/v1/runners/test-connection?token=${encodeURIComponent(connInfo.token)}`;
          console.log("[QUICK_CONNECT] Test connection URL:", testUrl);
          const testResponse = await fetch(testUrl, { method: "POST" });
          console.log("[QUICK_CONNECT] Test connection response status:", testResponse.status);

          if (testResponse.ok) {
            const testData = await testResponse.json();
            console.log("[QUICK_CONNECT] Test connection response data:", testData);
            // Runner token is valid - streaming is enabled by definition
            cloudPermissionEnabled = true;
            console.log("[QUICK_CONNECT] Runner token authentication successful");
          } else {
            const errorText = await testResponse.text();
            console.log("[QUICK_CONNECT] Test connection failed:", errorText);
            throw new Error(
              "Invalid runner token. Please get a new connection string from qontinui.io/connect-runner.",
            );
          }
        }
      } catch (err) {
        // Re-throw user-friendly errors
        if (
          err instanceof Error &&
          (err.message.includes("expired") ||
            err.message.includes("Authentication failed") ||
            err.message.includes("Invalid runner token"))
        ) {
          throw err;
        }
        console.warn("[QUICK_CONNECT] Failed to check cloud permission:", err);
        throw new Error("Failed to verify connection. Please check your network and try again.");
      }

      console.log("[QUICK_CONNECT] Final cloudPermissionEnabled:", cloudPermissionEnabled);

      const newSettings = {
        enabled: true,
        url: connInfo.url,
        token: connInfo.token,
        projectId: connInfo.projectId !== null ? connInfo.projectId.toString() : "",
        connected: false,
        backendUrl: connInfo.backendUrl,
        cloudPermissionEnabled,
        sessionsLimit: null,
        sessionsUsed: 0,
        sessionsResetAt: null,
        sendToCloud: cloudPermissionEnabled,
        sendLogs: true,
        sendScreenshots: true,
        sendVideos: false,
      };

      console.log("[QUICK_CONNECT] New settings:", {
        ...newSettings,
        token: newSettings.token?.substring(0, 20) + "...",
      });
      setWsSettings(newSettings);

      if (!newSettings.url || !newSettings.token) {
        throw new Error("WebSocket URL and token are required");
      }

      if (!cloudPermissionEnabled) {
        throw new Error(
          'Automation streaming is not enabled for your account. Enable it on the "Connect Runner" page at qontinui.io.',
        );
      }

      // projectId is now a UUID string, no parsing needed
      const projectId = newSettings.projectId || null;
      const runnerName = newSettings.runnerName || null;
      console.log(
        "[QUICK_CONNECT] Configuring WebSocket with URL:",
        newSettings.url,
        "projectId:",
        projectId,
        "runnerName:",
        runnerName,
      );

      const configResult: any = await invoke("configure_websocket", {
        config: {
          enabled: newSettings.enabled,
          url: newSettings.url,
          token: newSettings.token,
          project_id: projectId,
          runner_name: runnerName,
        },
      });
      console.log("[QUICK_CONNECT] configure_websocket result:", configResult);

      if (!configResult || !configResult.success) {
        throw new Error(
          `Failed to configure WebSocket: ${configResult?.message || "Unknown error"}`,
        );
      }

      console.log("[QUICK_CONNECT] Connecting WebSocket...");
      const connectResult: any = await invoke("connect_websocket");
      console.log("[QUICK_CONNECT] connect_websocket result:", connectResult);

      if (!connectResult || !connectResult.success) {
        throw new Error(
          `Failed to connect WebSocket: ${connectResult?.message || "Unknown error"}`,
        );
      }

      console.log("[QUICK_CONNECT] WebSocket connected! Testing connection with backend...");

      // Test connection with backend to confirm everything is working
      let successMessage = "Connected successfully! Your settings have been configured.";
      try {
        const testResponse = await fetch(
          `${connInfo.backendUrl}/api/v1/runners/test-connection?token=${encodeURIComponent(connInfo.token)}`,
          { method: "POST" },
        );
        console.log("[QUICK_CONNECT] test-connection response status:", testResponse.status);
        if (testResponse.ok) {
          const testResult = await testResponse.json();
          console.log("[QUICK_CONNECT] test-connection result:", testResult);
          successMessage = testResult.message || successMessage;
          onLog(
            "success",
            `Quick Connect: Connection verified - ${testResult.auth_method === "runner_token" ? `Token: ${testResult.token_name}` : "JWT Auth"}`,
          );
        }
      } catch (testErr) {
        // Non-fatal - connection still works, just couldn't verify
        console.warn("[QUICK_CONNECT] Failed to verify connection with backend:", testErr);
      }

      setWsSettings((prev) => ({ ...prev, connected: true }));
      setQuickConnectSuccess(successMessage);
      setConnectionString("");
      console.log("[QUICK_CONNECT] SUCCESS! Connection complete.");
      onLog("success", "Quick Connect: WebSocket connected successfully");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      setQuickConnectError(errorMessage);
      onLog("error", `Quick Connect failed: ${errorMessage}`);
    } finally {
      setQuickConnecting(false);
    }
  };

  const handleQRScan = (data: string) => {
    setQrScannerOpen(false);
    setConnectionString(data);
    handleQuickConnect(data);
  };

  const applyWebSocketSettings = async () => {
    if (!wsSettings.enabled) {
      onLog("info", "WebSocket streaming is disabled");
      return;
    }

    if (!wsSettings.url || !wsSettings.token) {
      onLog("error", "WebSocket URL and token are required");
      setError("WebSocket URL and token are required");
      return;
    }

    if (!wsSettings.cloudPermissionEnabled) {
      onLog(
        "error",
        'Automation streaming is not enabled. Enable it on the "Connect Runner" page at qontinui.io.',
      );
      setError(
        'Automation streaming is not enabled. Enable it on the "Connect Runner" page at qontinui.io.',
      );
      return;
    }

    if (!wsSettings.sendToCloud) {
      onLog("info", "Cloud sync disabled by user preference");
      return;
    }

    try {
      // projectId is now a UUID string, no parsing needed
      const projectId = wsSettings.projectId || null;
      const runnerName = wsSettings.runnerName || null;

      const configResult: any = await invoke("configure_websocket", {
        config: {
          enabled: wsSettings.enabled,
          url: wsSettings.url,
          token: wsSettings.token,
          project_id: projectId,
          runner_name: runnerName,
        },
      });

      if (configResult && configResult.success) {
        onLog("success", "WebSocket configured successfully");

        const connectResult: any = await invoke("connect_websocket");
        if (connectResult && connectResult.success) {
          setWsSettings((prev) => ({ ...prev, connected: true }));
          onLog("success", "WebSocket connected successfully");
        } else {
          onLog(
            "error",
            `Failed to connect WebSocket: ${connectResult?.message || "Unknown error"}`,
          );
        }
      } else {
        onLog(
          "error",
          `Failed to configure WebSocket: ${configResult?.message || "Unknown error"}`,
        );
      }
    } catch (err) {
      console.error("Failed to apply WebSocket settings:", err);
      onLog("error", `Failed to apply WebSocket settings: ${err}`);
      setError(`Failed to apply WebSocket settings: ${err}`);
    }
  };

  const disconnectWebSocket = async () => {
    try {
      const result: any = await invoke("disconnect_websocket");
      if (result && result.success) {
        setWsSettings((prev) => ({ ...prev, connected: false }));
        onLog("success", "WebSocket disconnected");
      } else {
        onLog("error", `Failed to disconnect WebSocket: ${result?.message || "Unknown error"}`);
      }
    } catch (err) {
      console.error("Failed to disconnect WebSocket:", err);
      onLog("error", `Failed to disconnect WebSocket: ${err}`);
    }
  };

  const handleToggleSendToCloud = (checked: boolean) => {
    setWsSettings((prev) => ({
      ...prev,
      sendToCloud: checked,
      sendLogs: checked ? true : prev.sendLogs,
      sendScreenshots: checked ? true : prev.sendScreenshots,
    }));
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Connection"
        description="Connect your runner to qontinui.io for real-time monitoring, cloud storage, and collaboration features."
        icon={<Wifi className="w-6 h-6" />}
      />

      {/* Error message */}
      {error && (
        <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
          <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
          <span className="text-red-400 text-sm">{error}</span>
        </div>
      )}

      {/* Quick Connect Section */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Wifi className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Quick Connect to qontinui.io</h4>
        </div>

        <div className="text-sm text-muted-foreground">
          Paste connection details from <strong>qontinui.io/connect-runner</strong> to connect
          instantly
        </div>

        {quickConnectError && (
          <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
            <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
            <span className="text-red-400 text-sm">{quickConnectError}</span>
          </div>
        )}

        {quickConnectSuccess && (
          <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
            <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
            <span className="text-green-400 text-sm">{quickConnectSuccess}</span>
          </div>
        )}

        <div className="space-y-2">
          <label className="block">
            <div className="font-medium mb-1">Runner Name</div>
            <div className="text-sm text-muted-foreground mb-3">
              Give this runner a name to identify it on qontinui.io (e.g., "My Laptop", "Work
              Desktop")
            </div>
            <input
              type="text"
              value={wsSettings.runnerName}
              onChange={(e) => setWsSettings((prev) => ({ ...prev, runnerName: e.target.value }))}
              placeholder="My Runner"
              className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
            />
          </label>
        </div>

        <div className="space-y-2">
          <label className="block">
            <div className="font-medium mb-1">Connection String</div>
            <div className="text-sm text-muted-foreground mb-3">
              Paste the JSON connection string from qontinui.io or scan a QR code
            </div>
            <textarea
              value={connectionString}
              onChange={(e) => setConnectionString(e.target.value)}
              placeholder='{"version":"1.0","url":"ws://localhost:8001","token":"eyJ...","userId":"...","projectId":1,"createdAt":"..."}'
              rows={6}
              className="w-full px-3 py-2 bg-input border border-border/50 rounded-md font-mono text-xs resize-y"
            />
          </label>
        </div>

        <div className="flex flex-wrap gap-3">
          <button
            onClick={() => setQrScannerOpen(true)}
            className="flex items-center gap-2 px-4 py-2 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors font-medium"
          >
            <Camera className="w-4 h-4" />
            Scan QR Code
          </button>
          <button
            onClick={() => handleQuickConnect()}
            disabled={quickConnecting || !connectionString.trim()}
            className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
          >
            {quickConnecting ? (
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
          {connectionString.trim() && (
            <button
              onClick={() => {
                setConnectionString("");
                setQuickConnectError(null);
                setQuickConnectSuccess(null);
              }}
              className="px-4 py-2 bg-muted hover:bg-muted/80 text-foreground rounded-md font-medium transition-colors"
            >
              Clear
            </button>
          )}
        </div>

        <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
          <div className="text-sm text-muted-foreground">
            <strong className="text-foreground">How it works:</strong> Quick Connect automatically
            configures your WebSocket settings and tests the connection. You can find your
            connection string at qontinui.io/connect-runner (requires login).
          </div>
        </div>
      </div>

      <QRScannerDialog open={qrScannerOpen} onOpenChange={setQrScannerOpen} onScan={handleQRScan} />

      {/* Cloud Sync Settings */}
      <div
        className="space-y-6 bg-card rounded-lg border border-border/50 p-6"
        data-section="cloud-sync"
      >
        <div className="flex items-center gap-3">
          <Cloud className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Cloud Sync</h4>
          {wsSettings.connected && (
            <span className="flex items-center gap-2 text-green-600 text-sm">
              <span className="inline-block w-2 h-2 bg-green-600 rounded-full animate-pulse"></span>
              Connected
            </span>
          )}
        </div>

        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              <div className="font-medium">Enable Automation Streaming</div>
              <div className="text-sm text-muted-foreground">
                Connect to qontinui.io for real-time monitoring and cloud storage
              </div>
            </div>
            <button
              onClick={() => setWsSettings((prev) => ({ ...prev, enabled: !prev.enabled }))}
              className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                wsSettings.enabled ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                  wsSettings.enabled ? "translate-x-6" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        {wsSettings.enabled && (
          <>
            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">Backend URL</div>
                <div className="text-sm text-muted-foreground mb-3">
                  qontinui-web backend API endpoint (HTTP/HTTPS)
                  <br />
                  {import.meta.env.DEV && (
                    <span className="text-xs text-yellow-500">
                      Dev Mode: For WSL, use WSL IP (e.g., http://172.x.x.x:8000)
                    </span>
                  )}
                  {!import.meta.env.DEV && (
                    <span className="text-xs text-green-500">
                      Production: Defaults to https://qontinui.io/api
                    </span>
                  )}
                </div>
                <input
                  type="text"
                  value={wsSettings.backendUrl}
                  onChange={(e) =>
                    setWsSettings((prev) => ({ ...prev, backendUrl: e.target.value }))
                  }
                  placeholder={
                    import.meta.env.DEV ? "http://localhost:8000" : "https://qontinui.io/api"
                  }
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
                />
              </label>
            </div>

            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">WebSocket URL</div>
                <div className="text-sm text-muted-foreground mb-3">
                  qontinui-web backend WebSocket endpoint
                </div>
                <input
                  type="text"
                  value={wsSettings.url}
                  onChange={(e) => setWsSettings((prev) => ({ ...prev, url: e.target.value }))}
                  placeholder="ws://localhost:8001"
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
                />
              </label>
            </div>

            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">JWT Token</div>
                <div className="text-sm text-muted-foreground mb-3">
                  Authentication token from qontinui.io
                </div>
                <input
                  type="password"
                  value={wsSettings.token}
                  onChange={(e) => setWsSettings((prev) => ({ ...prev, token: e.target.value }))}
                  placeholder="eyJhbGciOiJIUzI1NiIsInR5cCI6..."
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md font-mono text-sm"
                />
              </label>
            </div>

            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">Project ID</div>
                <div className="text-sm text-muted-foreground mb-3">
                  Project identifier (integer)
                </div>
                <input
                  type="text"
                  value={wsSettings.projectId}
                  onChange={(e) =>
                    setWsSettings((prev) => ({ ...prev, projectId: e.target.value }))
                  }
                  placeholder="1"
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
                />
              </label>
            </div>

            {wsSettings.cloudPermissionEnabled && wsSettings.token && (
              <div className="p-3 bg-primary/10 border border-primary/30 rounded-lg">
                <div className="text-sm">
                  <strong>Cloud Storage Available</strong>
                  <p className="text-muted-foreground mt-1">
                    {wsSettings.sessionsLimit
                      ? `${wsSettings.sessionsUsed} of ${wsSettings.sessionsLimit} sessions used this month`
                      : "Unlimited sessions available"}
                  </p>
                </div>
              </div>
            )}

            {!wsSettings.cloudPermissionEnabled && wsSettings.token && (
              <div className="p-3 bg-orange-500/10 border border-orange-500/30 rounded-lg flex items-start gap-2">
                <AlertCircle className="h-4 w-4 text-orange-400 shrink-0 mt-0.5" />
                <div className="text-sm text-orange-300">
                  Automation streaming is not enabled for your account. Enable it on the "Connect
                  Runner" page at qontinui.io.
                </div>
              </div>
            )}

            <div className="space-y-2">
              <label className="flex items-center justify-between cursor-pointer">
                <div className="space-y-1">
                  <div className="font-medium">Send to Cloud</div>
                  <div className="text-sm text-muted-foreground">
                    Upload automation data to qontinui.io for analysis and collaboration
                  </div>
                </div>
                <button
                  onClick={() => handleToggleSendToCloud(!wsSettings.sendToCloud)}
                  disabled={!wsSettings.cloudPermissionEnabled}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                    wsSettings.sendToCloud ? "bg-primary" : "bg-muted"
                  } ${!wsSettings.cloudPermissionEnabled ? "opacity-50 cursor-not-allowed" : ""}`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                      wsSettings.sendToCloud ? "translate-x-6" : "translate-x-1"
                    }`}
                  />
                </button>
              </label>
            </div>

            {wsSettings.sendToCloud && (
              <div className="space-y-3 pl-6 border-l-2 border-primary/30">
                <div className="text-sm font-medium">What to send:</div>

                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={wsSettings.sendLogs}
                    onChange={(e) =>
                      setWsSettings((prev) => ({ ...prev, sendLogs: e.target.checked }))
                    }
                    className="w-4 h-4 rounded border-gray-300 text-primary focus:ring-primary"
                  />
                  <span className="text-sm">Logs & Events (required for analysis)</span>
                </label>

                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={wsSettings.sendScreenshots}
                    onChange={(e) =>
                      setWsSettings((prev) => ({ ...prev, sendScreenshots: e.target.checked }))
                    }
                    className="w-4 h-4 rounded border-gray-300 text-primary focus:ring-primary"
                  />
                  <span className="text-sm">Screenshots (required for State Discovery)</span>
                </label>

                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={wsSettings.sendVideos}
                    onChange={(e) =>
                      setWsSettings((prev) => ({ ...prev, sendVideos: e.target.checked }))
                    }
                    className="w-4 h-4 rounded border-gray-300 text-primary focus:ring-primary"
                  />
                  <span className="text-sm">Videos (optional, large files)</span>
                </label>
              </div>
            )}

            <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
              <div className="text-sm text-muted-foreground">
                <strong className="text-foreground">Note:</strong> Cloud sync requires both account
                permission and your consent. You can control exactly what data is uploaded.
              </div>
            </div>

            <div className="flex gap-3">
              {!wsSettings.connected ? (
                <button
                  onClick={applyWebSocketSettings}
                  className="px-4 py-2 bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
                >
                  Connect
                </button>
              ) : (
                <button
                  onClick={disconnectWebSocket}
                  className="px-4 py-2 bg-destructive text-destructive-foreground rounded-md hover:bg-destructive/90 transition-colors"
                >
                  Disconnect
                </button>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
