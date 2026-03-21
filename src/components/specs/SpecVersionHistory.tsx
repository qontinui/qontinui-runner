/**
 * SpecVersionHistory — Shows the version history for a given spec.
 *
 * Fetches compliance and accuracy records to reconstruct a timeline of
 * spec changes and their outcomes. Displays as a vertical timeline.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Clock, TrendingUp, TrendingDown, Minus, ChevronDown, ChevronRight } from "lucide-react";

interface VersionEntry {
  id: string;
  spec_id: string;
  timestamp: string;
  overall_score: number;
  assertions_passed: number;
  assertions_total: number;
  source: string;
}

interface SpecVersionHistoryProps {
  specId: string;
}

function scoreColor(score: number): string {
  if (score >= 0.9) return "text-green-400";
  if (score >= 0.7) return "text-yellow-400";
  return "text-red-400";
}

function scoreBg(score: number): string {
  if (score >= 0.9) return "bg-green-900/30 border-green-800";
  if (score >= 0.7) return "bg-yellow-900/30 border-yellow-800";
  return "bg-red-900/30 border-red-800";
}

function TrendIcon({ current, previous }: { current: number; previous: number | null }) {
  if (previous === null) return <Minus className="w-3 h-3 text-zinc-500" />;
  if (current > previous) return <TrendingUp className="w-3 h-3 text-green-400" />;
  if (current < previous) return <TrendingDown className="w-3 h-3 text-red-400" />;
  return <Minus className="w-3 h-3 text-zinc-500" />;
}

export function SpecVersionHistory({ specId }: SpecVersionHistoryProps) {
  const [entries, setEntries] = useState<VersionEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  useEffect(() => {
    setLoading(true);
    invoke<VersionEntry[]>("get_spec_compliance_history", { specId })
      .then((results) => {
        // Sort newest first
        const sorted = [...results].sort(
          (a, b) => new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime(),
        );
        setEntries(sorted);
      })
      .catch((err) => {
        console.error("[SpecVersionHistory] Failed to load history:", err);
        setEntries([]);
      })
      .finally(() => setLoading(false));
  }, [specId]);

  if (loading) {
    return (
      <div className="p-4 text-sm text-zinc-500">
        <Clock className="w-4 h-4 inline-block mr-1 animate-pulse" />
        Loading version history...
      </div>
    );
  }

  if (entries.length === 0) {
    return (
      <div className="p-4 space-y-2">
        <div className="text-sm text-zinc-400 font-medium">No version history yet</div>
        <div className="text-xs text-zinc-500">
          Run a spec verification workflow to start tracking compliance over time. Each run records
          assertion pass rates and overall scores.
        </div>
      </div>
    );
  }

  return (
    <div className="p-4 space-y-1">
      <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-3">
        Version History
      </h3>
      <div className="space-y-2">
        {entries.map((entry, idx) => {
          const previousScore = idx < entries.length - 1 ? entries[idx + 1].overall_score : null;
          const isExpanded = expandedId === entry.id;
          const date = new Date(entry.timestamp);

          return (
            <div
              key={entry.id}
              className="border border-zinc-700 rounded-lg bg-zinc-800/50 overflow-hidden"
            >
              <button
                onClick={() => setExpandedId(isExpanded ? null : entry.id)}
                className="w-full flex items-center gap-3 px-3 py-2 text-left hover:bg-zinc-700/30 transition-colors"
              >
                {isExpanded ? (
                  <ChevronDown className="w-3 h-3 text-zinc-500 shrink-0" />
                ) : (
                  <ChevronRight className="w-3 h-3 text-zinc-500 shrink-0" />
                )}

                <div className="flex-1 min-w-0">
                  <div className="text-xs text-zinc-400">
                    {date.toLocaleDateString()}{" "}
                    {date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                  </div>
                </div>

                <div className="flex items-center gap-2 shrink-0">
                  <TrendIcon current={entry.overall_score} previous={previousScore} />
                  <span
                    className={`px-2 py-0.5 rounded text-xs border ${scoreBg(entry.overall_score)} ${scoreColor(entry.overall_score)}`}
                  >
                    {(entry.overall_score * 100).toFixed(0)}%
                  </span>
                </div>
              </button>

              {isExpanded && (
                <div className="px-3 pb-3 pt-1 border-t border-zinc-700/50 space-y-2">
                  <div className="grid grid-cols-2 gap-2 text-xs">
                    <div className="text-zinc-500">
                      Assertions:{" "}
                      <span className="text-zinc-300">
                        {entry.assertions_passed}/{entry.assertions_total}
                      </span>
                    </div>
                    <div className="text-zinc-500">
                      Source: <span className="text-zinc-300">{entry.source || "workflow"}</span>
                    </div>
                  </div>
                  {previousScore !== null && (
                    <div className="text-xs text-zinc-500">
                      Change:{" "}
                      <span
                        className={
                          entry.overall_score > previousScore
                            ? "text-green-400"
                            : entry.overall_score < previousScore
                              ? "text-red-400"
                              : "text-zinc-400"
                        }
                      >
                        {entry.overall_score > previousScore ? "+" : ""}
                        {((entry.overall_score - previousScore) * 100).toFixed(1)}pp
                      </span>
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
