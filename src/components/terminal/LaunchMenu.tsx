import { useState, useMemo, useRef, useEffect, useCallback } from "react";
import type { JSX } from "react";
import { listen } from "@tauri-apps/api/event";
import { Star, Terminal, Play, Loader2, Lock } from "lucide-react";
import { getApiPort } from "@/lib/runner-api";
import type {
  AccountUsageInfo,
  AiStatus,
  ConflictReport,
  FileLockInfo,
  FileRegistryInfoEntry,
  PredictedCollision,
} from "./useSessionManager";

interface LaunchMenuProps {
  onCreatePlain: (count: number) => void;
  onCreateAiSession: (count: number, configDir: string, context?: string) => void;
  onCreateMultiAiSessions: (configDirs: string[], context?: string) => void;
  onCreateWithCommand: (count: number, command: string) => void;
  accountUsage: AccountUsageInfo[];
  launchCommands?: Record<string, string>;
  fileLocks?: FileLockInfo[];
  /**
   * Active-tab working directory plumbed in by `TerminalTabBar`. The
   * probe sends this in its JSON body so the runner's
   * `path_extraction::extract_candidate_paths` can resolve relative-path
   * tokens (e.g. "edit src/foo.rs") against the launching session's
   * cwd, rather than degrading to bare-filename matching against the
   * registry's normalized keys. Undefined / first-tab case falls back
   * to "" — matches the legacy hardcoded behavior and the backend
   * handler's documented "no relative-path resolution" mode.
   */
  cwd?: string;
  /**
   * Resolves a holder name (matches a tab's `title`) to focus the
   * corresponding terminal tab. No-op when no tab matches — the holder
   * may be a non-terminal session (e.g. a workflow runner). Mirrors
   * `findTabByHolderName` from `useFileLockTracking.ts:44-49`.
   */
  onJumpToHolder?: (holderName: string) => void;
  /**
   * Resolves a `task_run_id` (e.g. from `report.recent_editors`) to a
   * friendly display name. Returns `undefined` when no mapping exists
   * — callers should fall back to a truncated id. Wired in the
   * parent (`TerminalTabBar.tsx`) using the same tab-lookup pattern as
   * `useFileLockTracking.ts:44-49`. See plan §1 gap 4.
   */
  resolveSessionName?: (taskRunId: string) => string | undefined;
  onClose: () => void;
}

const COUNTS = [2, 4, 6, 9] as const;

// ── Probe constants ──────────────────────────────────────────────────────────

/**
 * Threshold of predicted collisions at which the launch buttons get
 * dimmed as a hint (still clickable). Below this, no visual change.
 */
export const COLLISION_DIM_THRESHOLD = 3;
/** Debounce delay for re-probing on prompt changes (ms). */
export const PROBE_DEBOUNCE_MS = 300;
/** Polling interval while the menu is open (ms). */
export const PROBE_POLL_INTERVAL_MS = 2000;

// ── Pure helpers (exported for tests) ────────────────────────────────────────

/**
 * Resolve the runner HTTP port for local-control-surface traffic
 * (`/file-registry/...`, `/file-locks/...`, etc.).
 *
 * Resolution order:
 *   1. `window.__QONTINUI_PORT__` if explicitly set — manual-test
 *      override and the historical fallback hatch.
 *   2. `getApiPort()` from `@/lib/runner-api` — the centralized port
 *      populated by `useApiReady` from Tauri's `api-ready` event and
 *      the `get_api_port` IPC. Defaults to 9876 when the API isn't
 *      bound yet, so first-poll on a secondary runner can still
 *      briefly hit the primary; the next 2s poll cycle picks up the
 *      correct port once `setApiPort` runs.
 *
 * The hardcoded `9876` fallback is intentionally absent here — it
 * lives inside `getApiPort()`'s default state so it's a single source
 * of truth for the entire frontend.
 */
export function resolvePort(): number {
  if (typeof window === "undefined") return getApiPort();
  const fromGlobal = (window as unknown as Record<string, unknown>).__QONTINUI_PORT__;
  if (fromGlobal) return Number(fromGlobal);
  return getApiPort();
}

export function buildProbeUrl(port: number): string {
  return `http://127.0.0.1:${port}/file-registry/probe-conflicts`;
}

/**
 * Build the JSON body for a probe request.
 *
 * `cwd` defaults to `""` when undefined — matches the legacy hardcoded
 * behavior and the backend handler's documented "no relative-path
 * resolution" mode. An explicit `""` from the caller is preserved
 * (not coerced to undefined / dropped from the body) so the wire shape
 * is stable regardless of which call site reached us.
 */
export function buildProbeBody(prompt: string | null, cwd: string | undefined): string {
  return JSON.stringify({ prompt, cwd: cwd ?? "" });
}

/**
 * Group `live_holdings` by `holder_task_run_id`. Each group keeps the
 * holder name (taken from any of the entries — they share it) and the
 * count of files held + the **oldest** `registered_at` (so age formats
 * to the longest-running file held by that holder).
 */
export interface HolderGroup {
  holder_task_run_id: string;
  holder_name: string;
  count: number;
  oldest_registered_at: number;
}

export function groupHoldingsByHolder(
  holdings: readonly FileRegistryInfoEntry[],
): HolderGroup[] {
  const map = new Map<string, HolderGroup>();
  for (const h of holdings) {
    const existing = map.get(h.holder_task_run_id);
    if (existing) {
      existing.count += 1;
      if (h.registered_at < existing.oldest_registered_at) {
        existing.oldest_registered_at = h.registered_at;
      }
    } else {
      map.set(h.holder_task_run_id, {
        holder_task_run_id: h.holder_task_run_id,
        holder_name: h.holder_name,
        count: 1,
        oldest_registered_at: h.registered_at,
      });
    }
  }
  // Stable ordering: oldest holding first (matches "who's been working on
  // this longest"). Tie-break by holder name for determinism.
  return Array.from(map.values()).sort((a, b) => {
    if (a.oldest_registered_at !== b.oldest_registered_at) {
      return a.oldest_registered_at - b.oldest_registered_at;
    }
    return a.holder_name.localeCompare(b.holder_name);
  });
}

/**
 * Format a registered_at timestamp (ms) as a short relative-age string:
 * "5s", "42s", "5m", "2h". Negative ages clamp to "0s" — clock skew
 * shouldn't show "-12s" in the UI.
 */
export function formatHoldingAge(nowMs: number, registeredAtMs: number): string {
  const deltaSec = Math.max(0, Math.floor((nowMs - registeredAtMs) / 1000));
  if (deltaSec < 60) return `${deltaSec}s`;
  const deltaMin = Math.floor(deltaSec / 60);
  if (deltaMin < 60) return `${deltaMin}m`;
  const deltaHr = Math.floor(deltaMin / 60);
  return `${deltaHr}h`;
}

/**
 * Pre-probe fallback: summarize `fileLocks` (the legacy pre-probe data
 * source) into "X file(s) locked by Y, ..." parts. Kept exported so the
 * fallback render path stays testable independently of the probe.
 */
export function summarizeFileLocksByHolder(locks: readonly FileLockInfo[]): string {
  const byHolder = new Map<string, number>();
  for (const lock of locks) {
    byHolder.set(lock.holder_name, (byHolder.get(lock.holder_name) ?? 0) + 1);
  }
  return Array.from(byHolder.entries())
    .map(([name, count]) => `${count} file${count > 1 ? "s" : ""} locked by ${name}`)
    .join(", ");
}

// ── Confidence bucketing for predicted-collision badges ─────────────────────

/**
 * Confidence tiers for the {@link PredictedCollision.confidence} score
 * (Phase 1 plan delta gap §1). Three discrete buckets keep the UI noise
 * low — the underlying score is continuous but users only need a quick
 * "how worried should I be" hint.
 */
export type ConfidenceTier = "match" | "likely" | "maybe";

/**
 * Bucket a confidence score into one of three tiers:
 * - `>= 0.9` → `"match"` (extracted_candidates contains the literal path)
 * - `0.5..0.9` → `"likely"` (substring/basename overlap)
 * - `< 0.5` → `"maybe"` (directory-level overlap only)
 */
export function confidenceTier(confidence: number): ConfidenceTier {
  if (confidence >= 0.9) return "match";
  if (confidence >= 0.5) return "likely";
  return "maybe";
}

/** Tailwind classes per tier — co-located with {@link confidenceTier}. */
export const CONFIDENCE_TIER_CLASS: Record<ConfidenceTier, string> = {
  match: "bg-[#f7768e]/15 text-[#f7768e]",
  likely: "bg-[#e0af68]/15 text-[#e0af68]",
  maybe: "bg-[#565f89]/15 text-[#a9b1d6]",
};

// ── UI subcomponents ─────────────────────────────────────────────────────────

function CountButtons({
  counts = COUNTS,
  onSelect,
  suffix,
}: {
  counts?: readonly number[];
  onSelect: (count: number) => void;
  suffix?: string;
}) {
  return (
    <span className="flex items-center gap-1 ml-auto shrink-0">
      {counts.map((c) => (
        <button
          key={c}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(c);
          }}
          className="px-1.5 py-0.5 rounded text-[10px] font-mono bg-[#2a2d3d]/60 text-[#a9b1d6] hover:bg-[#7aa2f7]/20 hover:text-[#7aa2f7] transition-colors"
        >
          {c}
          {suffix}
        </button>
      ))}
    </span>
  );
}

function UtilizationBar({ utilization }: { utilization: number }) {
  const pct = Math.round((utilization ?? 0) * 100);
  const color = pct >= 80 ? "#f7768e" : pct >= 50 ? "#e0af68" : "#9ece6a";

  return (
    <span className="flex items-center gap-1.5 shrink-0">
      <span className="w-12 h-1.5 rounded-full bg-[#2a2d3d] overflow-hidden">
        <span
          className="block h-full rounded-full transition-all"
          style={{ width: `${pct}%`, backgroundColor: color }}
        />
      </span>
      <span className="text-[10px] font-mono w-7 text-right" style={{ color }}>
        {pct}%
      </span>
    </span>
  );
}

function SectionHeader({ children }: { children: React.ReactNode }) {
  return (
    <div className="px-3 py-1.5 text-[9px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d] select-none">
      {children}
    </div>
  );
}

/**
 * Small badge shown when the AI extractor's status is non-`"Ok"`. Tells
 * the user the prediction pipeline degraded (offline, rate-limited,
 * disabled in settings) so an empty `predicted_collisions` list isn't
 * silently trusted.
 *
 * Two render paths:
 *   - default: sits in the predicted-collisions panel header (right
 *     side), next to the panel title.
 *   - `compact`: rendered as a standalone muted line when there are no
 *     predicted collisions but the AI is still degraded — the user
 *     would otherwise see nothing.
 */
export function AiStatusBadge({
  status,
  compact = false,
}: {
  status: Exclude<AiStatus, "Ok">;
  compact?: boolean;
}): JSX.Element {
  const label =
    status === "Offline"
      ? "AI offline — regex still running"
      : status === "RateLimited"
        ? "AI rate-limited — regex still running"
        : /* Disabled */ "AI prediction off (Settings)";
  // Gear for the user-actionable settings case, info glyph for the
  // transient (offline / rate-limited) cases.
  const icon = status === "Disabled" ? "⚙" : "ⓘ";
  const baseCls = "inline-flex items-center gap-1 text-[10px] text-[#a9b1d6]";
  return (
    <span
      data-testid="ai-status-badge"
      data-status={status}
      data-compact={compact ? "true" : "false"}
      className={compact ? `${baseCls} px-2 py-0.5` : baseCls}
      title={label}
    >
      <span aria-hidden>{icon}</span>
      <span>{label}</span>
    </span>
  );
}

/**
 * "Wait for release" subcomponent.
 *
 * Subscribes to `file-lock-released` events (added in this plan
 * iteration) for the captured set of conflict file paths. As each
 * release arrives, drops the file from the pending set; when the set
 * empties, fires {@link onAllClear} (the launch action the user picked).
 *
 * Rendered inline inside the existing predicted-collisions panel
 * rather than as a separate top-level component — keeps the diff small
 * and the UI focused on the panel that already shows the conflicts.
 *
 * Cancellation: the wrapping `LaunchMenu` re-mounts the panel each
 * time `report.predicted_collisions` changes (parent key is
 * `c.file_path`), so the user can also dismiss by clearing the
 * relevant prompt or closing the menu — both unmount this component
 * and run its cleanup.
 */
function WaitForReleasePanel({
  filePaths,
  onAllClear,
}: {
  filePaths: readonly string[];
  onAllClear: () => void;
}) {
  // `idle` until the user clicks "Wait", then `waiting`. Cancelling
  // returns to `idle` and clears the listener. `pending` only carries
  // meaningful state while waiting — when idle, the displayed count
  // (if any) reads off the live `filePaths` prop directly.
  const [state, setState] = useState<"idle" | "waiting">("idle");
  const [pending, setPending] = useState<ReadonlySet<string>>(() => new Set());
  const allClearRef = useRef(onAllClear);
  useEffect(() => {
    allClearRef.current = onAllClear;
  }, [onAllClear]);

  // Keep a ref to the current file paths so the click handler captures
  // the snapshot at click time without us needing a state mirror that
  // updates every render (avoids the react-hooks/set-state-in-effect
  // warning the prior implementation triggered).
  const filePathsRef = useRef(filePaths);
  useEffect(() => {
    filePathsRef.current = filePaths;
  }, [filePaths]);

  // Subscribe / unsubscribe to file-lock-released while waiting.
  useEffect(() => {
    if (state !== "waiting") return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    listen<{
      type: string;
      file_path: string;
      task_run_id: string;
      holder_name: string;
    }>("file-lock-released", (event) => {
      if (cancelled) return;
      const { file_path } = event.payload;
      setPending((prev) => {
        if (!prev.has(file_path)) return prev;
        const next = new Set(prev);
        next.delete(file_path);
        if (next.size === 0) {
          // Defer the launch until after this state update commits so
          // React's setState batching doesn't get confused by a parent
          // unmounting us mid-render.
          queueMicrotask(() => allClearRef.current());
        }
        return next;
      });
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [state]);

  if (state === "idle") {
    return (
      <button
        type="button"
        data-testid="wait-for-release-button"
        onClick={(e) => {
          e.stopPropagation();
          setPending(new Set(filePathsRef.current));
          setState("waiting");
        }}
        className="text-[10px] text-[#7aa2f7] hover:underline"
      >
        Wait for release
      </button>
    );
  }

  // waiting state
  return (
    <span
      data-testid="waiting-for-release-indicator"
      className="flex items-center gap-1 text-[10px] text-[#e0af68]"
    >
      <Loader2 className="w-3 h-3 animate-spin" />
      <span>
        waiting on {pending.size} file{pending.size === 1 ? "" : "s"}
      </span>
      <button
        type="button"
        data-testid="cancel-wait-button"
        onClick={(e) => {
          e.stopPropagation();
          setState("idle");
        }}
        className="ml-1 text-[#565f89] hover:text-[#c0caf5]"
      >
        cancel
      </button>
    </span>
  );
}

export function LaunchMenu({
  onCreatePlain,
  onCreateAiSession,
  onCreateMultiAiSessions,
  onCreateWithCommand,
  accountUsage,
  launchCommands,
  fileLocks,
  cwd,
  onJumpToHolder,
  resolveSessionName,
  onClose,
}: LaunchMenuProps) {
  const [customCommand, setCustomCommand] = useState("");
  const [sessionContext, setSessionContext] = useState("");
  const [report, setReport] = useState<ConflictReport | null>(null);
  // Captured `Date.now()` at the time of the latest probe response. Ages
  // are computed against this so render is pure; the value refreshes
  // whenever a new report lands (debounce or 2s poll). This avoids
  // calling `Date.now()` directly during render — see the
  // `react-hooks/purity` rule.
  const [reportFetchedAt, setReportFetchedAt] = useState<number>(() => Date.now());
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [onClose]);

  // ── Probe: fetch ConflictReport on prompt change (debounced) ───────────────
  // The probe runs against the runner's local /file-registry/probe-conflicts
  // endpoint. `cwd` is per-active-tab semantics — plumbed in by
  // `TerminalTabBar` from the focused tab's `workingDir`. When undefined
  // (e.g. first-tab case), buildProbeBody defaults it to "" so the backend
  // falls back to its "no relative-path resolution; only absolute paths
  // and bare-filename matches" mode.

  const runProbe = useCallback(
    async (prompt: string | null, signal?: AbortSignal): Promise<void> => {
      try {
        const port = resolvePort();
        const resp = await fetch(buildProbeUrl(port), {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: buildProbeBody(prompt, cwd),
          signal,
        });
        if (!resp.ok) return;
        const json = (await resp.json()) as ConflictReport;
        if (signal?.aborted) return;
        setReport(json);
        setReportFetchedAt(Date.now());
      } catch {
        // Network or abort — leave the existing report in place so the
        // UI doesn't flicker when the runner is briefly busy.
      }
    },
    [cwd],
  );

  // Debounced re-probe on prompt change. `runProbe` is included in the
  // dep list — and `runProbe` itself depends on `cwd` — so a tab switch
  // that changes the active workingDir re-fires the probe with the new
  // cwd.
  useEffect(() => {
    const controller = new AbortController();
    const handle = setTimeout(() => {
      const prompt = sessionContext.trim() || null;
      void runProbe(prompt, controller.signal);
    }, PROBE_DEBOUNCE_MS);
    return () => {
      clearTimeout(handle);
      controller.abort();
    };
  }, [sessionContext, runProbe]);

  // 2 s polling while the menu is open. The debounce effect above
  // handles prompt-driven re-probes; this one keeps `live_holdings`
  // fresh even when the user isn't typing.
  useEffect(() => {
    let cancelled = false;
    const tick = () => {
      if (cancelled) return;
      const prompt = sessionContext.trim() || null;
      void runProbe(prompt);
    };
    const interval = setInterval(tick, PROBE_POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
    // sessionContext is read at tick time via closure — we don't want
    // a fresh interval per keystroke (the debounce effect already
    // covers that path). Empty deps keep the interval stable for the
    // menu's lifetime. ESLint exhaustive-deps disagrees; we accept
    // the staleness because the polling path is a "fresh ambient data"
    // primitive, not a "react to prompt" primitive.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runProbe]);

  const sortedAccounts = useMemo(
    () => [...accountUsage].sort((a, b) => (a.utilization ?? 0) - (b.utilization ?? 0)),
    [accountUsage],
  );

  const bestAccount = sortedAccounts.length > 0 ? sortedAccounts[0] : null;
  const hasAccounts = accountUsage.length > 0;

  const getCustomCommand = (configDir: string): string | undefined => launchCommands?.[configDir];

  const ctx = sessionContext.trim() || undefined;

  // Dim the launch buttons (still clickable) when the predicted-collision
  // count crosses the threshold — visibility-first per
  // `feedback_worktrees_vs_pauses.md`: the user decides, we surface signal.
  const collisionCount = report?.predicted_collisions?.length ?? 0;
  const dimLaunch = collisionCount >= COLLISION_DIM_THRESHOLD;
  const dimClass = dimLaunch ? "opacity-60" : "";

  const launchAccount = (count: number, configDir: string) => {
    // Always route through onCreateAiSession so context is preserved.
    // The handler checks for custom launch commands internally.
    onCreateAiSession(count, configDir, ctx);
  };

  const extractLabel = (configDir: string): string => {
    const normalized = configDir.replace(/\\/g, "/").replace(/\/$/, "");
    const last = normalized.split("/").pop() ?? "";
    const match = last.match(/^\.claude-(.+)$/);
    return match ? match[1] : "default";
  };

  const distributeRoundRobin = (count: number): string[] => {
    if (sortedAccounts.length === 0) return [];
    return Array.from(
      { length: count },
      (_, i) => sortedAccounts[i % sortedAccounts.length].config_dir,
    );
  };

  const fire = (fn: () => void) => {
    fn();
    onClose();
  };

  // ── "Currently editing" panel data ────────────────────────────────────────

  const liveHoldings = report?.live_holdings ?? null;
  const holderGroups = useMemo<HolderGroup[] | null>(
    () => (liveHoldings ? groupHoldingsByHolder(liveHoldings) : null),
    [liveHoldings],
  );
  const now = reportFetchedAt;
  // Pre-probe fallback: render the legacy summary only if the probe
  // hasn't resolved yet but we have lock data (avoids flicker on first
  // open).
  const showLegacyLockSummary =
    holderGroups === null && fileLocks !== undefined && fileLocks.length > 0;
  const legacyLockSummary = showLegacyLockSummary ? summarizeFileLocksByHolder(fileLocks!) : "";

  return (
    <div
      ref={menuRef}
      className="absolute left-0 top-full mt-1 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden min-w-[280px]"
    >
      {/* ── Plain Terminal ────────────────────────────────────── */}
      <SectionHeader>Plain Terminal</SectionHeader>

      <button
        onClick={() => fire(() => onCreatePlain(1))}
        className="w-full flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#7aa2f7]/10 transition-colors"
      >
        <Terminal className="w-3.5 h-3.5 text-[#565f89]" />
        <span>Open 1 terminal</span>
      </button>

      <div className="flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#7aa2f7]/10 transition-colors">
        <Terminal className="w-3.5 h-3.5 text-[#565f89]" />
        <span>Open multiple</span>
        <CountButtons onSelect={(c) => fire(() => onCreatePlain(c))} />
      </div>

      {/* ── AI Session ────────────────────────────────────────── */}
      {hasAccounts && (
        <>
          <SectionHeader>AI Session</SectionHeader>

          {/* Currently editing — per-holder rows from `report.live_holdings`. */}
          {holderGroups && holderGroups.length > 0 && (
            <div
              data-testid="currently-editing-panel"
              className="px-3 py-1.5 text-[10px] text-[#a9b1d6] bg-[#1f2030] border-b border-[#2a2d3d]"
            >
              <div className="text-[9px] uppercase tracking-wider text-[#565f89] mb-1">
                Currently editing
              </div>
              {holderGroups.map((g) => (
                <div
                  key={g.holder_task_run_id}
                  data-testid="currently-editing-row"
                  className="flex items-center gap-1.5"
                >
                  <Lock className="w-3 h-3 shrink-0 text-[#7aa2f7] opacity-70" />
                  <span className="truncate">{g.holder_name}</span>
                  <span className="text-[#565f89]">
                    {g.count} file{g.count > 1 ? "s" : ""}
                  </span>
                  <span className="ml-auto text-[#565f89] font-mono">
                    {formatHoldingAge(now, g.oldest_registered_at)}
                  </span>
                </div>
              ))}
            </div>
          )}

          {/* Pre-probe fallback: brief one-liner before the first probe lands. */}
          {showLegacyLockSummary && (
            <div className="flex items-center gap-1.5 px-3 py-1 text-[10px] text-[#e0af68] bg-[#e0af68]/5 border-b border-[#2a2d3d]">
              <Lock className="w-3 h-3 shrink-0" />
              <span>{legacyLockSummary}</span>
            </div>
          )}

          {/* Predicted-collision warning panel — yellow, never red. The
              warning is a hint, not a block; the launch buttons stay
              clickable. */}
          {report && report.predicted_collisions.length > 0 && (
            <div
              data-testid="predicted-collisions-panel"
              className="px-3 py-2 border-b border-[#e0af68]/30 bg-[#e0af68]/5"
            >
              <div className="flex items-center justify-between">
                <div className="text-[10px] uppercase tracking-wider text-[#e0af68]">
                  Possible conflicts
                </div>
                {report.ai_status !== "Ok" && <AiStatusBadge status={report.ai_status} />}
              </div>
              {report.predicted_collisions.map((c: PredictedCollision) => {
                const holders = c.other_holders.map((h) => h.holder_name).join(", ");
                const firstHolder = c.other_holders[0]?.holder_name;
                // confidence is part of the wire shape, but defensively
                // default to 0 so older runner builds (pre-confidence)
                // still render — they just won't show the high-tier badge.
                const tier = confidenceTier(c.confidence ?? 0);
                return (
                  <div
                    key={c.file_path}
                    data-testid="predicted-collision-row"
                    className="flex items-center gap-2 mt-1"
                  >
                    <span
                      data-testid="confidence-badge"
                      data-tier={tier}
                      className={`px-1 py-0 rounded text-[9px] font-medium uppercase tracking-wider shrink-0 ${CONFIDENCE_TIER_CLASS[tier]}`}
                      title={`confidence ${(typeof c.confidence === "number" ? c.confidence : 0).toFixed(2)}`}
                    >
                      {tier}
                    </span>
                    <span className="font-mono text-xs truncate">{c.file_path}</span>
                    <span className="text-[10px] text-[#a9b1d6] truncate">held by {holders}</span>
                    {firstHolder && onJumpToHolder && (
                      <button
                        type="button"
                        data-testid="open-holder-button"
                        onClick={(e) => {
                          e.stopPropagation();
                          onJumpToHolder(firstHolder);
                        }}
                        className="ml-auto text-[10px] text-[#7aa2f7] hover:underline"
                      >
                        Open holder
                      </button>
                    )}
                  </div>
                );
              })}
              {/* Wait-for-release: subscribes to file-lock-released for
                  the current conflict file paths and auto-fires the
                  Best Available launch when all clear. Shown only when
                  we have at least one usable launch target — falling
                  back to onCreatePlain doesn't make sense for an
                  "after lock release" affordance. */}
              {bestAccount && (
                <div
                  data-testid="wait-for-release-row"
                  className="flex items-center justify-end mt-2"
                >
                  <WaitForReleasePanel
                    filePaths={report.predicted_collisions.map((c) => c.file_path)}
                    onAllClear={() => {
                      // Fire the equivalent of "Best Available, count=1"
                      // and close the menu — matches what a click on
                      // that button would do. The user opted into the
                      // wait, so the best-available pick is the
                      // sensible default; if they wanted a different
                      // account they could have just clicked it
                      // (launch was never blocked, only dimmed).
                      onCreateAiSession(1, bestAccount.config_dir, ctx);
                      onClose();
                    }}
                  />
                </div>
              )}
              {/* Recent editors hint — when the probe surfaced a
                  prior editor of one of the candidate paths, show
                  their friendly name (resolved via the parent's tab
                  lookup). Falls back to a truncated task_run_id when
                  no mapping is known (workflow runner, dormant
                  session, etc). */}
              {resolveSessionName && report.recent_editors.length > 0 && (
                <div
                  data-testid="recent-editors-list"
                  className="mt-2 text-[10px] text-[#565f89]"
                >
                  Recent editors:{" "}
                  {report.recent_editors.map((e, i) => {
                    const name = resolveSessionName(e.task_run_id);
                    return (
                      <span key={`${e.task_run_id}:${e.file_path}:${i}`}>
                        {i > 0 && ", "}
                        <span
                          data-testid="recent-editor-name"
                          title={`${e.task_run_id} edited ${e.file_path}`}
                          className={name ? "text-[#a9b1d6]" : "font-mono"}
                        >
                          {name ?? e.task_run_id.slice(0, 8)}
                        </span>
                      </span>
                    );
                  })}
                </div>
              )}
            </div>
          )}

          {/* Session context / initial instructions */}
          <div className="px-3 py-1.5 border-b border-[#2a2d3d]">
            <textarea
              data-testid="launch-menu-context"
              value={sessionContext}
              onChange={(e) => setSessionContext(e.target.value)}
              onKeyDown={(e) => e.stopPropagation()}
              placeholder="Initial instructions (optional)"
              rows={2}
              className="w-full bg-[#13141f] border border-[#2a2d3d] rounded px-2 py-1 text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] transition-colors resize-none"
            />
          </div>

          {/* AI-status compact line: surfaces a degraded extractor when
              the predicted-collisions panel above is empty (and thus
              hidden). Without this, the user might trust an empty
              probe result that's actually missing the AI half. */}
          {report &&
            report.ai_status !== "Ok" &&
            report.predicted_collisions.length === 0 && (
              <div
                data-testid="ai-status-compact-line"
                className="px-3 py-1 border-b border-[#2a2d3d]"
              >
                <AiStatusBadge status={report.ai_status} compact />
              </div>
            )}

          {/* Best available - single */}
          {bestAccount && (
            <>
              <button
                onClick={() => fire(() => launchAccount(1, bestAccount.config_dir))}
                className={`w-full flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#9ece6a]/10 transition-colors ${dimClass}`}
              >
                <Star className="w-3.5 h-3.5 text-[#e0af68]" />
                <span className="flex-1 text-left">
                  Best Available
                  <span className="ml-1.5 text-[10px] text-[#565f89]">
                    ({extractLabel(bestAccount.config_dir)}{" "}
                    {Math.round(bestAccount.utilization * 100)}%)
                  </span>
                </span>
              </button>

              {/* Best available - multi */}
              <div
                className={`flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#9ece6a]/10 transition-colors ${dimClass}`}
              >
                <Star className="w-3.5 h-3.5 text-[#e0af68] opacity-50" />
                <span className="text-left">Best Account x N</span>
                <CountButtons
                  counts={[2, 4, 6]}
                  onSelect={(c) => fire(() => launchAccount(c, bestAccount.config_dir))}
                />
              </div>
            </>
          )}

          {/* Divider */}
          <div className="mx-3 border-t border-[#2a2d3d]" />

          {/* Individual accounts */}
          {sortedAccounts.map((account) => {
            const cmd = getCustomCommand(account.config_dir);
            return (
              <button
                key={account.config_dir}
                onClick={() => fire(() => launchAccount(1, account.config_dir))}
                className={`w-full flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#7aa2f7]/10 transition-colors ${dimClass}`}
              >
                <span className="w-2 h-2 rounded-full bg-[#7aa2f7] shrink-0" />
                <span className="flex-1 text-left truncate">
                  {extractLabel(account.config_dir)}
                  {cmd && (
                    <span className="ml-1.5 text-[10px] text-[#565f89] font-mono">{cmd}</span>
                  )}
                </span>
                <UtilizationBar utilization={account.utilization} />
              </button>
            );
          })}

          {/* Multi-account round-robin */}
          {sortedAccounts.length > 1 && (
            <>
              <div className="mx-3 border-t border-[#2a2d3d]" />
              <div
                className={`flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#bb9af7]/10 transition-colors ${dimClass}`}
              >
                <Play className="w-3.5 h-3.5 text-[#bb9af7]" />
                <span className="text-left">Round-robin accounts</span>
                <CountButtons
                  counts={[2, 4, 6]}
                  onSelect={(c) =>
                    fire(() => onCreateMultiAiSessions(distributeRoundRobin(c), ctx))
                  }
                />
              </div>
            </>
          )}
        </>
      )}

      {/* Loading state when no accounts yet */}
      {!hasAccounts && (
        <>
          <SectionHeader>AI Session</SectionHeader>
          <div className="flex items-center gap-2 px-3 py-2 text-[10px] text-[#565f89]">
            <Loader2 className="w-3 h-3 animate-spin" />
            <span>Checking account availability...</span>
          </div>
        </>
      )}

      {/* ── Custom Command ────────────────────────────────────── */}
      <SectionHeader>Custom Command</SectionHeader>

      <div className="px-3 py-1.5">
        <input
          value={customCommand}
          onChange={(e) => setCustomCommand(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter" && customCommand.trim()) {
              fire(() => onCreateWithCommand(1, customCommand.trim()));
            }
          }}
          placeholder='Auto-run command (e.g. "npm run dev")'
          className="w-full bg-[#13141f] border border-[#2a2d3d] rounded px-2 py-1 text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] transition-colors"
        />
      </div>

      <div className="flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5]">
        <Play className="w-3.5 h-3.5 text-[#565f89]" />
        <span>Run command in</span>
        <CountButtons
          counts={[1, 2, 4, 6]}
          onSelect={(c) => {
            if (customCommand.trim()) {
              fire(() => onCreateWithCommand(c, customCommand.trim()));
            }
          }}
        />
      </div>
    </div>
  );
}
