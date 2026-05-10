import { useState, useMemo, useRef, useEffect, useCallback } from "react";
import { Star, Terminal, Play, Loader2, Lock } from "lucide-react";
import type {
  AccountUsageInfo,
  ConflictReport,
  FileLockInfo,
  FileRegistryInfoEntry,
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
   * Resolves a holder name (matches a tab's `title`) to focus the
   * corresponding terminal tab. No-op when no tab matches — the holder
   * may be a non-terminal session (e.g. a workflow runner). Mirrors
   * `findTabByHolderName` from `useFileLockTracking.ts:44-49`.
   */
  onJumpToHolder?: (holderName: string) => void;
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
 * Resolve the runner HTTP port. Mirrors the pattern from
 * `useFileLockTracking.ts:90-95` so probe traffic lands on the same
 * port as the rest of the runner's local control surface.
 */
export function resolvePort(): number {
  if (typeof window === "undefined") return 9876;
  const fromGlobal = (window as unknown as Record<string, unknown>).__QONTINUI_PORT__;
  return fromGlobal ? Number(fromGlobal) : 9876;
}

export function buildProbeUrl(port: number): string {
  return `http://127.0.0.1:${port}/file-registry/probe-conflicts`;
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

export function LaunchMenu({
  onCreatePlain,
  onCreateAiSession,
  onCreateMultiAiSessions,
  onCreateWithCommand,
  accountUsage,
  launchCommands,
  fileLocks,
  onJumpToHolder,
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
  // endpoint. cwd is intentionally empty for now — the backend handles "" as
  // "no relative-path resolution; only absolute paths and bare-filename
  // matches." See plan §Open Q3.
  // TODO(plan §Open Q3): plumb the active session's cwd through here so
  // relative path tokens like "src/foo/bar.rs" can resolve against the
  // launching session's working directory rather than degrading to
  // bare-filename matching.

  const runProbe = useCallback(
    async (prompt: string | null, signal?: AbortSignal): Promise<void> => {
      try {
        const port = resolvePort();
        const resp = await fetch(buildProbeUrl(port), {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ prompt, cwd: "" }),
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
    [],
  );

  // Debounced re-probe on prompt change.
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
              <div className="text-[10px] uppercase tracking-wider text-[#e0af68]">
                Possible conflicts
              </div>
              {report.predicted_collisions.map((c) => {
                const holders = c.other_holders.map((h) => h.holder_name).join(", ");
                const firstHolder = c.other_holders[0]?.holder_name;
                return (
                  <div
                    key={c.file_path}
                    data-testid="predicted-collision-row"
                    className="flex items-center gap-2 mt-1"
                  >
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
