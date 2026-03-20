/**
 * SpecAttentionBanner -- compact alert banner showing specs that need attention.
 *
 * Fetches `get_specs_needing_attention` on mount and displays a summary banner
 * when any specs have broken assertions, are stale, or have never been run.
 * Expandable to show the full list with per-spec details.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface SpecAttentionItem {
  spec_id: string;
  reason: string; // "broken_assertions" | "stale" | "never_run"
  broken_count: number;
  staleness_days: number;
  latest_score: number | null;
  detail: string;
}

function reasonLabel(reason: string): string {
  switch (reason) {
    case "broken_assertions":
      return "broken assertions";
    case "stale":
      return "stale";
    case "never_run":
      return "never run";
    default:
      return reason;
  }
}

function reasonBadgeClass(reason: string): string {
  switch (reason) {
    case "broken_assertions":
      return "bg-red-900/30 text-red-400 border-red-800";
    case "stale":
      return "bg-yellow-900/30 text-yellow-400 border-yellow-800";
    case "never_run":
      return "bg-zinc-700 text-zinc-400 border-zinc-600";
    default:
      return "bg-zinc-700 text-zinc-400 border-zinc-600";
  }
}

function bannerBorderClass(items: SpecAttentionItem[]): string {
  const hasBroken = items.some((i) => i.reason === "broken_assertions");
  if (hasBroken) return "border-red-700 bg-red-950/20";
  return "border-yellow-700 bg-yellow-950/20";
}

export function SpecAttentionBanner() {
  const [items, setItems] = useState<SpecAttentionItem[]>([]);
  const [expanded, setExpanded] = useState(false);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    invoke<SpecAttentionItem[]>("get_specs_needing_attention")
      .then(setItems)
      .catch((e) => console.error("Failed to load spec attention items:", e))
      .finally(() => setLoading(false));
  }, []);

  if (loading || items.length === 0) {
    return null;
  }

  const brokenCount = items.filter((i) => i.reason === "broken_assertions").length;
  const staleCount = items.filter((i) => i.reason === "stale").length;
  const neverRunCount = items.filter((i) => i.reason === "never_run").length;

  const parts: string[] = [];
  if (brokenCount > 0) {
    parts.push(`${brokenCount} with broken assertions`);
  }
  if (staleCount > 0) {
    parts.push(`${staleCount} stale`);
  }
  if (neverRunCount > 0) {
    parts.push(`${neverRunCount} never run`);
  }

  const summaryText = `${items.length} spec${items.length === 1 ? "" : "s"} need${items.length === 1 ? "s" : ""} attention: ${parts.join(", ")}`;

  return (
    <div className={`rounded-lg border p-3 mb-4 ${bannerBorderClass(items)}`}>
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className={brokenCount > 0 ? "text-red-400" : "text-yellow-400"}>
            {brokenCount > 0 ? "!!" : "!"}
          </span>
          <span className="text-sm text-zinc-200">{summaryText}</span>
        </div>
        <button
          onClick={() => setExpanded(!expanded)}
          className="text-xs text-zinc-400 hover:text-zinc-200 px-2 py-1"
        >
          {expanded ? "Collapse" : "Details"}
        </button>
      </div>

      {expanded && (
        <div className="mt-3 space-y-2">
          {items.map((item) => (
            <div
              key={`${item.spec_id}-${item.reason}`}
              className="flex items-start gap-3 bg-zinc-800/50 rounded px-3 py-2"
            >
              <span
                className={`text-xs px-2 py-0.5 rounded border whitespace-nowrap mt-0.5 ${reasonBadgeClass(item.reason)}`}
              >
                {reasonLabel(item.reason)}
              </span>
              <div className="flex-1 min-w-0">
                <div className="text-sm text-zinc-200 font-medium truncate">
                  {item.spec_id}
                </div>
                <div className="text-xs text-zinc-400 mt-0.5">{item.detail}</div>
              </div>
              <div className="text-right shrink-0">
                {item.latest_score != null ? (
                  <span
                    className={`text-xs ${
                      item.latest_score >= 0.9
                        ? "text-green-400"
                        : item.latest_score >= 0.7
                          ? "text-yellow-400"
                          : "text-red-400"
                    }`}
                  >
                    {(item.latest_score * 100).toFixed(0)}%
                  </span>
                ) : (
                  <span className="text-xs text-zinc-600">--</span>
                )}
                {item.broken_count > 0 && (
                  <div className="text-xs text-red-400 mt-0.5">
                    {item.broken_count} broken
                  </div>
                )}
                {item.staleness_days > 0 && (
                  <div className="text-xs text-yellow-400 mt-0.5">
                    {item.staleness_days}d stale
                  </div>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
