/**
 * MetaOptimizerPage — main container for the meta-optimizer system.
 *
 * Sub-tabs: Recommendations, Prompt Registry, Config Suggestions, Run History.
 * Dev-only feature for reviewing and applying AI-generated system improvements.
 */

import { useState } from "react";
import { ProgressTab } from "../components/meta-optimizer/ProgressTab";
import { RecommendationsTab } from "../components/meta-optimizer/RecommendationsTab";
import { PromptRegistryTab } from "../components/meta-optimizer/PromptRegistryTab";
import { ConfigSuggestionsTab } from "../components/meta-optimizer/ConfigSuggestionsTab";
import { RunHistoryTab } from "../components/meta-optimizer/RunHistoryTab";
import { EvalSpecsTab } from "../components/meta-optimizer/EvalSpecsTab";
import { RobustnessTab } from "../components/meta-optimizer/RobustnessTab";

type SubTab =
  | "progress"
  | "recommendations"
  | "prompts"
  | "config"
  | "history"
  | "eval_specs"
  | "robustness";

const TABS: { id: SubTab; label: string }[] = [
  { id: "progress", label: "Progress" },
  { id: "recommendations", label: "Recommendations" },
  { id: "prompts", label: "Prompt Registry" },
  { id: "config", label: "Config Suggestions" },
  { id: "history", label: "Run History" },
  { id: "eval_specs", label: "Eval Specs" },
  { id: "robustness", label: "Robustness" },
];

export function MetaOptimizerPage() {
  const [tab, setTab] = useState<SubTab>("progress");

  return (
    <div className="flex flex-col h-full">
      {/* Tab bar */}
      <div className="flex items-center gap-1 px-3 pt-3 pb-1 border-b border-zinc-800">
        {TABS.map((t) => (
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
          </button>
        ))}
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto">
        {tab === "progress" && <ProgressTab />}
        {tab === "recommendations" && <RecommendationsTab />}
        {tab === "prompts" && <PromptRegistryTab />}
        {tab === "config" && <ConfigSuggestionsTab />}
        {tab === "history" && <RunHistoryTab />}
        {tab === "eval_specs" && <EvalSpecsTab />}
        {tab === "robustness" && <RobustnessTab />}
      </div>
    </div>
  );
}
