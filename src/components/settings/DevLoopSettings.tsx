/**
 * DevLoopSettings.tsx
 *
 * "Test My Change" — the canonical developer dev-loop for the runner.
 *
 * Build your current change (a git worktree, a git ref, or an explicit
 * worktree path) into an ISOLATED temp runner via the supervisor's
 * `POST /runners/spawn-test`. The primary runner is NEVER restarted — the
 * rule is "never restart the primary to test; use a temp runner".
 *
 * Provenance is mutually exclusive: the request carries EITHER
 * `{ worktree_path }` OR `{ git_ref }` — never both (the supervisor returns
 * 400 `provenance_conflict` if both are sent).
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { FlaskConical, RefreshCw, Square, ExternalLink, Loader2 } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";

// ---------------------------------------------------------------------------
// Types mirroring the Rust shapes from `commands::instances` and the
// supervisor's spawn-test / runners contract.
// ---------------------------------------------------------------------------

/** Mirrors the Rust `DevLoopWorktree` from `commands::instances`. */
interface DevLoopWorktree {
  path: string;
  branch: string | null;
  head: string | null;
  is_main: boolean;
  is_detached: boolean;
  buildable: boolean;
}

/** A single runner row from the supervisor's `GET /runners`. */
interface SupervisorRunner {
  id: string;
  name: string;
  port: number;
  is_primary: boolean;
  running: boolean;
  pid: number | null;
  api_responding: boolean;
}

/** 200 response shape from `POST /runners/spawn-test`. */
interface SpawnTestResult {
  id: string;
  port: number;
  api_url?: string;
  ui_bridge_url?: string;
  status: "healthy" | "timeout" | "started" | string;
  git_sha?: string;
  source?: "live_tree" | "worktree" | "worktree_path" | string;
  git_ref_resolved_sha?: string;
  git_ref_resolved_short?: string;
  message?: string;
}

type SourceMode = "worktree" | "git_ref" | "custom_path";

interface DevLoopSettingsProps {
  onLog: LogFunction;
}

const DEFAULT_SUPERVISOR_PORT = "9875";

function supervisorBase(port: string): string {
  const p = port.trim() || DEFAULT_SUPERVISOR_PORT;
  return `http://localhost:${p}`;
}

function shortPath(p: string): string {
  // Show the trailing two path segments so long absolute paths stay readable.
  const norm = p.replace(/\\/g, "/").replace(/\/+$/, "");
  const parts = norm.split("/");
  if (parts.length <= 2) return p;
  return `…/${parts.slice(-2).join("/")}`;
}

export function DevLoopSettings({ onLog }: DevLoopSettingsProps) {
  const [supervisorPort, setSupervisorPort] = useState(DEFAULT_SUPERVISOR_PORT);

  // Source picker (provenance — mutually exclusive)
  const [sourceMode, setSourceMode] = useState<SourceMode>("worktree");
  const [worktrees, setWorktrees] = useState<DevLoopWorktree[]>([]);
  const [selectedWorktreePath, setSelectedWorktreePath] = useState<string>("");
  const [gitRef, setGitRef] = useState("");
  const [customPath, setCustomPath] = useState("");

  // Discovery state
  const [discovering, setDiscovering] = useState(false);
  const [discoveryError, setDiscoveryError] = useState<string | null>(null);

  // Build state
  const [building, setBuilding] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [result, setResult] = useState<SpawnTestResult | null>(null);
  const [buildError, setBuildError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const elapsedTimerRef = useRef<number | null>(null);

  // Running temp runners list
  const [tempRunners, setTempRunners] = useState<SupervisorRunner[]>([]);

  // -------------------------------------------------------------------------
  // Worktree discovery: ask the supervisor for its build root, then ask the
  // runner backend (via the Tauri command) to enumerate worktrees.
  // -------------------------------------------------------------------------
  const discoverWorktrees = useCallback(async () => {
    setDiscovering(true);
    setDiscoveryError(null);
    try {
      const resp = await fetch(`${supervisorBase(supervisorPort)}/health`);
      if (!resp.ok) {
        throw new Error(`supervisor /health: HTTP ${resp.status}`);
      }
      const health = (await resp.json()) as {
        supervisor?: { project_dir?: string };
        project_dir?: string;
      };
      // The supervisor reports its build root under `supervisor.project_dir`
      // (e.g. `.../qontinui-runner/src-tauri`); older shapes put it at the top
      // level, so accept either.
      const projectDir = health.supervisor?.project_dir ?? health.project_dir;
      if (!projectDir) {
        throw new Error("supervisor /health did not report project_dir");
      }
      const list = await invoke<DevLoopWorktree[]>("list_repo_worktrees", {
        repoRoot: projectDir,
      });
      setWorktrees(list);

      // Default-select the first non-main buildable worktree, if any.
      const firstBuildable = list.find((w) => !w.is_main && w.buildable);
      if (firstBuildable) {
        setSelectedWorktreePath((prev) => prev || firstBuildable.path);
      }
      if (list.length === 0) {
        setDiscoveryError("No worktrees found");
      }
    } catch (err) {
      setWorktrees([]);
      setDiscoveryError(String(err));
    } finally {
      setDiscovering(false);
    }
  }, [supervisorPort]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async discovery updates state in a callback, not synchronously
    void discoverWorktrees();
  }, [discoverWorktrees]);

  // -------------------------------------------------------------------------
  // Poll the supervisor for temp runners every 5s.
  // -------------------------------------------------------------------------
  const refreshTempRunners = useCallback(async () => {
    try {
      const resp = await fetch(`${supervisorBase(supervisorPort)}/runners`);
      if (!resp.ok) return;
      const runners = (await resp.json()) as SupervisorRunner[];
      setTempRunners(runners.filter((r) => r.id.startsWith("test-")));
    } catch {
      // Non-fatal — supervisor may not be up.
    }
  }, [supervisorPort]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async fetch updates state in a callback, not synchronously
    void refreshTempRunners();
    const interval = window.setInterval(() => {
      void refreshTempRunners();
    }, 5000);
    return () => {
      window.clearInterval(interval);
    };
  }, [refreshTempRunners]);

  // Elapsed-seconds ticker while building.
  useEffect(() => {
    if (!building) {
      if (elapsedTimerRef.current != null) {
        window.clearInterval(elapsedTimerRef.current);
        elapsedTimerRef.current = null;
      }
      return;
    }
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reset the elapsed ticker when a build starts
    setElapsed(0);
    elapsedTimerRef.current = window.setInterval(() => {
      setElapsed((e) => e + 1);
    }, 1000);
    return () => {
      if (elapsedTimerRef.current != null) {
        window.clearInterval(elapsedTimerRef.current);
        elapsedTimerRef.current = null;
      }
    };
  }, [building]);

  // Abort any in-flight build on unmount.
  useEffect(() => {
    return () => {
      abortRef.current?.abort();
    };
  }, []);

  // -------------------------------------------------------------------------
  // Resolve the current provenance to EITHER { worktree_path } OR { git_ref }.
  // Returns null when the current selection isn't valid.
  // -------------------------------------------------------------------------
  const resolveProvenance = useCallback((): {
    worktree_path?: string;
    git_ref?: string;
  } | null => {
    if (sourceMode === "worktree") {
      const path = selectedWorktreePath.trim();
      if (!path) return null;
      const wt = worktrees.find((w) => w.path === path);
      // The live tree can't be a worktree_path — block it here.
      if (wt?.is_main) return null;
      return { worktree_path: path };
    }
    if (sourceMode === "git_ref") {
      const ref = gitRef.trim();
      if (!ref) return null;
      return { git_ref: ref };
    }
    // custom_path
    const path = customPath.trim();
    if (!path) return null;
    return { worktree_path: path };
  }, [sourceMode, selectedWorktreePath, worktrees, gitRef, customPath]);

  const provenance = resolveProvenance();
  const canBuild = !building && provenance !== null;
  const expectedNonLiveSource = provenance !== null; // we never intentionally request the live tree here

  // -------------------------------------------------------------------------
  // Build & launch a test runner.
  // -------------------------------------------------------------------------
  const handleBuild = async () => {
    const prov = resolveProvenance();
    if (!prov) {
      onLog("warning", "Select a worktree, enter a git ref, or enter a worktree path first.");
      return;
    }

    setBuilding(true);
    setResult(null);
    setBuildError(null);

    const controller = new AbortController();
    abortRef.current = controller;

    const body = {
      rebuild: true,
      wait: true,
      wait_timeout_secs: 180,
      frontend_only: false,
      requester_id: "dev-loop-ui",
      ...prov,
    };

    try {
      const resp = await fetch(`${supervisorBase(supervisorPort)}/runners/spawn-test`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
        signal: controller.signal,
      });

      const text = await resp.text();
      let parsed: unknown = null;
      try {
        parsed = text ? JSON.parse(text) : null;
      } catch {
        parsed = null;
      }

      if (!resp.ok) {
        const errObj = (parsed ?? {}) as { error?: string; recent_logs?: string };
        const detail =
          errObj.error ??
          (typeof parsed === "object" && parsed !== null ? JSON.stringify(parsed) : text);
        const logs = errObj.recent_logs ? `\n${errObj.recent_logs}` : "";
        const combined = `HTTP ${resp.status}: ${detail}${logs}`.slice(0, 400);
        setBuildError(combined);
        onLog("error", `spawn-test failed: ${combined}`);
        return;
      }

      const data = parsed as SpawnTestResult;
      setResult(data);
      if (data.source === "live_tree") {
        onLog(
          "warning",
          `spawn-test resolved to live_tree (provenance was dropped) — port ${data.port}`,
        );
      } else {
        onLog(
          "success",
          `Test runner up on port ${data.port} (${data.source ?? "?"}, ${data.git_sha ?? "?"})`,
        );
      }
      void refreshTempRunners();
    } catch (err) {
      if (controller.signal.aborted) {
        onLog(
          "info",
          "Cancelled the UI wait — the build continues server-side; check the temp-runner list.",
        );
      } else {
        const msg = String(err).slice(0, 400);
        setBuildError(msg);
        onLog("error", `spawn-test request failed: ${msg}`);
      }
    } finally {
      setBuilding(false);
      abortRef.current = null;
    }
  };

  const handleCancel = () => {
    abortRef.current?.abort();
  };

  const handleOpen = async (port: number) => {
    const url = `http://localhost:${port}/`;
    try {
      await openUrl(url);
    } catch {
      window.open(url, "_blank");
    }
  };

  const handleStop = async (id: string) => {
    try {
      const resp = await fetch(
        `${supervisorBase(supervisorPort)}/runners/${encodeURIComponent(id)}/stop`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ force: false }),
        },
      );
      if (!resp.ok) {
        const body = await resp.text().catch(() => "");
        throw new Error(`HTTP ${resp.status}: ${body.slice(0, 200)}`);
      }
      onLog("success", `Stopped temp runner '${id}'`);
      void refreshTempRunners();
    } catch (err) {
      onLog("error", `Failed to stop '${id}': ${err}`);
    }
  };

  return (
    <div className="space-y-4">
      <SectionHeader
        title="Test My Change"
        description="Build your current change into an isolated temp runner — the primary runner is never restarted."
        icon={<FlaskConical />}
      />

      <div className="flex items-start gap-2 rounded-lg border border-border bg-muted/40 p-2.5 text-xs text-muted-foreground">
        <FlaskConical className="mt-0.5 h-3.5 w-3.5 shrink-0" />
        <span>
          This is the canonical dev-loop:{" "}
          <span className="font-medium text-foreground">
            never restart the primary to test — use a temp runner.
          </span>{" "}
          The supervisor cold-builds the selected source on a fresh port (9877–9899); your primary
          runner keeps running untouched.
        </span>
      </div>

      {/* Supervisor port */}
      <div className="flex items-center gap-2 p-3 rounded-lg border border-border bg-card">
        <label className="text-xs text-muted-foreground w-32 shrink-0" htmlFor="dev-loop-sup-port">
          Supervisor port
        </label>
        <input
          id="dev-loop-sup-port"
          type="text"
          inputMode="numeric"
          value={supervisorPort}
          onChange={(e) => setSupervisorPort(e.target.value)}
          placeholder={DEFAULT_SUPERVISOR_PORT}
          className="w-24 px-2 py-1 text-xs rounded border border-border bg-background"
        />
        <button
          type="button"
          onClick={() => void discoverWorktrees()}
          disabled={discovering}
          className="ml-auto flex items-center gap-1.5 px-2 py-1 text-xs rounded border border-border hover:bg-accent transition-colors disabled:opacity-50"
          title="Re-detect worktrees from the supervisor"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${discovering ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>

      {/* Source picker (provenance — mutually exclusive) */}
      <div className="space-y-2 p-3 rounded-lg border border-border bg-card">
        <div className="text-sm font-semibold">Source to build</div>

        {/* Worktree */}
        <label className="flex items-start gap-2 text-xs">
          <input
            type="radio"
            name="dev-loop-source"
            className="mt-1"
            checked={sourceMode === "worktree"}
            onChange={() => setSourceMode("worktree")}
          />
          <div className="flex-1 min-w-0 space-y-1">
            <span className="font-medium text-foreground">Worktree (detected)</span>
            {sourceMode === "worktree" && (
              <>
                {discovering && (
                  <p className="text-[11px] text-muted-foreground">Detecting worktrees…</p>
                )}
                {!discovering && worktrees.length > 0 && (
                  <select
                    value={selectedWorktreePath}
                    onChange={(e) => setSelectedWorktreePath(e.target.value)}
                    className="w-full px-2 py-1 text-xs rounded border border-border bg-background"
                  >
                    <option value="">Select a worktree…</option>
                    {worktrees.map((w) => {
                      const disabled = w.is_main || !w.buildable;
                      const name = w.branch || (w.is_detached ? "(detached)" : "(no branch)");
                      let suffix = "";
                      if (w.is_main) suffix = " — live tree, use a git ref instead";
                      else if (!w.buildable) suffix = " — not buildable (no src-tauri)";
                      return (
                        <option key={w.path} value={w.path} disabled={disabled}>
                          {name} · {shortPath(w.path)}
                          {suffix}
                        </option>
                      );
                    })}
                  </select>
                )}
                {!discovering && discoveryError && (
                  <p className="text-[11px] text-muted-foreground">
                    Couldn&apos;t auto-detect worktrees — enter a git ref or path manually.
                  </p>
                )}
              </>
            )}
          </div>
        </label>

        {/* Git ref */}
        <label className="flex items-start gap-2 text-xs">
          <input
            type="radio"
            name="dev-loop-source"
            className="mt-1"
            checked={sourceMode === "git_ref"}
            onChange={() => setSourceMode("git_ref")}
          />
          <div className="flex-1 min-w-0 space-y-1">
            <span className="font-medium text-foreground">Git ref</span>
            {sourceMode === "git_ref" && (
              <input
                type="text"
                value={gitRef}
                onChange={(e) => setGitRef(e.target.value)}
                placeholder="branch / tag / SHA (e.g. feat/my-change)"
                className="w-full px-2 py-1 text-xs rounded border border-border bg-background"
              />
            )}
          </div>
        </label>

        {/* Custom worktree path */}
        <label className="flex items-start gap-2 text-xs">
          <input
            type="radio"
            name="dev-loop-source"
            className="mt-1"
            checked={sourceMode === "custom_path"}
            onChange={() => setSourceMode("custom_path")}
          />
          <div className="flex-1 min-w-0 space-y-1">
            <span className="font-medium text-foreground">Custom worktree path</span>
            {sourceMode === "custom_path" && (
              <input
                type="text"
                value={customPath}
                onChange={(e) => setCustomPath(e.target.value)}
                placeholder="absolute path to a worktree (must contain src-tauri/Cargo.toml)"
                className="w-full px-2 py-1 text-xs rounded border border-border bg-background"
              />
            )}
          </div>
        </label>
      </div>

      {/* Build & launch */}
      <div className="space-y-2 p-3 rounded-lg border border-border bg-card">
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void handleBuild()}
            disabled={!canBuild}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            {building ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <FlaskConical className="w-3.5 h-3.5" />
            )}
            Build &amp; launch test runner
          </button>
          {building && (
            <button
              type="button"
              onClick={handleCancel}
              className="px-3 py-1.5 text-xs font-medium rounded border border-border hover:bg-accent transition-colors"
            >
              Cancel
            </button>
          )}
        </div>

        {building && (
          <p className="text-[11px] text-muted-foreground">
            Building… {elapsed}s elapsed. Cold builds take a few minutes. The primary runner is
            unaffected. Cancel detaches the UI from the wait — the build continues server-side.
          </p>
        )}

        {buildError && (
          <pre className="whitespace-pre-wrap break-words text-[11px] text-destructive font-mono p-2 rounded border border-destructive/40 bg-destructive/5">
            {buildError}
          </pre>
        )}

        {result && (
          <div className="p-3 rounded-lg border border-border bg-background/40 space-y-1.5">
            <div className="flex items-center gap-2">
              <span
                className={`w-2.5 h-2.5 rounded-full shrink-0 ${
                  result.status === "healthy"
                    ? "bg-green-500"
                    : result.status === "timeout"
                      ? "bg-yellow-500"
                      : "bg-blue-500"
                }`}
                title={result.status}
              />
              <span className="text-sm font-medium">Port {result.port}</span>
              <span className="text-[11px] text-muted-foreground">{result.status}</span>
            </div>
            <div className="text-[11px] text-muted-foreground font-mono space-y-0.5">
              <div>git_sha: {result.git_sha ?? "—"}</div>
              <div>
                source:{" "}
                {result.source === "live_tree" && expectedNonLiveSource ? (
                  <span className="text-destructive font-semibold">
                    {result.source} — provenance was dropped! (expected a worktree/ref)
                  </span>
                ) : (
                  <span>{result.source ?? "—"}</span>
                )}
              </div>
              {result.git_ref_resolved_short && (
                <div>ref resolved: {result.git_ref_resolved_short}</div>
              )}
            </div>
            {result.message && <p className="text-[11px] text-muted-foreground">{result.message}</p>}
            <p className="text-[11px] text-muted-foreground select-text break-all font-mono">
              http://localhost:{result.port}/
            </p>
            <div className="flex items-center gap-2 pt-1">
              <button
                type="button"
                onClick={() => void handleOpen(result.port)}
                className="flex items-center gap-1.5 px-2 py-1 text-xs rounded border border-border hover:bg-accent transition-colors"
              >
                <ExternalLink className="w-3.5 h-3.5" />
                Open
              </button>
              <button
                type="button"
                onClick={() => void handleStop(result.id)}
                className="flex items-center gap-1.5 px-2 py-1 text-xs rounded border border-border text-destructive hover:bg-destructive/10 transition-colors"
              >
                <Square className="w-3.5 h-3.5" />
                Stop
              </button>
            </div>
          </div>
        )}
      </div>

      {/* Running temp runners */}
      <div className="space-y-2 p-3 rounded-lg border border-border bg-card">
        <div className="flex items-center justify-between">
          <div className="text-sm font-semibold">Running test runners</div>
          <button
            type="button"
            onClick={() => void refreshTempRunners()}
            className="flex items-center gap-1.5 px-2 py-1 text-xs rounded border border-border hover:bg-accent transition-colors"
          >
            <RefreshCw className="w-3.5 h-3.5" />
            Refresh
          </button>
        </div>
        {tempRunners.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            No temp runners. Build a change above to spawn one.
          </p>
        ) : (
          <div className="space-y-1.5">
            {tempRunners.map((r) => (
              <div
                key={r.id}
                className="flex items-center gap-3 p-2 rounded border border-border/60 bg-background/30"
              >
                <span
                  className={`w-2.5 h-2.5 rounded-full shrink-0 ${
                    r.running && r.api_responding
                      ? "bg-green-500"
                      : r.running
                        ? "bg-yellow-500"
                        : "bg-gray-500"
                  }`}
                  title={r.running ? (r.api_responding ? "Ready" : "Starting…") : "Stopped"}
                />
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium truncate">{r.name}</div>
                  <div className="text-xs text-muted-foreground">
                    Port {r.port}
                    {r.pid != null && <span className="ml-2">PID {r.pid}</span>}
                  </div>
                </div>
                <div className="flex items-center gap-1 shrink-0">
                  <button
                    type="button"
                    onClick={() => void handleOpen(r.port)}
                    className="p-1.5 rounded hover:bg-accent text-muted-foreground transition-colors"
                    title="Open"
                  >
                    <ExternalLink className="w-4 h-4" />
                  </button>
                  <button
                    type="button"
                    onClick={() => void handleStop(r.id)}
                    className="p-1.5 rounded hover:bg-destructive/10 text-destructive transition-colors"
                    title="Stop"
                  >
                    <Square className="w-4 h-4" />
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
