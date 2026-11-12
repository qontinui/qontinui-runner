/**
 * PhysicalDeviceManager.tsx
 *
 * UI for managing physical device connections in the runner.
 * Supports viewing connected devices, pairing a device, and manual LAN registration.
 * Fetches from the runner's /ui-bridge/devices/* API endpoints.
 *
 * Pairing is the phone's: the runner asks it for a token
 * (POST /ui-bridge/devices/{id}/pair) and signs every later request to it with
 * that token. Over USB the phone asks no code. On Wi-Fi it asks for the 6-digit
 * code its Settings shows after Pair is tapped. The token never reaches this UI.
 */

import { useState, useEffect, useCallback } from "react";
import {
  Smartphone,
  Tablet,
  RefreshCw,
  Plus,
  Plug,
  Unplug,
  Wifi,
  Usb,
  Cloud,
  AlertCircle,
} from "lucide-react";
import { getApiBase } from "@/lib/runner-api";
import { tracedFetch } from "@/lib/runner-api";
import { ConnectionWizard } from "../connection-wizard/ConnectionWizard";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface DeviceTransport {
  kind: "usb" | "lan" | "cloud";
  proxyUrl: string;
  establishedAt: number;
  lastHealthyAt: number;
  failCount: number;
}

interface PhysicalDevice {
  info: {
    id: string;
    os: "android" | "ios";
    deviceKind: string;
    model?: string;
    appId?: string;
    uiBridgeVersion?: string;
    firstSeenAt: number;
  };
  transports: DeviceTransport[];
  activeTransportIdx: number;
  healthState: "healthy" | "degraded" | "unreachable";
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
  code?: string;
}

interface DeviceListData {
  devices: PhysicalDevice[];
  count: number;
}

interface PairResult {
  deviceId: string;
  uiBridgeDeviceId: string;
  transport: "usb" | "lan" | "cloud";
  paired: boolean;
}

interface PairNotice {
  deviceId: string;
  ok: boolean;
  text: string;
  /**
   * Set when the runner refused to replace the token it holds for the phone
   * (`PAIRING_TOKEN_HELD`). The phone spent the code on that pairing, so a retry
   * with `replace: true` needs a new one. The operator should replace only if they
   * revoked the pairing in the app's Settings.
   */
  offerReplace?: boolean;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function transportIcon(kind: "usb" | "lan" | "cloud") {
  switch (kind) {
    case "usb":
      return <Usb className="w-3 h-3" />;
    case "lan":
      return <Wifi className="w-3 h-3" />;
    case "cloud":
      return <Cloud className="w-3 h-3" />;
  }
}

function transportBadgeClass(kind: "usb" | "lan" | "cloud") {
  switch (kind) {
    case "usb":
      return "bg-blue-500/15 text-blue-400 border border-blue-500/20";
    case "lan":
      return "bg-green-500/15 text-green-400 border border-green-500/20";
    case "cloud":
      return "bg-purple-500/15 text-purple-400 border border-purple-500/20";
  }
}

/**
 * The transport a pairing goes over: the best one other than the cloud relay,
 * which needs no pairing. Mirrors the runner's own choice in POST .../pair.
 */
function pairingTransportOf(device: PhysicalDevice): DeviceTransport | null {
  const active = device.transports[device.activeTransportIdx];
  const rest = device.transports.filter((_, idx) => idx !== device.activeTransportIdx);
  return [active, ...rest].find((t) => t !== undefined && t.kind !== "cloud") ?? null;
}

const PAIRING_CODE = /^\d{6}$/;

function healthDotClass(state: "healthy" | "degraded" | "unreachable") {
  switch (state) {
    case "healthy":
      return "bg-green-500 shadow-[0_0_6px_rgba(34,197,94,0.4)]";
    case "degraded":
      return "bg-yellow-400 shadow-[0_0_6px_rgba(234,179,8,0.4)]";
    case "unreachable":
      return "bg-red-500";
  }
}

function healthLabel(state: "healthy" | "degraded" | "unreachable") {
  switch (state) {
    case "healthy":
      return "Healthy";
    case "degraded":
      return "Degraded";
    case "unreachable":
      return "Unreachable";
  }
}

function healthTextClass(state: "healthy" | "degraded" | "unreachable") {
  switch (state) {
    case "healthy":
      return "text-green-400";
    case "degraded":
      return "text-yellow-400";
    case "unreachable":
      return "text-red-400";
  }
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function PhysicalDeviceManager() {
  const [devices, setDevices] = useState<PhysicalDevice[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [showWizard, setShowWizard] = useState(false);

  // Pairing state: which card shows the code entry, and the last outcome.
  const [pairOpenId, setPairOpenId] = useState<string | null>(null);
  const [pairCode, setPairCode] = useState("");
  const [pairBusyId, setPairBusyId] = useState<string | null>(null);
  const [pairNotice, setPairNotice] = useState<PairNotice | null>(null);
  // The device whose next code pairing replaces the token the runner holds.
  const [replaceId, setReplaceId] = useState<string | null>(null);

  // Manual LAN registration
  const [showLanSection, setShowLanSection] = useState(false);
  const [lanIp, setLanIp] = useState("");
  const [lanPort, setLanPort] = useState("8087");
  const [lanError, setLanError] = useState<string | null>(null);
  const [lanLoading, setLanLoading] = useState(false);
  const [lanSuccess, setLanSuccess] = useState<string | null>(null);

  // Per-device action state
  const [connectingDeviceId, setConnectingDeviceId] = useState<string | null>(null);

  // ---------------------------------------------------------------------------
  // Data fetching
  // ---------------------------------------------------------------------------

  const fetchDevices = useCallback(async () => {
    try {
      const res = await tracedFetch(`${getApiBase()}/ui-bridge/devices`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const json: ApiResponse<DeviceListData> = await res.json();
      if (json.success && json.data) {
        setDevices(json.data.devices);
        setError(null);
      } else {
        setError(json.error ?? "Failed to load devices");
      }
    } catch (err) {
      // Silently ignore on background polls; show error only on initial load
      if (loading) {
        setError(`Could not reach device API: ${err}`);
      }
    } finally {
      setLoading(false);
    }
  }, [loading]);

  useEffect(() => {
    let cancelled = false;
    // Defer via microtask so the effect body itself doesn't synchronously
    // trigger setState inside fetchDevices.
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchDevices();
    });
    const interval = setInterval(fetchDevices, 10000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ---------------------------------------------------------------------------
  // Connect / Disconnect
  // ---------------------------------------------------------------------------

  const connectDevice = async (id: string) => {
    setConnectingDeviceId(id);
    try {
      const res = await tracedFetch(
        `${getApiBase()}/ui-bridge/devices/${encodeURIComponent(id)}/connect`,
        {
          method: "POST",
        },
      );
      const json: ApiResponse<unknown> = await res.json();
      if (!json.success && json.code === "PAIRING_REQUIRED") {
        // The phone requires pairing: open its code entry rather than an error banner.
        setPairOpenId(id);
        setPairNotice({ deviceId: id, ok: false, text: json.error ?? "Pairing required" });
        return;
      }
      if (!json.success) {
        throw new Error(json.error ?? "Connect failed");
      }
      await fetchDevices();
    } catch (err) {
      setError(`Failed to connect device: ${err}`);
    } finally {
      setConnectingDeviceId(null);
    }
  };

  const disconnectDevice = async (id: string) => {
    setConnectingDeviceId(id);
    try {
      const res = await tracedFetch(
        `${getApiBase()}/ui-bridge/devices/${encodeURIComponent(id)}/disconnect`,
        {
          method: "POST",
        },
      );
      const json: ApiResponse<unknown> = await res.json();
      if (!json.success) {
        throw new Error(json.error ?? "Disconnect failed");
      }
      await fetchDevices();
    } catch (err) {
      setError(`Failed to disconnect device: ${err}`);
    } finally {
      setConnectingDeviceId(null);
    }
  };

  // ---------------------------------------------------------------------------
  // Pairing
  // ---------------------------------------------------------------------------

  const pairDevice = async (id: string, code?: string, replace = false) => {
    setPairBusyId(id);
    setPairNotice(null);
    try {
      const res = await tracedFetch(
        `${getApiBase()}/ui-bridge/devices/${encodeURIComponent(id)}/pair`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ...(code ? { code } : {}), ...(replace ? { replace } : {}) }),
        },
      );
      const json: ApiResponse<PairResult> = await res.json();
      if (json.success && json.data) {
        // A pair makes the paired transport active and drops the device's live
        // connections, so the list changes and the next Connect goes to it, signed.
        setPairNotice({
          deviceId: id,
          ok: true,
          text: `Paired over ${json.data.transport.toUpperCase()}. Connect to drive it.`,
        });
        setPairOpenId(null);
        setPairCode("");
        setReplaceId(null);
        await fetchDevices();
      } else {
        // A code-less pair the phone answers with "a code is needed" (whatever the
        // transport) opens the code entry, so the operator can type the one the
        // app's Settings shows after Pair is tapped.
        if (!code && (json.code === "PAIRING_CODE_REQUIRED" || json.code === "PAIRING_NOT_ARMED")) {
          setPairOpenId(id);
          setPairCode("");
        }
        // The phone spent its code on a pairing the runner refused to keep, so the
        // code entry is cleared for a new one.
        if (json.code === "PAIRING_TOKEN_HELD") {
          setPairOpenId(id);
          setPairCode("");
        }
        setPairNotice({
          deviceId: id,
          ok: false,
          text: json.error ?? "Pairing failed",
          offerReplace: json.code === "PAIRING_TOKEN_HELD",
        });
      }
    } catch (err) {
      setPairNotice({ deviceId: id, ok: false, text: `Pairing request failed: ${err}` });
    } finally {
      setPairBusyId(null);
    }
  };

  const startPairing = (device: PhysicalDevice) => {
    const transport = pairingTransportOf(device);
    if (transport?.kind === "usb") {
      // Over a cable the phone asks no code.
      void pairDevice(device.info.id);
      return;
    }
    setPairOpenId((open) => (open === device.info.id ? null : device.info.id));
    setPairCode("");
    setPairNotice(null);
    setReplaceId(null);
  };

  // ---------------------------------------------------------------------------
  // Manual LAN registration
  // ---------------------------------------------------------------------------

  const registerLanDevice = async () => {
    if (!lanIp.trim()) {
      setLanError("IP address is required");
      return;
    }
    setLanLoading(true);
    setLanError(null);
    setLanSuccess(null);
    try {
      const res = await tracedFetch(`${getApiBase()}/ui-bridge/devices/register-lan`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        // The route takes one `address`, "IP:port".
        body: JSON.stringify({ address: `${lanIp.trim()}:${parseInt(lanPort, 10) || 8087}` }),
      });
      const json: ApiResponse<PhysicalDevice> = await res.json();
      if (json.success) {
        const registeredId = json.data?.info.id;
        setLanSuccess(`Device registered${registeredId ? ` (${registeredId})` : ""}`);
        setLanIp("");
        await fetchDevices();
      } else {
        setLanError(json.error ?? "Registration failed");
      }
    } catch (err) {
      setLanError(`Registration request failed: ${err}`);
    } finally {
      setLanLoading(false);
    }
  };

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  const activeTransportOf = (device: PhysicalDevice): DeviceTransport | null => {
    return device.transports[device.activeTransportIdx] ?? null;
  };

  return (
    <div className="space-y-4">
      <ConnectionWizard
        open={showWizard}
        onClose={() => {
          setShowWizard(false);
          fetchDevices();
        }}
      />
      {/* Header row */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Smartphone className="w-4 h-4 text-primary/70" />
          <h4 className="font-medium text-sm">Physical Devices</h4>
          {!loading && (
            <span className="text-[10px] text-muted-foreground">
              {devices.length === 0
                ? "No devices"
                : `${devices.length} device${devices.length !== 1 ? "s" : ""}`}
            </span>
          )}
        </div>
        <button
          onClick={() => {
            setLoading(true);
            fetchDevices();
          }}
          disabled={loading}
          className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted rounded transition-colors"
          title="Refresh device list"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>

      {/* API error banner */}
      {error && (
        <div className="flex items-start gap-2 p-2.5 rounded-md bg-destructive/10 border border-destructive/20">
          <AlertCircle className="w-4 h-4 text-destructive shrink-0 mt-0.5" />
          <p className="text-xs text-destructive">{error}</p>
        </div>
      )}

      {/* Device cards */}
      {loading ? (
        <div className="flex items-center justify-center py-8 text-xs text-muted-foreground">
          <RefreshCw className="w-4 h-4 animate-spin mr-2" />
          Loading devices...
        </div>
      ) : devices.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-8 gap-2 text-center rounded-lg bg-muted/20 border border-dashed border-muted">
          <Tablet className="w-8 h-8 text-muted-foreground/40" />
          <p className="text-xs text-muted-foreground">No physical devices registered.</p>
          <p className="text-[10px] text-muted-foreground/70">
            Use the Connection Wizard or add a LAN device below.
          </p>
        </div>
      ) : (
        <div className="space-y-2">
          {devices.map((device) => {
            const activeTransport = activeTransportOf(device);
            const isBusy = connectingDeviceId === device.info.id;
            const isConnected = activeTransport !== null && device.healthState !== "unreachable";
            const pairingTransport = pairingTransportOf(device);
            const isPairing = pairBusyId === device.info.id;
            const notice = pairNotice?.deviceId === device.info.id ? pairNotice : null;

            return (
              <div
                key={device.info.id}
                className="rounded-lg bg-card/50 border border-border/50 p-3 space-y-2"
              >
                {/* Top row: icon + name + health */}
                <div className="flex items-start justify-between gap-2">
                  <div className="flex items-center gap-2 min-w-0">
                    {device.info.os === "ios" ? (
                      <Tablet className="w-4 h-4 text-muted-foreground shrink-0" />
                    ) : (
                      <Smartphone className="w-4 h-4 text-muted-foreground shrink-0" />
                    )}
                    <div className="min-w-0">
                      <div className="flex items-center gap-1.5">
                        <span className="text-sm font-medium truncate">
                          {device.info.model ?? device.info.id}
                        </span>
                        <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted/60 text-muted-foreground uppercase shrink-0">
                          {device.info.os}
                        </span>
                      </div>
                      <p className="text-[10px] text-muted-foreground truncate">{device.info.id}</p>
                    </div>
                  </div>

                  {/* Health indicator */}
                  <div
                    className={`flex items-center gap-1 shrink-0 text-[10px] font-medium ${healthTextClass(device.healthState)}`}
                  >
                    <div className={`w-2 h-2 rounded-full ${healthDotClass(device.healthState)}`} />
                    {healthLabel(device.healthState)}
                  </div>
                </div>

                {/* Transport row */}
                <div className="flex items-center gap-2 flex-wrap">
                  {activeTransport ? (
                    <span
                      className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium ${transportBadgeClass(activeTransport.kind)}`}
                    >
                      {transportIcon(activeTransport.kind)}
                      {activeTransport.kind.toUpperCase()} active
                    </span>
                  ) : (
                    <span className="text-[10px] text-muted-foreground">No active transport</span>
                  )}

                  {/* Available transports */}
                  {device.transports.length > 1 &&
                    device.transports.map((t, idx) => {
                      if (idx === device.activeTransportIdx) return null;
                      return (
                        <span
                          key={idx}
                          className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] text-muted-foreground border border-border/40"
                        >
                          {transportIcon(t.kind)}
                          {t.kind}
                        </span>
                      );
                    })}
                </div>

                {/* App ID / version if present */}
                {(device.info.appId || device.info.uiBridgeVersion) && (
                  <p className="text-[10px] text-muted-foreground">
                    {device.info.appId && <span>{device.info.appId}</span>}
                    {device.info.appId && device.info.uiBridgeVersion && <span> · </span>}
                    {device.info.uiBridgeVersion && <span>SDK v{device.info.uiBridgeVersion}</span>}
                  </p>
                )}

                {/* Pairing code entry (Wi-Fi), and the last pairing outcome */}
                {pairOpenId === device.info.id && (
                  <div className="space-y-2 rounded-md bg-muted/30 border border-border/50 p-2.5">
                    <p className="text-[10px] text-muted-foreground">
                      {replaceId === device.info.id
                        ? "Tap Pair in the app's Settings again and enter the new code: the last one is spent. Pressing Replace keeps the token the phone answers with, in place of the one this runner holds."
                        : "Tap Pair in the app's Settings and enter the 6-digit code it shows."}
                    </p>
                    <div className="flex gap-2">
                      <input
                        aria-label="Pairing code"
                        type="text"
                        inputMode="numeric"
                        autoComplete="one-time-code"
                        maxLength={6}
                        value={pairCode}
                        onChange={(e) => setPairCode(e.target.value.replace(/\D/g, ""))}
                        placeholder="123456"
                        className="w-28 px-2.5 py-1.5 font-mono text-sm tracking-[0.3em] bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
                      />
                      <button
                        onClick={() =>
                          pairDevice(device.info.id, pairCode, replaceId === device.info.id)
                        }
                        disabled={isPairing || !PAIRING_CODE.test(pairCode)}
                        className="px-3 py-1.5 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                      >
                        {isPairing
                          ? "Pairing..."
                          : replaceId === device.info.id
                            ? "Replace"
                            : "Pair"}
                      </button>
                      <button
                        onClick={() => {
                          setPairOpenId(null);
                          setReplaceId(null);
                        }}
                        className="px-3 py-1.5 text-xs font-medium rounded-md bg-muted text-muted-foreground hover:text-foreground hover:bg-muted/80 transition-colors"
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                )}
                {notice && (
                  <p className={`text-[10px] ${notice.ok ? "text-green-400" : "text-destructive"}`}>
                    {notice.text}
                  </p>
                )}
                {notice?.offerReplace && replaceId !== device.info.id && (
                  <div className="flex justify-end">
                    <button
                      onClick={() => {
                        // The refused pairing spent the code, so this only arms the
                        // next one: the operator enters a new code to replace.
                        setReplaceId(device.info.id);
                        setPairOpenId(device.info.id);
                        setPairCode("");
                      }}
                      disabled={isPairing}
                      title="Only if you revoked this runner's pairing in the app's Settings"
                      className="px-2.5 py-1 text-xs rounded-md border border-destructive/30 text-destructive hover:bg-destructive/10 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      I revoked it in the app: replace the pairing
                    </button>
                  </div>
                )}

                {/* Pair + Connect / Disconnect buttons */}
                <div className="flex justify-end gap-2 pt-1">
                  {pairingTransport && (
                    <button
                      onClick={() => startPairing(device)}
                      disabled={isPairing}
                      title={
                        pairingTransport.kind === "usb"
                          ? "Fetch this phone's pairing token over USB (no code needed)"
                          : "Pair over Wi-Fi with the code the app shows"
                      }
                      className="flex items-center gap-1.5 px-2.5 py-1 text-xs rounded-md border border-border bg-transparent text-foreground hover:bg-muted/30 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      {isPairing ? "Pairing..." : "Pair"}
                    </button>
                  )}
                  {isConnected ? (
                    <button
                      onClick={() => disconnectDevice(device.info.id)}
                      disabled={isBusy}
                      className="flex items-center gap-1.5 px-2.5 py-1 text-xs rounded-md bg-destructive/10 text-destructive hover:bg-destructive/20 border border-destructive/20 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <Unplug className="w-3 h-3" />
                      {isBusy ? "Disconnecting..." : "Disconnect"}
                    </button>
                  ) : (
                    <button
                      onClick={() => connectDevice(device.info.id)}
                      disabled={isBusy}
                      className="flex items-center gap-1.5 px-2.5 py-1 text-xs rounded-md bg-primary/10 text-primary hover:bg-primary/20 border border-primary/20 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <Plug className="w-3 h-3" />
                      {isBusy ? "Connecting..." : "Connect"}
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Pair New Device */}
      <div className="rounded-lg bg-card/50 p-3 space-y-3">
        <div className="flex items-center justify-between">
          <div>
            <h5 className="text-sm font-medium">Pair New Device</h5>
            <p className="text-[10px] text-muted-foreground mt-0.5">
              Connect a phone, then pair it in its row above: over USB no code is needed, and on
              Wi-Fi the app&apos;s Settings shows a 6-digit code after Pair is tapped.
            </p>
          </div>
          <button
            onClick={() => setShowWizard(true)}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
          >
            <Plus className="w-3 h-3" />
            Connection Wizard
          </button>
        </div>
      </div>

      {/* Manual LAN Registration */}
      <div className="rounded-lg bg-card/50 p-3 space-y-3">
        <div className="flex items-center justify-between">
          <div>
            <h5 className="text-sm font-medium">Add via IP Address</h5>
            <p className="text-[10px] text-muted-foreground mt-0.5">
              Directly register a device on the local network by IP.
            </p>
          </div>
          <button
            onClick={() => setShowLanSection((v) => !v)}
            className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted rounded transition-colors"
          >
            <Wifi className="w-3 h-3" />
            {showLanSection ? "Hide" : "Show"}
          </button>
        </div>

        {showLanSection && (
          <div className="space-y-3">
            <div className="flex gap-2">
              <div className="flex-1 space-y-1">
                <label htmlFor="lan-ip" className="text-[10px] font-medium">
                  IP Address
                </label>
                <input
                  id="lan-ip"
                  type="text"
                  value={lanIp}
                  onChange={(e) => {
                    setLanIp(e.target.value);
                    setLanError(null);
                    setLanSuccess(null);
                  }}
                  placeholder="192.168.1.100"
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
                />
              </div>
              <div className="w-24 space-y-1">
                <label htmlFor="lan-port" className="text-[10px] font-medium">
                  Port
                </label>
                <input
                  id="lan-port"
                  type="number"
                  value={lanPort}
                  onChange={(e) => setLanPort(e.target.value)}
                  min={1}
                  max={65535}
                  className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
                />
              </div>
              <div className="flex items-end">
                <button
                  onClick={registerLanDevice}
                  disabled={lanLoading || !lanIp.trim()}
                  className="px-3 py-1.5 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed whitespace-nowrap"
                >
                  {lanLoading ? "Adding..." : "Add Device"}
                </button>
              </div>
            </div>

            {lanError && (
              <div className="flex items-start gap-2 p-2.5 rounded-md bg-destructive/10 border border-destructive/20">
                <AlertCircle className="w-4 h-4 text-destructive shrink-0 mt-0.5" />
                <p className="text-xs text-destructive">{lanError}</p>
              </div>
            )}

            {lanSuccess && (
              <div className="p-2.5 rounded-md bg-green-500/10 border border-green-500/20">
                <p className="text-xs text-green-400">{lanSuccess}</p>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
