/**
 * PipelineConfigPanel — configure multi-agent pipeline agents and strategy.
 *
 * Shown only when workflow_architecture === "multi_agent_pipeline".
 */

import { useState } from "react";
import type { MultiAgentPipelineConfig, PipelineAgentConfig } from "@/types";

const inputClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50";
const selectClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50";

const AGENT_ROLES = [
  { key: "spec_analyst" as const, label: "Spec Analyst" },
  { key: "locator" as const, label: "Locator" },
  { key: "implementer" as const, label: "Implementer" },
  { key: "verifier" as const, label: "Verifier" },
];

function AgentSection({
  label,
  config,
  onChange,
}: {
  label: string;
  config: PipelineAgentConfig | undefined;
  onChange: (c: PipelineAgentConfig) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const c = config ?? {};

  return (
    <div className="border border-zinc-700 rounded-md">
      <button
        type="button"
        className="w-full flex items-center justify-between px-3 py-2 text-sm font-medium text-zinc-300 hover:bg-zinc-800/50"
        onClick={() => setExpanded(!expanded)}
      >
        <span>{label}</span>
        <span className="text-xs text-zinc-500">{expanded ? "▲" : "▼"}</span>
      </button>
      {expanded && (
        <div className="px-3 pb-3 grid grid-cols-2 gap-2">
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Model</label>
            <input
              type="text"
              value={c.model ?? ""}
              onChange={(e) => onChange({ ...c, model: e.target.value || undefined })}
              placeholder="(default)"
              className={inputClass}
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Provider</label>
            <input
              type="text"
              value={c.provider ?? ""}
              onChange={(e) => onChange({ ...c, provider: e.target.value || undefined })}
              placeholder="(default)"
              className={inputClass}
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Max Tokens</label>
            <input
              type="number"
              value={c.max_tokens ?? ""}
              onChange={(e) =>
                onChange({ ...c, max_tokens: e.target.value ? Number(e.target.value) : undefined })
              }
              placeholder="(default)"
              className={inputClass}
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Temperature</label>
            <input
              type="number"
              step="0.1"
              min="0"
              max="2"
              value={c.temperature ?? ""}
              onChange={(e) =>
                onChange({
                  ...c,
                  temperature: e.target.value ? Number(e.target.value) : undefined,
                })
              }
              placeholder="(default)"
              className={inputClass}
            />
          </div>
        </div>
      )}
    </div>
  );
}

interface Props {
  config: Record<string, unknown> | undefined;
  onChange: (config: Record<string, unknown>) => void;
}

export function PipelineConfigPanel({ config, onChange }: Props) {
  const c = (config ?? {}) as Partial<MultiAgentPipelineConfig>;

  function update(patch: Partial<MultiAgentPipelineConfig>) {
    onChange({ ...c, ...patch } as unknown as Record<string, unknown>);
  }

  return (
    <div className="space-y-3">
      <div className="text-xs text-zinc-500 mb-1">
        Configure specialized agents and execution strategy for the multi-agent pipeline.
      </div>

      {/* Agent sections */}
      {AGENT_ROLES.map(({ key, label }) => (
        <AgentSection
          key={key}
          label={label}
          config={c[key]}
          onChange={(agentConfig) => update({ [key]: agentConfig })}
        />
      ))}

      {/* Strategy section */}
      <div className="border border-zinc-700 rounded-md p-3 space-y-2">
        <div className="text-sm font-medium text-zinc-300">Strategy</div>
        <div className="grid grid-cols-2 gap-2">
          <div>
            <label className="block text-xs text-zinc-500 mb-1">DAG Strategy</label>
            <select
              value={c.dag_strategy ?? "strict"}
              onChange={(e) =>
                update({ dag_strategy: e.target.value as MultiAgentPipelineConfig["dag_strategy"] })
              }
              className={selectClass}
            >
              <option value="strict">Strict</option>
              <option value="permissive">Permissive</option>
              <option value="flat">Flat</option>
            </select>
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Level Strategy</label>
            <select
              value={c.level_strategy ?? "level_by_level"}
              onChange={(e) =>
                update({
                  level_strategy: e.target.value as MultiAgentPipelineConfig["level_strategy"],
                })
              }
              className={selectClass}
            >
              <option value="level_by_level">Level-by-Level</option>
              <option value="greedy">Greedy</option>
              <option value="critical_first">Critical First</option>
            </select>
          </div>
        </div>
      </div>

      {/* Limits section */}
      <div className="border border-zinc-700 rounded-md p-3 space-y-2">
        <div className="text-sm font-medium text-zinc-300">Limits</div>
        <div className="grid grid-cols-3 gap-2">
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Max Parallel Implementers</label>
            <input
              type="number"
              min={1}
              max={10}
              value={c.max_parallel_implementers ?? 3}
              onChange={(e) =>
                update({
                  max_parallel_implementers: e.target.value ? Number(e.target.value) : undefined,
                })
              }
              className={inputClass}
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Max Retries/Subtree</label>
            <input
              type="number"
              min={0}
              max={10}
              value={c.max_retries_per_subtree ?? 2}
              onChange={(e) =>
                update({
                  max_retries_per_subtree: e.target.value ? Number(e.target.value) : undefined,
                })
              }
              className={inputClass}
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Max Total Iterations</label>
            <input
              type="number"
              min={1}
              max={100}
              value={c.max_total_iterations ?? 20}
              onChange={(e) =>
                update({
                  max_total_iterations: e.target.value ? Number(e.target.value) : undefined,
                })
              }
              className={inputClass}
            />
          </div>
        </div>
        <div className="flex items-center gap-2 pt-1">
          <input
            type="checkbox"
            id="integration_verification"
            checked={c.integration_verification ?? true}
            onChange={(e) => update({ integration_verification: e.target.checked })}
            className="rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
          />
          <label htmlFor="integration_verification" className="text-xs text-zinc-400">
            Integration verification after all subtrees
          </label>
        </div>
      </div>
    </div>
  );
}
