/**
 * PerAgentTab — orchestrator for per-agent autoresearch.
 *
 * Top-level container with agent selector and view switcher.
 */

import { useState } from "react";
import { AgentCampaignConfig } from "./AgentCampaignConfig";
import { AgentPerformanceView } from "./AgentPerformanceView";
import { AgentTraceComparison } from "./AgentTraceComparison";
import { AgentCampaignHistory } from "./AgentCampaignHistory";

export type AgentType = "spec_analyst" | "locator" | "implementer" | "verifier";
export type PerAgentView = "configure" | "monitor" | "results" | "history";

const AGENTS: { id: AgentType; label: string }[] = [
  { id: "spec_analyst", label: "Spec Analyst" },
  { id: "locator", label: "Locator" },
  { id: "implementer", label: "Implementer" },
  { id: "verifier", label: "Verifier" },
];

const VIEWS: { id: PerAgentView; label: string }[] = [
  { id: "configure", label: "Configure" },
  { id: "monitor", label: "Monitor" },
  { id: "results", label: "Results" },
  { id: "history", label: "History" },
];

export function PerAgentTab() {
  const [selectedAgent, setSelectedAgent] = useState<AgentType>("implementer");
  const [activeView, setActiveView] = useState<PerAgentView>("configure");

  return (
    <div className="flex flex-col h-full">
      {/* Selection bars */}
      <div className="px-4 pt-3 pb-2 space-y-2 border-b border-zinc-800">
        {/* Agent selector */}
        <div className="flex items-center gap-2">
          <span className="text-xs text-zinc-500 w-12 shrink-0">Agent:</span>
          <div className="flex gap-1">
            {AGENTS.map((a) => (
              <button
                key={a.id}
                onClick={() => setSelectedAgent(a.id)}
                className={`px-3 py-1 text-xs rounded-md transition-colors ${
                  selectedAgent === a.id
                    ? "bg-blue-600 text-white"
                    : "bg-zinc-800 text-zinc-400 hover:text-zinc-200 hover:bg-zinc-700"
                }`}
              >
                {a.label}
              </button>
            ))}
          </div>
        </div>

        {/* View selector */}
        <div className="flex items-center gap-2">
          <span className="text-xs text-zinc-500 w-12 shrink-0">View:</span>
          <div className="flex gap-1">
            {VIEWS.map((v) => (
              <button
                key={v.id}
                onClick={() => setActiveView(v.id)}
                className={`px-3 py-1 text-xs rounded-md transition-colors ${
                  activeView === v.id
                    ? "bg-zinc-700 text-zinc-100"
                    : "bg-zinc-800/50 text-zinc-400 hover:text-zinc-200 hover:bg-zinc-700/50"
                }`}
              >
                {v.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto">
        {activeView === "configure" && (
          <AgentCampaignConfig
            selectedAgent={selectedAgent}
            onStarted={() => setActiveView("monitor")}
          />
        )}
        {activeView === "monitor" && (
          <AgentPerformanceView selectedAgent={selectedAgent} live />
        )}
        {activeView === "results" && (
          <AgentTraceComparison selectedAgent={selectedAgent} />
        )}
        {activeView === "history" && (
          <AgentCampaignHistory
            selectedAgent={selectedAgent}
            onSelectCampaign={() => setActiveView("monitor")}
          />
        )}
      </div>
    </div>
  );
}
