/**
 * AccountUsageCard.tsx
 *
 * Shows per-account token usage vs expected usage for the LLM Observability dashboard.
 * Fetches from the MCP HTTP API endpoint /analytics/account-usage.
 */

import { useEffect, useState } from "react";
import { Users, RefreshCw } from "lucide-react";
import { getAccentColors } from "@/design-system";
import { getApiPort } from "@/lib/runner-api";
import type { AccountUsageInfo } from "../settings/types";

export function AccountUsageCard() {
  const [accounts, setAccounts] = useState<AccountUsageInfo[]>([]);
  const [loading, setLoading] = useState(false);

  const fetchUsage = async () => {
    setLoading(true);
    try {
      const port = getApiPort();
      const res = await fetch(`http://localhost:${port}/analytics/account-usage`);
      if (res.ok) {
        const json = await res.json();
        setAccounts(json.data ?? []);
      }
    } catch {
      // Silently fail — accounts may not be configured
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchUsage();
  }, []);

  const valid = accounts.filter((a) => !a.error);
  if (valid.length === 0 && !loading) return null;

  return (
    <div className="rounded-lg border border-border bg-card p-4 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium flex items-center gap-2">
          <Users className="w-4 h-4" />
          Account Usage vs Expected
        </h3>
        <button
          onClick={fetchUsage}
          disabled={loading}
          className="p-1 rounded hover:bg-muted transition-colors"
          title="Refresh account usage"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      <div className="space-y-2">
        {valid.map((account) => {
          const pct = Math.round(account.utilization * 100);
          const expPct =
            account.expected_utilization != null
              ? Math.round(account.expected_utilization * 100)
              : null;
          const delta = account.usage_delta;
          const isOver = delta != null && delta > 0.02;
          const isUnder = delta != null && delta < -0.02;

          return (
            <div key={account.config_dir} className="space-y-1">
              <div className="flex items-center justify-between text-xs">
                <span className="font-medium">{account.label}</span>
                <div className="flex items-center gap-2">
                  <span
                    className={`font-mono ${
                      pct >= 80
                        ? getAccentColors("red").text
                        : pct >= 50
                          ? getAccentColors("yellow").text
                          : getAccentColors("green").text
                    }`}
                  >
                    {pct}%
                  </span>
                  {expPct != null && (
                    <span className="text-muted-foreground">/ {expPct}% expected</span>
                  )}
                </div>
              </div>

              <div className="relative h-2 bg-muted/50 rounded-full overflow-hidden">
                <div
                  className={`h-full rounded-full transition-all ${
                    pct >= 80 ? "bg-red-500" : pct >= 50 ? "bg-yellow-500" : "bg-green-500"
                  }`}
                  style={{ width: `${Math.max(pct, 1)}%` }}
                />
                {expPct != null && (
                  <div
                    className="absolute top-0 h-full w-0.5 bg-foreground/60"
                    style={{ left: `${expPct}%` }}
                    title={`Expected: ${expPct}%`}
                  />
                )}
              </div>

              <div className="flex items-center justify-between text-[10px] text-muted-foreground">
                {delta != null && (
                  <span
                    className={
                      isOver
                        ? getAccentColors("red").text
                        : isUnder
                          ? getAccentColors("green").text
                          : ""
                    }
                  >
                    {delta > 0 ? "+" : ""}
                    {Math.round(delta * 100)}pp{" "}
                    {isOver ? "over budget" : isUnder ? "under budget" : "on track"}
                  </span>
                )}
                {account.period_remaining_days != null && (
                  <span>{account.period_remaining_days.toFixed(1)} days left</span>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
