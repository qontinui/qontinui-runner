import { useState, useMemo, useRef, useEffect } from "react";
import { Star, Terminal, Play, Loader2, Lock } from "lucide-react";
import type { AccountUsageInfo, FileLockInfo } from "./useSessionManager";

interface LaunchMenuProps {
  onCreatePlain: (count: number) => void;
  onCreateAiSession: (count: number, configDir: string, context?: string) => void;
  onCreateMultiAiSessions: (configDirs: string[], context?: string) => void;
  onCreateWithCommand: (count: number, command: string) => void;
  accountUsage: AccountUsageInfo[];
  launchCommands?: Record<string, string>;
  fileLocks?: FileLockInfo[];
  onClose: () => void;
}

const COUNTS = [2, 4, 6, 9] as const;

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
  onClose,
}: LaunchMenuProps) {
  const [customCommand, setCustomCommand] = useState("");
  const [sessionContext, setSessionContext] = useState("");
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

  const sortedAccounts = useMemo(
    () => [...accountUsage].sort((a, b) => (a.utilization ?? 0) - (b.utilization ?? 0)),
    [accountUsage],
  );

  const bestAccount = sortedAccounts.length > 0 ? sortedAccounts[0] : null;
  const hasAccounts = accountUsage.length > 0;

  const getCustomCommand = (configDir: string): string | undefined => launchCommands?.[configDir];

  const ctx = sessionContext.trim() || undefined;

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

          {/* Active file locks summary */}
          {fileLocks &&
            fileLocks.length > 0 &&
            (() => {
              const byHolder = new Map<string, number>();
              for (const lock of fileLocks) {
                byHolder.set(lock.holder_name, (byHolder.get(lock.holder_name) ?? 0) + 1);
              }
              return (
                <div className="flex items-center gap-1.5 px-3 py-1 text-[10px] text-[#e0af68] bg-[#e0af68]/5 border-b border-[#2a2d3d]">
                  <Lock className="w-3 h-3 shrink-0" />
                  <span>
                    {Array.from(byHolder.entries())
                      .map(
                        ([name, count]) => `${count} file${count > 1 ? "s" : ""} locked by ${name}`,
                      )
                      .join(", ")}
                  </span>
                </div>
              );
            })()}

          {/* Session context / initial instructions */}
          <div className="px-3 py-1.5 border-b border-[#2a2d3d]">
            <textarea
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
                className="w-full flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#9ece6a]/10 transition-colors"
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
              <div className="flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#9ece6a]/10 transition-colors">
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
                className="w-full flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#7aa2f7]/10 transition-colors"
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
              <div className="flex items-center gap-2 px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#bb9af7]/10 transition-colors">
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
