/**
 * useAppDiscovery Hook
 *
 * Discovers UI Bridge-enabled applications across web, desktop, and mobile
 * by calling the runner's /ui-bridge/apps/scan endpoints.
 */

import { useState, useCallback, useRef } from "react";

// ============================================================================
// Types
// ============================================================================

export interface DiscoveredApp {
  appId: string;
  appName: string;
  appType: "web" | "desktop" | "mobile" | "other";
  framework?: string;
  url: string;
  port: number;
  version?: string;
  capabilities: string[];
  elementCount?: number;
  componentCount?: number;
  discoveredAt: number;
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

export interface UseAppDiscoveryReturn {
  webApps: DiscoveredApp[];
  desktopApps: DiscoveredApp[];
  mobileDevices: MobileDevice[];
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
}

// ============================================================================
// Constants
// ============================================================================

const RUNNER_API_BASE = "http://localhost:9876";

// ============================================================================
// Hook
// ============================================================================

export function useAppDiscovery(): UseAppDiscoveryReturn {
  const [webApps, setWebApps] = useState<DiscoveredApp[]>([]);
  const [desktopApps, setDesktopApps] = useState<DiscoveredApp[]>([]);
  const [mobileDevices, setMobileDevices] = useState<MobileDevice[]>([]);
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

  const fetchJson = useCallback(async <T>(url: string, signal?: AbortSignal): Promise<T> => {
    const response = await fetch(url, {
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
        `${RUNNER_API_BASE}/ui-bridge/apps/scan`,
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
      const apps = await fetchJson<DiscoveredApp[]>(`${RUNNER_API_BASE}/ui-bridge/apps/scan/web`);
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
      const apps = await fetchJson<DiscoveredApp[]>(
        `${RUNNER_API_BASE}/ui-bridge/apps/scan/desktop`,
      );
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
      const devices = await fetchJson<MobileDevice[]>(
        `${RUNNER_API_BASE}/ui-bridge/apps/scan/mobile`,
      );
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
        const response = await fetch(`${RUNNER_API_BASE}/ui-bridge/apps/forward-device`, {
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

  return {
    webApps,
    desktopApps,
    mobileDevices,
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
  };
}
