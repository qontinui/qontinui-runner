/**
 * AutoresearchPage — main container for the autoresearch feature.
 *
 * Sub-tabs: Campaign, Results, Worktrees, and Per-Agent (dev only).
 */

import { useState } from "react";
import { CampaignTab } from "../components/autoresearch/CampaignTab";
import { ResultsTab } from "../components/autoresearch/ResultsTab";
import { WorktreeComparisonTab } from "../components/autoresearch/WorktreeComparisonTab";
import { PerAgentTab } from "../components/autoresearch/PerAgentTab";
import { ComparisonTab } from "../components/autoresearch/ComparisonTab";

type SubTab = "campaign" | "results" | "worktrees" | "comparison" | "per_agent";

const TABS: { id: SubTab; label: string; devOnly?: boolean }[] = [
  { id: "campaign", label: "Campaign" },
  { id: "results", label: "Results" },
  { id: "worktrees", label: "Worktrees" },
  { id: "comparison", label: "Comparison" },
  { id: "per_agent", label: "Per-Agent", devOnly: true },
];

export function AutoresearchPage() {
  const [tab, setTab] = useState<SubTab>("campaign");

  const visibleTabs = TABS.filter((t) => !t.devOnly || import.meta.env.DEV);

  return (
    <div className="flex flex-col h-full">
      {/* Tab bar */}
      <div className="flex items-center gap-1 px-3 pt-3 pb-1 border-b border-zinc-800">
        {visibleTabs.map((t) => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            className={`px-3 py-1.5 text-sm rounded-t-md transition-colors ${
              tab === t.id
                ? "bg-zinc-800 text-zinc-100 border border-b-0 border-zinc-700"
                : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800/50"
            }`}
          >
            {t.label}
            {t.devOnly && (
              <span className="ml-1 text-[10px] text-yellow-500/70">(dev)</span>
            )}
          </button>
        ))}
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto">
        {tab === "campaign" && <CampaignTab />}
        {tab === "results" && <ResultsTab />}
        {tab === "worktrees" && <WorktreeComparisonTab />}
        {tab === "comparison" && <ComparisonTab />}
        {tab === "per_agent" && <PerAgentTab />}
      </div>
    </div>
  );
}
