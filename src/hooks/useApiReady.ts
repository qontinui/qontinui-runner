/**
 * useApiReady Hook
 *
 * Tracks whether the runner's HTTP API server (port 9876) is ready.
 * Uses three mechanisms to ensure reliable detection:
 * 1. Tauri event listener for `api-ready` (fastest path on initial startup)
 * 2. IPC re-check after listener setup (closes the race window)
 * 3. Polling fallback every 500ms (guarantees detection even if event is missed)
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export function useApiReady(): boolean {
  const [isReady, setIsReady] = useState(false);

  useEffect(() => {
    if (isReady) return;

    let cancelled = false;

    // 1. Listen for the api-ready event (fastest path on fresh startup)
    let unlisten: (() => void) | null = null;

    listen<number>("api-ready", (event) => {
      if (!cancelled) {
        console.log("[API] API server ready (event, port:", event.payload, ")");
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
              console.log("[API] API server already ready (IPC post-listen check)");
              setIsReady(true);
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
            console.log("[API] API server ready (poll)");
            setIsReady(true);
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
