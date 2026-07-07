import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";

/**
 * Launch-time auto-update check.
 *
 * On startup this asks the backend whether a newer signed release is available
 * (`check_for_updates` in `system_ops.rs`, which verifies the update signature
 * against the `pubkey` embedded in `tauri.conf.json`). If one is, it prompts the
 * user once and, on consent, downloads + installs it (`install_update`) — the
 * app then restarts into the new version.
 *
 * Renders nothing. Intentionally fail-open: the backend commands are release-only
 * (`#[cfg(not(debug_assertions))]`), so in dev the invoke rejects and is
 * swallowed; any check/download error is caught and must never disrupt launch.
 * The check is deferred a few seconds so it never competes with first paint or
 * auth bootstrap, and runs at most once per launch.
 */
export function AutoUpdateChecker(): null {
  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(() => {
      void (async () => {
        try {
          const result = (await invoke("check_for_updates")) as {
            success: boolean;
            data?: { available?: boolean; version?: string };
          };
          if (cancelled || !result.success || !result.data?.available) return;

          const version = result.data.version
            ? `Qontinui Runner v${result.data.version}`
            : "A new version of Qontinui Runner";
          const accepted = await ask(
            `${version} is available.\n\nInstall it now? The app will restart to apply the update.`,
            {
              title: "Update available",
              kind: "info",
              okLabel: "Install & restart",
              cancelLabel: "Later",
            },
          );
          if (cancelled || !accepted) return;

          // Downloads, installs, and relaunches into the new version.
          await invoke("install_update");
        } catch {
          // Fail-open: a failed or blocked update check must never block launch.
        }
      })();
    }, 4000);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, []);

  return null;
}
