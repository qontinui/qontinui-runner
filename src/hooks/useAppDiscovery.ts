/**
 * useAppDiscovery Hook
 *
 * Discovers UI Bridge-enabled applications across web, desktop, and mobile
 * by calling the runner's /ui-bridge/apps/scan endpoints.
 *
 * Also polls /ui-bridge/apps/registered every 5s to surface apps that phoned
 * home via the SDK's `CommandRelayListener`. Phase 4 of the discovery
 * redesign — see C:/tmp/prompt-discovery-redesign.md.
 */

import { useState, useCallback, useEffect, useRef } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// ============================================================================
// Types
// ============================================================================

export interface DiscoveredApp {
  appId: string;
  appName: string;
  appType: "web" | "desktop" | "mobile" | "other" | string;
  framework?: string;
  url: string;
  port: number;
  /** Path under the origin where UI Bridge routes are mounted (e.g. "/ui-bridge"). */
  basePath: string;
  version?: string;
  capabilities: string[];
  elementCount?: number;
  componentCount?: number;
  discoveredAt: number;
  /**
   * Runner-assigned transport kind. Present on entries returned by
   * `/ui-bridge/apps/registered`; absent from scan-only results. `"websocket"`
   * indicates a wrapper app registered via `/ui-bridge/ws` — such entries have
   * no reachable `url`/`port` for HTTP proxying.
   */
  transport?: "http" | "websocket";
}

export interface MobileDevice {
  deviceId: string;
  deviceType: "device" | "emulator";
  model?: string;
  status: "online" | "offline" | "unauthorized";
  uiBridge?: DiscoveredApp;
}

export interface DiscoveryResult {
  web: DiscoveredApp[];
  desktop: DiscoveredApp[];
  mobile: MobileDevice[];
  scannedAt: number;
  durationMs: number;
}

export interface ForwardDeviceResult {
  localPort: number;
  deviceId: string;
  remotePort: number;
}

/**
 * Payload accepted by the runner's POST /ui-bridge/apps/register endpoint.
 * Mirrors `RegisterAppRequest` in `mcp/app_discovery.rs`. Either `baseUrl` or
 * (`url` + `port`) is required; the runner derives the canonical origin from
 * whichever is supplied.
 */
export interface RegisterAppPayload {
  appId: string;
  appName: string;
  appType: string;
  framework?: string;
  transport?: string;
  /** Absolute URL to the app's UI Bridge base, e.g. "http://127.0.0.1:9875/supervisor-bridge". */
  baseUrl?: string;
  /** Legacy — falls back to this when `baseUrl` is absent. */
  url?: string;
  port?: number;
  basePath?: string;
  capabilities?: string[];
  version?: string;
  origin?: string;
}

export interface UseAppDiscoveryReturn {
  webApps: DiscoveredApp[];
  desktopApps: DiscoveredApp[];
  mobileDevices: MobileDevice[];
  registeredApps: DiscoveredApp[];
  isScanning: boolean;
  isScanningWeb: boolean;
  isScanningDesktop: boolean;
  isScanningMobile: boolean;
  lastScanAt: number | null;
  error: string | null;

  scanAll: () => Promise<void>;
  scanWeb: () => Promise<void>;
  scanDesktop: () => Promise<void>;
  scanMobile: () => Promise<void>;
  forwardDevice: (deviceId: string, remotePort?: number) => Promise<ForwardDeviceResult | null>;

  /** Manually refresh the registered-apps list. Auto-polled every 5s. */
  refreshRegistered: () => Promise<void>;
  /** Stop the background poll for registered apps. */
  stopRegisteredPolling: () => void;
  /** Start the background poll for registered apps (idempotent). */
  startRegisteredPolling: () => void;
  /** POST a registration payload to /ui-bridge/apps/register. */
  registerApp: (payload: RegisterAppPayload) => Promise<DiscoveredApp | null>;
  /** DELETE an entry from /ui-bridge/apps/register/:appId. */
  deregisterApp: (appId: string) => Promise<boolean>;
}

// ============================================================================
// Constants
// ============================================================================

// ============================================================================
// Hook
// ============================================================================

const REGISTERED_POLL_INTERVAL_MS = 5_000;

export function useAppDiscovery(): UseAppDiscoveryReturn {
  const [webApps, setWebApps] = useState<DiscoveredApp[]>([]);
  const [desktopApps, setDesktopApps] = useState<DiscoveredApp[]>([]);
  const [mobileDevices, setMobileDevices] = useState<MobileDevice[]>([]);
  const [registeredApps, setRegisteredApps] = useState<DiscoveredApp[]>([]);
  const [isScanning, setIsScanning] = useState(false);
  const [isScanningWeb, setIsScanningWeb] = useState(false);
  const [isScanningDesktop, setIsScanningDesktop] = useState(false);
  const [isScanningMobile, setIsScanningMobile] = useState(false);
  const [lastScanAt, setLastScanAt] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const abortRef = useRef<AbortController | null>(null);
  const scanningDesktopRef = useRef(false);
  const scanningMobileRef = useRef(false);
  const scanningWebRef = useRef(false);
  const registeredPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const registeredInflightRef = useRef(false);

  const fetchJson = useCallback(async <T>(url: string, signal?: AbortSignal): Promise<T> => {
    const response = await tracedFetch(url, {
      method: url.includes("/scan") && !url.includes("/scan/") ? "POST" : "GET",
      headers: { "Content-Type": "application/json" },
      signal,
    });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}: ${response.statusText}`);
    }
    const json = await response.json();
    if (!json.success) {
      throw new Error(json.error || "Unknown error");
    }
    return json.data;
  }, []);

  const scanAll = useCallback(async () => {
    setIsScanning(true);
    setError(null);
    abortRef.current?.abort();
    abortRef.current = new AbortController();

    try {
      const result = await fetchJson<DiscoveryResult>(
        `${getApiBase()}/ui-bridge/apps/scan`,
        abortRef.current.signal,
      );
      setWebApps(result.web);
      setDesktopApps(result.desktop);
      setMobileDevices(result.mobile);
      setLastScanAt(result.scannedAt);
    } catch (err) {
      if ((err as Error).name !== "AbortError") {
        setError((err as Error).message);
      }
    } finally {
      setIsScanning(false);
    }
  }, [fetchJson]);

  const scanWeb = useCallback(async () => {
    if (scanningWebRef.current) return; // Already scanning
    scanningWebRef.current = true;
    setIsScanningWeb(true);
    setError(null);
    try {
      const apps = await fetchJson<DiscoveredApp[]>(`${getApiBase()}/ui-bridge/apps/scan/web`);
      setWebApps(apps);
      setLastScanAt(Date.now());
    } catch (err) {
      setError((err as Error).message);
    } finally {
      scanningWebRef.current = false;
      setIsScanningWeb(false);
    }
  }, [fetchJson]);

  const scanDesktop = useCallback(async () => {
    if (scanningDesktopRef.current) return; // Already scanning
    scanningDesktopRef.current = true;
    setIsScanningDesktop(true);
    setError(null);
    try {
      const apps = await fetchJson<DiscoveredApp[]>(`${getApiBase()}/ui-bridge/apps/scan/desktop`);
      setDesktopApps(apps);
      setLastScanAt(Date.now());
    } catch (err) {
      setError((err as Error).message);
    } finally {
      scanningDesktopRef.current = false;
      setIsScanningDesktop(false);
    }
  }, [fetchJson]);

  const scanMobile = useCallback(async () => {
    if (scanningMobileRef.current) return; // Already scanning
    scanningMobileRef.current = true;
    setIsScanningMobile(true);
    setError(null);
    try {
      const devices = await fetchJson<MobileDevice[]>(`${getApiBase()}/ui-bridge/apps/scan/mobile`);
      setMobileDevices(devices);
      setLastScanAt(Date.now());
    } catch (err) {
      setError((err as Error).message);
    } finally {
      scanningMobileRef.current = false;
      setIsScanningMobile(false);
    }
  }, [fetchJson]);

  const forwardDevice = useCallback(
    async (deviceId: string, remotePort: number = 9876): Promise<ForwardDeviceResult | null> => {
      try {
        const response = await tracedFetch(`${getApiBase()}/ui-bridge/apps/forward-device`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ deviceId, remotePort }),
        });
        if (!response.ok) {
          throw new Error(`HTTP ${response.status}: ${response.statusText}`);
        }
        const json = await response.json();
        if (!json.success) {
          throw new Error(json.error || "Failed to forward device port");
        }
        return json.data as ForwardDeviceResult;
      } catch (err) {
        setError((err as Error).message);
        return null;
      }
    },
    [],
  );

  // --------------------------------------------------------------------------
  // Phone-home registry (Phase 4 of discovery redesign)
  // --------------------------------------------------------------------------

  const refreshRegistered = useCallback(async () => {
    // Coalesce overlapping polls so a slow runner doesn't stack up N requests.
    if (registeredInflightRef.current) return;
    registeredInflightRef.current = true;
    try {
      const response = await tracedFetch(`${getApiBase()}/ui-bridge/apps/registered`, {
        method: "GET",
        headers: { "Content-Type": "application/json" },
      });
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
      }
      const json = await response.json();
      if (!json.success) {
        throw new Error(json.error || "Failed to fetch registered apps");
      }
      const apps = (json.data ?? []) as DiscoveredApp[];
      setRegisteredApps(apps);
    } catch (err) {
      // Don't blow away the previous list on a transient poll failure — just
      // surface the error for the UI. A later successful poll will replace it.
      setError((err as Error).message);
    } finally {
      registeredInflightRef.current = false;
    }
  }, []);

  const registerApp = useCallback(
    async (payload: RegisterAppPayload): Promise<DiscoveredApp | null> => {
      try {
        const response = await tracedFetch(`${getApiBase()}/ui-bridge/apps/register`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
        if (!response.ok) {
          throw new Error(`HTTP ${response.status}: ${response.statusText}`);
        }
        const json = await response.json();
        if (!json.success) {
          throw new Error(json.error || "Failed to register app");
        }
        // Refresh eagerly so callers see the new entry without waiting for
        // the next poll tick.
        await refreshRegistered();
        return json.data as DiscoveredApp;
      } catch (err) {
        setError((err as Error).message);
        return null;
      }
    },
    [refreshRegistered],
  );

  const deregisterApp = useCallback(
    async (appId: string): Promise<boolean> => {
      try {
        const response = await tracedFetch(
          `${getApiBase()}/ui-bridge/apps/register/${encodeURIComponent(appId)}`,
          {
            method: "DELETE",
            headers: { "Content-Type": "application/json" },
          },
        );
        if (!response.ok) {
          throw new Error(`HTTP ${response.status}: ${response.statusText}`);
        }
        const json = await response.json();
        if (!json.success) {
          throw new Error(json.error || "Failed to deregister app");
        }
        await refreshRegistered();
        return Boolean(json.data);
      } catch (err) {
        setError((err as Error).message);
        return false;
      }
    },
    [refreshRegistered],
  );

  const stopRegisteredPolling = useCallback(() => {
    if (registeredPollRef.current !== null) {
      clearInterval(registeredPollRef.current);
      registeredPollRef.current = null;
    }
  }, []);

  const startRegisteredPolling = useCallback(() => {
    if (registeredPollRef.current !== null) return; // Idempotent.
    // Kick off immediately so callers don't wait a full interval for the
    // first result.
    void refreshRegistered();
    registeredPollRef.current = setInterval(() => {
      void refreshRegistered();
    }, REGISTERED_POLL_INTERVAL_MS);
  }, [refreshRegistered]);

  // Start polling on mount; stop on unmount.
  useEffect(() => {
    startRegisteredPolling();
    return () => {
      stopRegisteredPolling();
    };
  }, [startRegisteredPolling, stopRegisteredPolling]);

  return {
    webApps,
    desktopApps,
    mobileDevices,
    registeredApps,
    isScanning,
    isScanningWeb,
    isScanningDesktop,
    isScanningMobile,
    lastScanAt,
    error,
    scanAll,
    scanWeb,
    scanDesktop,
    scanMobile,
    forwardDevice,
    refreshRegistered,
    stopRegisteredPolling,
    startRegisteredPolling,
    registerApp,
    deregisterApp,
  };
}
