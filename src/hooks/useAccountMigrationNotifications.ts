/**
 * useAccountMigrationNotifications — toasts for automatic session account
 * migration (backend `terminal::account_migration`).
 *
 * The runner backend emits:
 * - `session-account-migrated` when a token-exhausted Claude session was
 *   respawned under a fresh account (the new pane appears via the normal
 *   `terminal-created` flow — this toast explains WHY a pane just cycled).
 * - `session-account-migration-skipped` when exhaustion was confirmed but no
 *   migration happened (no usable target / cap reached / mechanics failed) —
 *   surfaced as an error so the operator knows the session is stranded.
 *
 * Mount once at App level, next to `useErrorNotifications`.
 */

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { ShowToastFn } from "./useToast";

interface MigratedPayload {
  claudeSessionId?: string;
  fromConfigDir?: string;
  toConfigDir?: string;
}

interface SkippedPayload {
  claudeSessionId?: string;
  fromConfigDir?: string;
  reason?: string;
}

/** Last path segment of a config dir (`C:\claude\.claude-gmail` → `.claude-gmail`). */
function accountLabel(dir: string | undefined): string {
  if (!dir) return "unknown";
  const parts = dir.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? dir;
}

export function useAccountMigrationNotifications(showToast: ShowToastFn) {
  useEffect(() => {
    const unlistenMigrated = listen<MigratedPayload>("session-account-migrated", (event) => {
      const p = event.payload ?? {};
      showToast(
        `Session ${(p.claudeSessionId ?? "").slice(0, 8)} migrated: ${accountLabel(
          p.fromConfigDir,
        )} ran out of tokens → resumed on ${accountLabel(p.toConfigDir)}`,
        "success",
      );
    });
    const unlistenSkipped = listen<SkippedPayload>(
      "session-account-migration-skipped",
      (event) => {
        const p = event.payload ?? {};
        showToast(
          `Session ${(p.claudeSessionId ?? "").slice(0, 8)} is out of tokens on ${accountLabel(
            p.fromConfigDir,
          )} but was NOT migrated: ${p.reason ?? "unknown reason"}`,
          "error",
        );
      },
    );
    return () => {
      unlistenMigrated.then((fn) => fn());
      unlistenSkipped.then((fn) => fn());
    };
  }, [showToast]);
}
