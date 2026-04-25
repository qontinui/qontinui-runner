/**
 * WrapperToolsBadge
 *
 * Phase 4 visibility surface: a compact pill in the Terminal session header
 * that says "N wrapper tools available" and opens a popover listing every
 * tool the in-session AI agent can call. Each row shows the wrapper name +
 * the action description so the user can verify the agent sees what they
 * expect.
 *
 * The badge stays invisible until the wrapper fetch resolves with at least
 * one eligible action — keeping the header clean for users who haven't
 * installed any wrappers yet. A "Refresh tools" button in the popover lets
 * users pick up newly-installed wrappers without restarting their session.
 *
 * Follows the same color tokens / border styling as the surrounding
 * TerminalTabBar buttons (bg-[#1a1b26]/50 hover, text-[#565f89] base).
 */

import { useEffect, useRef, useState } from "react";
import { Package, RefreshCw } from "lucide-react";
import { useWrapperTools } from "@/hooks/useWrapperTools";

interface WrapperToolsBadgeProps {
  /**
   * Hide the badge entirely (e.g., when no terminal sessions are open).
   * Defaults to true — the badge is itself self-hiding when no tools are
   * available, so callers usually don't need this.
   */
  visible?: boolean;
}

export function WrapperToolsBadge({ visible = true }: WrapperToolsBadgeProps) {
  const { tools, routes, loading, error, refresh } = useWrapperTools();
  const [open, setOpen] = useState(false);
  const popoverRef = useRef<HTMLDivElement>(null);

  // Dismiss popover on outside click — same pattern the session dropdown
  // in TerminalTabBar uses.
  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (popoverRef.current && !popoverRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  if (!visible) return null;

  // Suppress the badge when there's nothing useful to show. We keep the
  // empty branch reachable when there's an error so users can still see
  // why the list is empty.
  if (tools.length === 0 && !error && !loading) return null;

  const count = tools.length;

  return (
    <div className="relative shrink-0" ref={popoverRef}>
      <button
        onClick={() => setOpen((v) => !v)}
        className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] transition-colors ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
        }`}
        title={`${count} wrapper tool${count === 1 ? "" : "s"} available to AI agents`}
        aria-haspopup="dialog"
        aria-expanded={open}
      >
        <Package className="w-3 h-3" />
        <span className="font-mono">{count}</span>
        <span className="hidden md:inline">wrapper tool{count === 1 ? "" : "s"}</span>
      </button>

      {open && (
        <div
          role="dialog"
          aria-label="Wrapper tools available"
          className="absolute right-0 top-full mt-1 w-96 max-w-[90vw] bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden"
        >
          <div className="flex items-center justify-between px-3 py-2 border-b border-[#2a2d3d]">
            <span className="text-[10px] uppercase tracking-wider text-[#565f89]">
              Wrapper tools ({count})
            </span>
            <button
              onClick={() => void refresh()}
              disabled={loading}
              className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-colors disabled:opacity-50"
              title="Refresh wrapper tool list"
            >
              <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
              Refresh
            </button>
          </div>

          {error ? (
            <div className="px-3 py-3 text-[11px] text-[#f7768e]">
              Failed to load wrappers: {error}
            </div>
          ) : tools.length === 0 ? (
            <div className="px-3 py-3 text-[11px] text-[#565f89]">
              No wrappers installed.{" "}
              <span className="text-[#a9b1d6]">Install one from the Wrappers page.</span>
            </div>
          ) : (
            <div className="max-h-64 overflow-y-auto scrollbar-dark">
              {tools.map((tool) => {
                const route = routes.get(tool.name);
                const wrapperName =
                  route?.wrapper.manifest?.displayName ?? route?.wrapper.id ?? "unknown";
                return (
                  <div
                    key={tool.name}
                    className="px-3 py-2 border-b border-[#2a2d3d]/50 last:border-b-0 hover:bg-[#2a2d3d]/30"
                    title={`${wrapperName} — ${tool.description}`}
                  >
                    <div className="flex items-center gap-2">
                      <span className="text-[11px] font-mono text-[#7aa2f7] truncate">
                        {tool.name}
                      </span>
                      <span className="text-[9px] text-[#565f89] shrink-0">{wrapperName}</span>
                    </div>
                    {tool.description && (
                      <p className="text-[10px] text-[#a9b1d6] mt-0.5 line-clamp-2">
                        {tool.description}
                      </p>
                    )}
                  </div>
                );
              })}
            </div>
          )}

          <div className="px-3 py-1.5 text-[9px] text-[#565f89] bg-[#13141f] border-t border-[#2a2d3d]">
            Tools auto-injected on session start. Restart sessions to pick up newly-installed
            wrappers, or click Refresh.
          </div>
        </div>
      )}
    </div>
  );
}
