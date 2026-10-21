/**
 * OsHealthBanner — "this computer's operating system is degraded" notice (plan
 * `2026-10-19-fleet-is-blind-to-kernel-object-leaks-and-os-process-creation-latency`,
 * Phase 5, D5).
 *
 * On 2026-10-15 a Windows kernel driver began leaking a token on every
 * console attach; within days every process creation took seconds, and
 * nothing a user could see said why. The runner now grades that condition
 * itself, and an end user has no coord console, so this banner is the only
 * alert they get. It shows once `osHealth.verdict = breach` has held for the
 * dwell (10 minutes), names the readings that breached, and says the one
 * thing that fixes it: a restart of the OS — never of the runner, which
 * cannot help.
 *
 * Mounted once at the App root beside `ResourceGuardGrantBanner`; machine-level
 * conditions render there. A dismissal lasts until the verdict next clears.
 */

import { Cpu, X } from "lucide-react";
import { StatusBanner } from "@/components/ui/StatusBanner";
import {
  dismissOsHealthBanner,
  useOsHealth,
  type OsHealthPayload,
  type OsHealthReason,
} from "@/hooks/useOsHealth";

/** "640 ms" / "2.4 s". */
export function formatMs(ms: number): string {
  return ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`;
}

/** "4.4M" / "250k" / "900". */
export function formatCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}k`;
  return `${n}`;
}

/** "39.4 GB". */
export function formatGb(bytes: number): string {
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

function hit(reasons: OsHealthReason[], rule: string): OsHealthReason | undefined {
  return reasons.find((r) => r.rule === rule && r.band !== "unknown");
}

/**
 * PURE: the banner's sentence for a payload, from the reasons that fired and
 * the readings beside them. E.g. "Windows on this computer is creating
 * processes slowly (p50 2.4 s; kernel token objects 4.4M, normally
 * thousands). Restarting Windows clears it."
 */
export function osHealthBannerCopy(p: OsHealthPayload): string {
  const windows = p.platform === "windows";
  const os = windows ? "Windows" : "The operating system";
  const r = p.reasons;
  const failed = hit(r, "spawn_failed");
  const slow = hit(r, "spawn_p50");
  const tokens = hit(r, "token_objects") ?? hit(r, "token_slope");
  const pool = hit(r, "pool_total") ?? hit(r, "pool_nonpaged");

  const details: string[] = [];
  let headline: string;
  if (failed) {
    headline = `${os} on this computer is failing to create processes`;
    const code = p.readings?.spawnProbe?.errorCode;
    if (code != null) details.push(`error ${code}`);
  } else if (slow) {
    headline = `${os} on this computer is creating processes slowly`;
    const p50 = p.readings?.spawnProbe?.p50Ms ?? slow.value;
    if (p50 != null) details.push(`p50 ${formatMs(p50)}`);
  } else {
    headline = `${os} on this computer is running out of kernel resources`;
  }
  const tokenCount = p.readings?.tokenObjects;
  if (tokens && tokenCount != null) {
    details.push(`kernel token objects ${formatCount(tokenCount)}, normally thousands`);
  }
  const paged = p.readings?.kernelPagedBytes;
  const nonpaged = p.readings?.kernelNonpagedBytes;
  if (pool && paged != null && nonpaged != null) {
    details.push(`kernel memory pool ${formatGb(paged + nonpaged)}, normally 1–3 GB`);
  }
  const restart = windows ? "Restarting Windows clears it." : "Restarting the computer clears it.";
  return details.length > 0
    ? `${headline} (${details.join("; ")}). ${restart}`
    : `${headline}. ${restart}`;
}

export function OsHealthBanner() {
  const { payload, bannerVisible } = useOsHealth();
  if (!bannerVisible || !payload) return null;
  return (
    <div
      className="fixed top-14 left-1/2 -translate-x-1/2 z-40 w-[560px] max-w-[92%] shadow-lg"
      data-ui-bridge-id="app.os-health-banner"
      data-os-health-verdict={payload.verdict}
    >
      <StatusBanner status="error" icon={<Cpu className="w-4 h-4" />} role="alert">
        <div className="flex items-start gap-2">
          <div className="text-[12px] leading-snug flex-1">{osHealthBannerCopy(payload)}</div>
          <button
            type="button"
            onClick={dismissOsHealthBanner}
            aria-label="Dismiss until this clears"
            title="Dismiss until this clears"
            data-ui-bridge-id="app.os-health-banner.dismiss"
            className="shrink-0 rounded p-0.5 hover:bg-black/10 transition-colors"
          >
            <X className="w-3.5 h-3.5" />
          </button>
        </div>
      </StatusBanner>
    </div>
  );
}
