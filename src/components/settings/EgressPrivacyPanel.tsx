/**
 * EgressPrivacyPanel — the Privacy section's READ-ONLY view of the six
 * outbound data flows and the per-project switch governing each one.
 *
 * Plan `2026-10-10-spec-front-end-phase-9-generic-boundary`, Phase 8 item 2.
 *
 * It renders `GET /health` → `data.egress`, the same block an auditor reads,
 * so the panel and the runner cannot disagree. Each row shows the flow's
 * state, which rung decided it (this project's choice, the last answer the
 * runner persisted, this machine's profile, or the product default), and
 * "applies at next start" where a flip needs a restart. A flow the runner did
 * not report is UNKNOWN, never "on".
 *
 * The switches are the PROJECT's (tenant policy), written in the web console
 * at `/admin/coord/tenant-policy`; every row deep-links there. The runner does
 * not write tenant policy. The user's own sync toggles stay beside this panel.
 */

import { useCallback, useEffect, useState } from "react";
import { ExternalLink, RefreshCw, ShieldCheck } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { tracedFetch, useApiBase } from "@/lib/runner-api";
import { describeThrown } from "@/lib/utils";
import {
  buildEgressRows,
  egressScope,
  tenantPolicyLink,
  type EgressRow,
} from "./egressPrivacyHelpers";

interface HealthEnvelope {
  data?: { egress?: unknown };
}

function stateClasses(state: EgressRow["state"]): string {
  switch (state) {
    case "on":
      return "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400";
    case "off":
      return "bg-muted text-muted-foreground";
    default:
      return "bg-amber-500/15 text-amber-600 dark:text-amber-400";
  }
}

export function EgressPrivacyPanel({ webAppUrl }: { webAppUrl: string | null }) {
  const apiBase = useApiBase();
  const [egress, setEgress] = useState<unknown>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const response = await tracedFetch(`${apiBase}/health`);
      if (!response.ok) throw new Error(`runner API answered HTTP ${response.status}`);
      const body = (await response.json()) as HealthEnvelope;
      setEgress(body.data?.egress);
    } catch (e) {
      // Unreachable is UNKNOWN for every row — never a default.
      setError(describeThrown(e, "Failed to read the runner's egress state"));
      setEgress(undefined);
    } finally {
      setLoading(false);
    }
  }, [apiBase]);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void load();
    });
    return () => {
      cancelled = true;
    };
  }, [load]);

  const rows = buildEgressRows(egress);
  const scope = egressScope(egress);

  return (
    <div
      className="space-y-3 rounded-lg bg-card/50 p-4"
      data-ui-bridge-id="settings.privacy.egress-flows"
    >
      <div className="flex items-start justify-between gap-3">
        <div className="flex items-start gap-3">
          <ShieldCheck className="w-5 h-5 text-primary shrink-0" />
          <div>
            <h4 className="text-sm font-medium">Privacy — outbound data flows</h4>
            <p className="text-xs text-muted-foreground">
              What this runner may send off this machine, per your project&apos;s settings. Change
              them in the web console; this runner only reads them.
            </p>
          </div>
        </div>
        <button
          type="button"
          onClick={() => void load()}
          disabled={loading}
          className="px-2.5 py-1.5 text-xs rounded-md bg-muted/50 hover:bg-muted transition-colors disabled:opacity-50 flex items-center gap-1.5 shrink-0"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>

      {error && (
        <p className="text-xs text-destructive">
          Could not read the runner&apos;s egress state: {error}. Every flow below is UNKNOWN — this
          is not a statement that any of them is on or off.
        </p>
      )}

      {scope && (
        <p className="text-[10px] text-muted-foreground">
          Read for {scope.tenantId ? `project ${scope.tenantId}` : "this device's default project"}:{" "}
          {scope.note}.
        </p>
      )}

      <ul className="space-y-2">
        {rows.map((row) => {
          const link = tenantPolicyLink(webAppUrl, row.key);
          return (
            <li
              key={row.key}
              id={`egress-flow-${row.key}`}
              className="rounded-lg bg-muted/30 p-3 space-y-1"
            >
              <div className="flex items-center justify-between gap-3">
                <span className="text-sm font-medium">{row.label}</span>
                <span
                  className={`shrink-0 px-2 py-0.5 rounded-full text-[10px] font-medium uppercase ${stateClasses(row.state)}`}
                >
                  {row.state}
                </span>
              </div>
              <p className="text-xs text-muted-foreground">{row.description}</p>
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-[10px] text-muted-foreground">
                <span>source: {row.sourceText}</span>
                {row.nextStartNote && <span>{row.nextStartNote}</span>}
                {row.refused > 0 && <span>refused this run: {row.refused}</span>}
                {link ? (
                  <button
                    type="button"
                    onClick={() => void openUrl(link)}
                    className="flex items-center gap-1 text-primary hover:underline"
                  >
                    Change in web console <ExternalLink className="w-3 h-3" />
                  </button>
                ) : (
                  <span>Change it in the web console under Coord → Tenant policy</span>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
