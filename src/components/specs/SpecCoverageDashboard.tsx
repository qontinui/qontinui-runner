/**
 * SpecCoverageDashboard -- Cross-references all spec files on disk with their
 * compliance scores from the database. Shows a summary header and a sortable
 * table of every spec with group/assertion counts, compliance score, trend,
 * and last-run date.
 */

import { useState, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";

interface SpecInventoryEntry {
  spec_id: string;
  file_name: string;
  group_count: number;
  assertion_count: number;
  latest_compliance_score: number | null;
  last_run_at: string | null;
  trend: string | null;
}

type SortField = "score" | "name" | "assertions" | "last_run";

function scoreColor(score: number): string {
  if (score >= 0.9) return "text-green-400";
  if (score >= 0.7) return "text-yellow-400";
  return "text-red-400";
}

function scoreBadgeClass(score: number | null): string {
  if (score === null) return "bg-zinc-700 text-zinc-500 border-zinc-600";
  if (score >= 0.9) return "bg-green-900/30 text-green-400 border-green-800";
  if (score >= 0.7) return "bg-yellow-900/30 text-yellow-400 border-yellow-800";
  return "bg-red-900/30 text-red-400 border-red-800";
}

function trendIndicator(trend: string | null): { icon: string; color: string } {
  if (trend === "improving") return { icon: "\u2191", color: "text-green-400" };
  if (trend === "declining") return { icon: "\u2193", color: "text-red-400" };
  if (trend === "stable") return { icon: "\u2192", color: "text-zinc-500" };
  return { icon: "--", color: "text-zinc-600" };
}

function formatDate(iso: string | null): string {
  if (!iso) return "--";
  try {
    const d = new Date(iso);
    return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
  } catch {
    return iso;
  }
}

export function SpecCoverageDashboard() {
  const [inventory, setInventory] = useState<SpecInventoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [sortField, setSortField] = useState<SortField>("score");
  const [sortAsc, setSortAsc] = useState(true);

  useEffect(() => {
    invoke<SpecInventoryEntry[]>("get_spec_inventory", { specsDir: "src/specs" })
      .then(setInventory)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  const sorted = useMemo(() => {
    const items = [...inventory];
    items.sort((a, b) => {
      let cmp = 0;
      switch (sortField) {
        case "score": {
          const sa = a.latest_compliance_score ?? -1;
          const sb = b.latest_compliance_score ?? -1;
          cmp = sa - sb;
          break;
        }
        case "name":
          cmp = a.spec_id.localeCompare(b.spec_id);
          break;
        case "assertions":
          cmp = a.assertion_count - b.assertion_count;
          break;
        case "last_run": {
          const da = a.last_run_at ?? "";
          const db = b.last_run_at ?? "";
          cmp = da.localeCompare(db);
          break;
        }
      }
      return sortAsc ? cmp : -cmp;
    });
    return items;
  }, [inventory, sortField, sortAsc]);

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortAsc(!sortAsc);
    } else {
      setSortField(field);
      setSortAsc(field === "score"); // score defaults ascending (worst first)
    }
  };

  const sortIndicator = (field: SortField) => {
    if (sortField !== field) return "";
    return sortAsc ? " \u25B2" : " \u25BC";
  };

  // Summary stats
  const totalSpecs = inventory.length;
  const specsWithData = inventory.filter((s) => s.latest_compliance_score !== null).length;
  const avgScore =
    specsWithData > 0
      ? inventory.reduce((sum, s) => sum + (s.latest_compliance_score ?? 0), 0) / specsWithData
      : 0;
  const totalAssertions = inventory.reduce((sum, s) => sum + s.assertion_count, 0);
  const totalGroups = inventory.reduce((sum, s) => sum + s.group_count, 0);

  if (loading) {
    return <div className="p-4 text-zinc-500 text-sm">Loading spec inventory...</div>;
  }

  if (error) {
    return <div className="p-4 text-red-400 text-sm">Failed to load spec inventory: {error}</div>;
  }

  return (
    <div className="space-y-4">
      {/* Summary header */}
      <div className="bg-zinc-800 rounded-lg p-4 border border-zinc-700">
        <div className="text-xs text-zinc-500 uppercase tracking-wide mb-3">
          Spec Coverage Summary
        </div>
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
          <div>
            <div className="text-2xl font-bold text-zinc-100">
              {specsWithData}/{totalSpecs}
            </div>
            <div className="text-xs text-zinc-500">specs have compliance data</div>
          </div>
          <div>
            <div
              className={`text-2xl font-bold ${specsWithData > 0 ? scoreColor(avgScore) : "text-zinc-600"}`}
            >
              {specsWithData > 0 ? `${(avgScore * 100).toFixed(1)}%` : "--"}
            </div>
            <div className="text-xs text-zinc-500">average compliance score</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-zinc-100">{totalGroups}</div>
            <div className="text-xs text-zinc-500">total assertion groups</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-zinc-100">{totalAssertions}</div>
            <div className="text-xs text-zinc-500">total assertions</div>
          </div>
        </div>
      </div>

      {/* Spec inventory table */}
      <div className="bg-zinc-800 rounded-lg border border-zinc-700 overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-zinc-500 text-xs border-b border-zinc-700">
              <th
                className="text-left py-2 px-3 cursor-pointer hover:text-zinc-300 select-none"
                onClick={() => handleSort("name")}
              >
                Spec{sortIndicator("name")}
              </th>
              <th className="text-right py-2 px-3">Groups</th>
              <th
                className="text-right py-2 px-3 cursor-pointer hover:text-zinc-300 select-none"
                onClick={() => handleSort("assertions")}
              >
                Assertions{sortIndicator("assertions")}
              </th>
              <th
                className="text-right py-2 px-3 cursor-pointer hover:text-zinc-300 select-none"
                onClick={() => handleSort("score")}
              >
                Compliance{sortIndicator("score")}
              </th>
              <th className="text-center py-2 px-3">Trend</th>
              <th
                className="text-right py-2 px-3 cursor-pointer hover:text-zinc-300 select-none"
                onClick={() => handleSort("last_run")}
              >
                Last Run{sortIndicator("last_run")}
              </th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((entry) => {
              const trend = trendIndicator(entry.trend);
              return (
                <tr
                  key={entry.spec_id}
                  className="border-b border-zinc-800/50 hover:bg-zinc-700/30"
                >
                  <td
                    className="py-2 px-3 text-zinc-300 truncate max-w-[240px]"
                    title={entry.file_name}
                  >
                    {entry.spec_id}
                  </td>
                  <td className="py-2 px-3 text-right text-zinc-400">{entry.group_count}</td>
                  <td className="py-2 px-3 text-right text-zinc-400">{entry.assertion_count}</td>
                  <td className="py-2 px-3 text-right">
                    <span
                      className={`px-2 py-0.5 rounded text-xs border ${scoreBadgeClass(entry.latest_compliance_score)}`}
                    >
                      {entry.latest_compliance_score !== null
                        ? `${(entry.latest_compliance_score * 100).toFixed(1)}%`
                        : "No data"}
                    </span>
                  </td>
                  <td className={`py-2 px-3 text-center ${trend.color}`}>{trend.icon}</td>
                  <td className="py-2 px-3 text-right text-zinc-400 text-xs">
                    {formatDate(entry.last_run_at)}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
