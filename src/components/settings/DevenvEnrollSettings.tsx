/**
 * DevenvEnrollSettings.tsx
 *
 * Phase 2 — in-app devenv enrollment (no terminal). Paste the one-time
 * enrollment code minted by the web dashboard, click Enroll, and this machine
 * binds to the environment and starts reporting its (secret-free) config to the
 * drift view. Wraps the `devenv_enroll` / `devenv_enroll_status` Tauri commands,
 * which share the `env_agent::enroll` core with the `qontinui_profile env enroll`
 * CLI — so the terminal and the UI enroll through one code path.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { KeyRound, Check, Loader2, Server, Terminal } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface EnrollStatus {
  enrolled: boolean;
  machine_id: string | null;
  environment_id: string | null;
  backend_url: string | null;
  enrolled_at: string | null;
  has_key: boolean;
  /** Whether `qontinui-runner env …` resolves in a terminal (install dir on PATH). */
  cli_on_path: boolean;
}

interface EnrollOutcome {
  machine_id: string;
  environment_id: string;
  backend_url: string;
}

interface DevenvEnrollSettingsProps {
  onLog: LogFunction;
}

export function DevenvEnrollSettings({ onLog }: DevenvEnrollSettingsProps) {
  const [code, setCode] = useState("");
  const [backend, setBackend] = useState("");
  const [status, setStatus] = useState<EnrollStatus | null>(null);
  const [enrolling, setEnrolling] = useState(false);
  const [installingPath, setInstallingPath] = useState(false);

  const refreshStatus = useCallback(async () => {
    try {
      const res =
        await invoke<TauriResult<EnrollStatus>>("devenv_enroll_status");
      if (res.success && res.data) setStatus(res.data);
    } catch (e) {
      onLog("error", `Failed to read enrollment status: ${e}`);
    }
  }, [onLog]);

  useEffect(() => {
    void refreshStatus();
  }, [refreshStatus]);

  const handleEnroll = useCallback(async () => {
    const trimmed = code.trim();
    if (!trimmed) {
      onLog("warning", "Enter the enrollment code from the dashboard first.");
      return;
    }
    setEnrolling(true);
    try {
      const res = await invoke<TauriResult<EnrollOutcome>>("devenv_enroll", {
        code: trimmed,
        backend: backend.trim() || null,
      });
      if (res.success) {
        onLog("success", res.message ?? "Enrolled successfully.");
        setCode("");
        await refreshStatus();
      } else {
        onLog("error", res.message ?? "Enrollment failed.");
      }
    } catch (e) {
      onLog("error", `Enrollment failed: ${e}`);
    } finally {
      setEnrolling(false);
    }
  }, [code, backend, onLog, refreshStatus]);

  const handleInstallPath = useCallback(async () => {
    setInstallingPath(true);
    try {
      const res = await invoke<TauriResult<{ message: string }>>(
        "devenv_install_cli_path",
      );
      if (res.success) {
        onLog("success", res.message ?? "Added to PATH.");
        await refreshStatus();
      } else {
        onLog("error", res.message ?? "Could not update PATH.");
      }
    } catch (e) {
      onLog("error", `PATH setup failed: ${e}`);
    } finally {
      setInstallingPath(false);
    }
  }, [onLog, refreshStatus]);

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Devenv Enrollment"
        description="Bind this machine to a dev-environment so it reports its config to the drift view. Paste the one-time code from the dashboard — no terminal, no build."
        icon={<Server className="size-5" />}
      />

      {/* Current enrollment status */}
      <div className="rounded-md border border-border bg-muted/40 px-4 py-3 text-sm">
        {status?.enrolled ? (
          <div className="space-y-1">
            <div className="flex items-center gap-2 font-medium text-green-600 dark:text-green-500">
              <Check className="size-4" /> Enrolled
            </div>
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
              <dt>Environment</dt>
              <dd className="font-mono">{status.environment_id}</dd>
              <dt>Machine</dt>
              <dd className="font-mono">{status.machine_id}</dd>
              <dt>Backend</dt>
              <dd className="font-mono break-all">{status.backend_url}</dd>
              {status.enrolled_at && (
                <>
                  <dt>Enrolled</dt>
                  <dd>{new Date(status.enrolled_at).toLocaleString()}</dd>
                </>
              )}
              <dt>Machine key</dt>
              <dd>{status.has_key ? "present" : "missing — re-enroll"}</dd>
            </dl>
          </div>
        ) : (
          <p className="text-muted-foreground">
            This machine is not enrolled yet. Create a machine in the dashboard,
            copy its one-time code, and paste it below.
          </p>
        )}
      </div>

      {/* Enroll form */}
      <div className="space-y-3">
        <label className="block space-y-1">
          <span className="text-xs font-medium">Enrollment code</span>
          <input
            type="text"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="paste the one-time code"
            className="w-full rounded-md border border-border bg-background px-3 py-2 font-mono text-sm"
            disabled={enrolling}
          />
        </label>
        <label className="block space-y-1">
          <span className="text-xs font-medium text-muted-foreground">
            Backend URL (optional — resolves from your active profile)
          </span>
          <input
            type="text"
            value={backend}
            onChange={(e) => setBackend(e.target.value)}
            placeholder="https://qontinui.io"
            className="w-full rounded-md border border-border bg-background px-3 py-2 font-mono text-sm"
            disabled={enrolling}
          />
        </label>
        <button
          type="button"
          onClick={handleEnroll}
          disabled={enrolling || !code.trim()}
          className="inline-flex items-center gap-2 rounded-md border border-border bg-muted px-4 py-2 text-sm font-medium hover:bg-muted/70 disabled:opacity-50"
        >
          {enrolling ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <KeyRound className="size-4" />
          )}
          {enrolling
            ? "Enrolling…"
            : status?.enrolled
              ? "Re-enroll"
              : "Enroll this machine"}
        </button>
      </div>

      {/* Terminal command (PATH) — makes the dashboard's copy-paste
          `qontinui-runner env enroll …` command resolve in a terminal. */}
      <div className="space-y-2 border-t border-border pt-4">
        <div className="flex items-center gap-2 text-xs font-medium">
          <Terminal className="size-4" />
          Terminal command
        </div>
        {status?.cli_on_path ? (
          <p className="flex items-center gap-2 text-xs text-muted-foreground">
            <Check className="size-4 text-green-600 dark:text-green-500" />
            <code className="font-mono">qontinui-runner</code> is on your PATH —
            the dashboard&apos;s copy-paste command works in a new terminal.
          </p>
        ) : (
          <>
            <p className="text-xs text-muted-foreground">
              Add the runner to your PATH so the dashboard&apos;s{" "}
              <code className="font-mono">qontinui-runner env enroll …</code>{" "}
              command resolves in a terminal (Windows).
            </p>
            <button
              type="button"
              onClick={handleInstallPath}
              disabled={installingPath}
              className="inline-flex items-center gap-2 rounded-md border border-border bg-muted px-4 py-2 text-sm font-medium hover:bg-muted/70 disabled:opacity-50"
            >
              {installingPath ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <Terminal className="size-4" />
              )}
              {installingPath ? "Adding…" : "Add to PATH"}
            </button>
          </>
        )}
      </div>
    </div>
  );
}
