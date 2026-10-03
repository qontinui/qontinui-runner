import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { RefreshCw, TerminalSquare } from "lucide-react";

import {
  installOsFor,
  launchEntry,
  type CliAvailability,
  type LaunchMenuProfile,
} from "./providerLaunchMenu";

/**
 * The Terminal page's provider launch menu: one row per served CLI profile,
 * each with its availability (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
 * Phase 6). The roster comes from the runner (`terminal_cli_profiles`), never
 * from this file; the row verdicts are `providerLaunchMenu.ts`'s. Every launch
 * is a PTY session — see that module for why no structured lane is offered.
 *
 * Rendered in `SessionManagerPanel`'s Live view beside `StewardControl`.
 */
export function ProviderLaunchMenu({ onLaunch }: { onLaunch: (provider: string) => void }) {
  const [profiles, setProfiles] = useState<LaunchMenuProfile[] | null>(null);
  const [rosterError, setRosterError] = useState<string | null>(null);
  /** Probe verdict per id: absent key = in flight, `null` = the probe could not run. */
  const [availability, setAvailability] = useState<Record<string, CliAvailability | null>>({});
  const [probeErrors, setProbeErrors] = useState<Record<string, string>>({});
  const os = installOsFor(navigator.platform);

  const probeAll = useCallback(async (): Promise<void> => {
    let roster: LaunchMenuProfile[];
    try {
      roster = await invoke<LaunchMenuProfile[]>("terminal_cli_profiles");
      setProfiles(roster);
      setRosterError(null);
    } catch (e) {
      setRosterError(e instanceof Error ? e.message : String(e));
      return;
    }
    setAvailability({});
    setProbeErrors({});
    await Promise.all(
      roster.map(async (profile) => {
        try {
          const verdict = await invoke<CliAvailability>("cli_profile_availability", {
            id: profile.id,
          });
          setAvailability((prev) => ({ ...prev, [profile.id]: verdict }));
        } catch (e) {
          setAvailability((prev) => ({ ...prev, [profile.id]: null }));
          setProbeErrors((prev) => ({
            ...prev,
            [profile.id]: e instanceof Error ? e.message : String(e),
          }));
        }
      }),
    );
  }, []);

  // First probe from a timer callback, never synchronously in the effect body
  // (react-hooks/set-state-in-effect) — the same shape as `StewardControl`.
  useEffect(() => {
    const first = setTimeout(() => void probeAll(), 0);
    return () => clearTimeout(first);
  }, [probeAll]);

  if (profiles === null) {
    if (rosterError === null) return null;
    return (
      <div
        className="px-3 py-1.5 border-b border-[#2a2d3d] text-[11px] text-[#565f89]"
        data-testid="provider-launch-unavailable"
      >
        AI CLI list unavailable — {rosterError}
      </div>
    );
  }

  return (
    <div className="border-b border-[#2a2d3d] bg-[#13141f]" data-testid="provider-launch-menu">
      <div className="flex items-center px-3 pt-1.5 text-[10px] uppercase tracking-wide text-[#565f89]">
        <span className="flex-1">Launch AI session</span>
        <button
          type="button"
          data-ui-bridge-id="terminal.provider-launch-recheck"
          onClick={() => void probeAll()}
          className="p-0.5 rounded hover:bg-[#2a2d3d] hover:text-[#c0caf5]"
          title="Check which AI CLIs are installed again"
        >
          <RefreshCw className="w-3 h-3" />
        </button>
      </div>
      {profiles.map((profile) => {
        const entry = launchEntry(
          profile,
          availability[profile.id],
          os,
          probeErrors[profile.id],
        );
        return (
          <div key={entry.id} className="px-3 py-1" data-testid={`provider-launch-row-${entry.id}`}>
            <button
              type="button"
              data-ui-bridge-id={`terminal.provider-launch-${entry.id}`}
              disabled={!entry.enabled}
              onClick={() => onLaunch(entry.id)}
              className="w-full flex items-center gap-2 text-[11px] text-[#c0caf5] hover:text-[#7aa2f7] transition-colors disabled:opacity-50 disabled:hover:text-[#c0caf5]"
              title={entry.enabled ? `Open a terminal running ${entry.label}` : (entry.detail ?? "")}
            >
              <TerminalSquare className="w-3.5 h-3.5 text-[#565f89]" />
              <span className="text-left">{entry.label}</span>
              {entry.detail && (
                <span
                  className={`flex-1 text-right text-[10px] truncate ${
                    entry.state === "unknown" ? "text-[#e0af68]" : "text-[#565f89]"
                  }`}
                  data-testid={`provider-launch-detail-${entry.id}`}
                >
                  {entry.detail}
                </span>
              )}
            </button>
            {entry.installCommand && (
              <div
                className="mt-0.5 ml-5 text-[10px] text-[#565f89]"
                data-testid={`provider-launch-install-${entry.id}`}
              >
                Install: <code className="font-mono text-[#a9b1d6] select-all">{entry.installCommand}</code>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
