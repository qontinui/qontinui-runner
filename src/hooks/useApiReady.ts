/**
 * useApiReady Hook
 *
 * Tracks whether the runner's HTTP API server is ready and configures
 * the centralized API base URL with the actual bound port.
 *
 * Uses three mechanisms to ensure reliable detection:
 * 1. Tauri event listener for `api-ready` (fastest path on initial startup)
 * 2. IPC re-check after listener setup (closes the race window)
 * 3. Polling fallback every 500ms (guarantees detection even if event is missed)
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { setApiPort } from "@/lib/runner-api";
import { createLogger } from "@/lib/logger";

const log = createLogger("ApiReady");

/** Fetch the actual port from the backend and configure the API base URL. */
async function syncApiPort(): Promise<void> {
  try {
    const port = await invoke<number>("get_api_port");
    if (port && port > 0) {
      setApiPort(port);
      log.debug("API port set to", port);
    }
  } catch {
    // Backend not ready yet — will retry
  }
}

export function useApiReady(): boolean {
  const [isReady, setIsReady] = useState(false);

  useEffect(() => {
    if (isReady) return;

    let cancelled = false;

    const markReady = () => {
      if (!cancelled) {
        syncApiPort().finally(() => {
          if (!cancelled) setIsReady(true);
        });
      }
    };

    // 1. Listen for the api-ready event (fastest path on fresh startup)
    let unlisten: (() => void) | null = null;

    listen<number>("api-ready", (event) => {
      if (!cancelled) {
        log.debug("API server ready (event, port:", event.payload, ")");
        if (event.payload && event.payload > 0) {
          setApiPort(event.payload);
        }
        setIsReady(true);
      }
    }).then((fn) => {
      unlisten = fn;

      // 2. Re-check via IPC after listener is established to close the
      // race window where the event fired between mount and listener setup
      if (!cancelled) {
        invoke<boolean>("is_api_ready")
          .then((ready) => {
            if (ready && !cancelled) {
              log.debug("API server already ready (IPC post-listen check)");
              markReady();
            }
          })
          .catch(() => {});
      }
    });

    // 3. Poll as a safety net — guarantees we never stay stuck
    const interval = setInterval(() => {
      invoke<boolean>("is_api_ready")
        .then((ready) => {
          if (ready && !cancelled) {
            log.debug("API server ready (poll)");
            markReady();
          }
        })
        .catch(() => {});
    }, 500);

    return () => {
      cancelled = true;
      clearInterval(interval);
      if (unlisten) unlisten();
    };
  }, [isReady]);

  return isReady;
}
