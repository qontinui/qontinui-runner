/**
 * useCloudRelayAutoConnect.ts
 *
 * Startup hook that automatically connects to the cloud relay when:
 * 1. Cloud relay is enabled in settings
 * 2. auto_connect is turned on
 * 3. The relay is not already running
 *
 * This runs at the App level so it fires on startup, not just when
 * the user visits the Settings page.
 */

import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";

const log = createLogger("useCloudRelayAutoConnect");

interface CloudRelaySettingsData {
  enabled: boolean;
  backend_url: string;
  auto_connect: boolean;
}

interface CloudRelayStatus {
  enabled: boolean;
  backend_url: string;
  auto_connect: boolean;
  is_running: boolean;
}

export function useCloudRelayAutoConnect() {
  const attemptedRef = useRef(false);

  useEffect(() => {
    if (attemptedRef.current) return;
    attemptedRef.current = true;

    const autoConnect = async () => {
      try {
        const settings = await invoke<CloudRelaySettingsData>("get_cloud_relay_settings");

        if (!settings?.enabled || !settings?.auto_connect) {
          return;
        }

        const status = await invoke<CloudRelayStatus>("get_cloud_relay_status");

        if (status?.is_running) {
          log.debug("Already running, skipping auto-connect");
          return;
        }

        log.debug("Auto-connecting cloud relay on startup...");
        await invoke<string>("start_cloud_relay");
        log.debug("Cloud relay auto-connect initiated");
      } catch (err) {
        console.error("[CLOUD_RELAY_AUTO] Auto-connect failed:", err);
      }
    };

    autoConnect();
  }, []);
}
