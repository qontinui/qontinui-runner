/**
 * Coord worktree-claim warning banner — Layer 3 of
 * `plans/2026-06-03-shared-checkout-coordination-gap-fix.md`.
 *
 * Branch-mutating git typed directly into the runner's Terminal PTY
 * (`git switch`, `git checkout <branch>`, `git reset --hard`) bypasses
 * every Claude Code hook, so a terminal can clobber a *peer's* in-flight
 * work in a shared checkout with no warning. The Rust side
 * (`src-tauri/src/terminal/coord_warn.rs`) detects such a command on the
 * PTY input line and — if a PEER holds the coord `worktree` claim on this
 * session's repo — emits a `terminal-coord-warning` Tauri event.
 *
 * This banner subscribes to that event, keeps the warnings for the
 * currently-active terminal, and renders a soft, dismissible advisory.
 *
 * SOFT ONLY: this never blocks input — the keystroke was already written
 * to the PTY before the warning fired. It is purely advisory and
 * dismissible. Visual language mirrors {@link DeconflictAdvisoryBanner}
 * (same soft-amber `#e0af68` accent) so it stacks consistently with the
 * other terminal advisories.
 */

import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AlertTriangle, X } from "lucide-react";
import { createLogger } from "@/lib/logger";

const logger = createLogger("CoordWarningBanner");

/**
 * Payload shape emitted by the Rust `terminal-coord-warning` event
 * (`CoordWarning` in `src-tauri/src/terminal/coord_warn.rs`,
 * `#[serde(rename_all = "camelCase")]`).
 */
export interface CoordWarning {
  terminalId: string;
  repo: string;
  holderMachineId: string;
  holderSessionId?: string | null;
  command: string;
}

/** Stable de-dup / list key for a warning. */
function warningKey(w: CoordWarning): string {
  return `${w.terminalId}|${w.repo}|${w.holderMachineId}|${w.holderSessionId ?? ""}|${w.command}`;
}

/** Shorten a long machine/session id for display (first 8 chars). */
function shortId(id: string): string {
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

export interface CoordWarningBannerProps {
  /** The active terminal's id. Only warnings for this terminal render
   *  (each terminal owns its own conflict surface). When undefined the
   *  banner renders nothing. */
  activeTerminalId: string | undefined;
}

export function CoordWarningBanner({ activeTerminalId }: CoordWarningBannerProps) {
  const [warnings, setWarnings] = useState<CoordWarning[]>([]);

  // Subscribe to `terminal-coord-warning`. The Rust side only emits when a
  // branch-mutating git command was typed AND a peer holds the worktree
  // claim, so this is rare and event-driven (never polled).
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;

    listen<unknown>("terminal-coord-warning", (event) => {
      if (cancelled) return;
      const payload = event.payload;
      if (typeof payload !== "object" || payload === null) return;
      const p = payload as Record<string, unknown>;
      const terminalId = typeof p.terminalId === "string" ? p.terminalId : null;
      const repo = typeof p.repo === "string" ? p.repo : null;
      const holderMachineId =
        typeof p.holderMachineId === "string" ? p.holderMachineId : null;
      const holderSessionId =
        typeof p.holderSessionId === "string" ? p.holderSessionId : null;
      const command = typeof p.command === "string" ? p.command : null;
      if (!terminalId || !repo || !holderMachineId || !command) return;
      const warning: CoordWarning = {
        terminalId,
        repo,
        holderMachineId,
        holderSessionId,
        command,
      };
      setWarnings((prev) => {
        const key = warningKey(warning);
        if (prev.some((w) => warningKey(w) === key)) return prev;
        return [...prev, warning];
      });
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((err) => {
        logger.error("listen('terminal-coord-warning') failed", err);
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const handleDismiss = (key: string) => {
    setWarnings((prev) => prev.filter((w) => warningKey(w) !== key));
  };

  const visible = activeTerminalId
    ? warnings.filter((w) => w.terminalId === activeTerminalId)
    : [];

  if (visible.length === 0) {
    return null;
  }

  return (
    <div
      data-ui-bridge-id="terminal.coord-warning-banner"
      data-terminal-id={activeTerminalId}
      className="absolute top-24 right-2 z-30 w-[340px] space-y-1.5"
    >
      {visible.map((w) => {
        const key = warningKey(w);
        const holder = w.holderSessionId
          ? `session ${shortId(w.holderSessionId)}`
          : `machine ${shortId(w.holderMachineId)}`;
        return (
          <div
            key={key}
            data-ui-bridge-id="terminal.coord-warning-banner-card"
            data-repo={w.repo}
            className="p-2.5 rounded border bg-[#e0af68]/10 border-[#e0af68]/35 shadow-lg"
          >
            <div className="flex items-start gap-2">
              <AlertTriangle className="w-3.5 h-3.5 shrink-0 text-[#e0af68] mt-0.5" />
              <div className="text-[11px] text-[#c0caf5] leading-snug flex-1">
                <div>
                  Another session is editing <span className="font-semibold">{w.repo}</span>{" "}
                  (held by {holder}). A branch switch here may clobber their work.
                </div>
                <div className="mt-1 font-mono text-[10px] text-[#e0af68]/80 break-all">
                  {w.command}
                </div>
              </div>
              <button
                type="button"
                aria-label="Dismiss warning"
                data-ui-bridge-id="terminal.coord-warning-banner-dismiss"
                data-repo={w.repo}
                onClick={() => handleDismiss(key)}
                className="text-[#e0af68] hover:text-[#c0caf5] leading-none px-1"
              >
                <X className="w-3 h-3" />
              </button>
            </div>
          </div>
        );
      })}
    </div>
  );
}
