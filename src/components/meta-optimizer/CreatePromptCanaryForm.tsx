/**
 * CreatePromptCanaryForm — Form for creating a new prompt template canary A/B test.
 */

import { useState } from "react";
import { useCreatePromptCanary } from "../../hooks/usePromptCanary";

interface CreatePromptCanaryFormProps {
  /** Pre-fill template ID when launching from a specific variant */
  defaultTemplateId?: string;
  /** Pre-fill baseline version */
  defaultBaselineVersion?: number;
  onCreated?: () => void;
  onClose?: () => void;
}

export function CreatePromptCanaryForm({
  defaultTemplateId = "",
  defaultBaselineVersion,
  onCreated,
  onClose,
}: CreatePromptCanaryFormProps) {
  const [templateId, setTemplateId] = useState(defaultTemplateId);
  const [baselineVersion, setBaselineVersion] = useState<number>(defaultBaselineVersion ?? 1);
  const [candidateVersion, setCandidateVersion] = useState<number>(
    (defaultBaselineVersion ?? 1) + 1,
  );
  const [trafficPct, setTrafficPct] = useState(10);

  const { createCanary, creating, error, success, reset } = useCreatePromptCanary();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!templateId.trim()) return;
    await createCanary(templateId.trim(), baselineVersion, candidateVersion, trafficPct);
    onCreated?.();
  };

  const handleReset = () => {
    reset();
    setTemplateId(defaultTemplateId);
    setBaselineVersion(defaultBaselineVersion ?? 1);
    setCandidateVersion((defaultBaselineVersion ?? 1) + 1);
    setTrafficPct(10);
  };

  return (
    <form
      onSubmit={handleSubmit}
      className="bg-zinc-900 border border-zinc-700 rounded-lg p-4 space-y-4"
    >
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium text-zinc-200">Create Prompt Canary A/B Test</h3>
        {onClose && (
          <button
            type="button"
            onClick={onClose}
            className="text-zinc-500 hover:text-zinc-300 text-sm"
          >
            Close
          </button>
        )}
      </div>

      <div className="space-y-3">
        <div>
          <label htmlFor="canary-template-id" className="block text-xs text-zinc-400 mb-1">
            Template ID
          </label>
          <input
            id="canary-template-id"
            type="text"
            value={templateId}
            onChange={(e) => setTemplateId(e.target.value)}
            placeholder="e.g. spec_analyst or variant UUID"
            className="w-full bg-zinc-800 text-zinc-200 text-sm px-3 py-1.5 rounded border border-zinc-700 focus:border-purple-600 focus:outline-none"
            required
          />
        </div>

        <div className="grid grid-cols-2 gap-3">
          <div>
            <label htmlFor="canary-baseline-ver" className="block text-xs text-zinc-400 mb-1">
              Baseline Version
            </label>
            <input
              id="canary-baseline-ver"
              type="number"
              min={1}
              value={baselineVersion}
              onChange={(e) => setBaselineVersion(parseInt(e.target.value, 10) || 1)}
              className="w-full bg-zinc-800 text-zinc-200 text-sm px-3 py-1.5 rounded border border-zinc-700 focus:border-purple-600 focus:outline-none"
              required
            />
          </div>
          <div>
            <label htmlFor="canary-candidate-ver" className="block text-xs text-zinc-400 mb-1">
              Candidate Version
            </label>
            <input
              id="canary-candidate-ver"
              type="number"
              min={1}
              value={candidateVersion}
              onChange={(e) => setCandidateVersion(parseInt(e.target.value, 10) || 1)}
              className="w-full bg-zinc-800 text-zinc-200 text-sm px-3 py-1.5 rounded border border-zinc-700 focus:border-purple-600 focus:outline-none"
              required
            />
          </div>
        </div>

        <div>
          <label htmlFor="canary-traffic-pct" className="block text-xs text-zinc-400 mb-1">
            Candidate Traffic: {trafficPct}%
          </label>
          <input
            id="canary-traffic-pct"
            type="range"
            min={1}
            max={100}
            value={trafficPct}
            onChange={(e) => setTrafficPct(parseInt(e.target.value, 10))}
            className="w-full accent-purple-500"
          />
          <div className="flex justify-between text-xs text-zinc-600 mt-0.5">
            <span>1%</span>
            <span>50%</span>
            <span>100%</span>
          </div>
        </div>
      </div>

      {error && (
        <div className="text-xs text-red-400 bg-red-900/20 border border-red-800/50 rounded px-3 py-2">
          {error}
        </div>
      )}

      {success && (
        <div className="text-xs text-green-400 bg-green-900/20 border border-green-800/50 rounded px-3 py-2">
          Canary A/B test created successfully.
        </div>
      )}

      <div className="flex items-center gap-2">
        <button
          type="submit"
          disabled={creating || !templateId.trim()}
          className="px-4 py-1.5 text-sm bg-purple-700 text-purple-100 rounded hover:bg-purple-600 disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {creating ? "Creating..." : "Create Canary"}
        </button>
        {success && (
          <button
            type="button"
            onClick={handleReset}
            className="px-3 py-1.5 text-sm bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600"
          >
            Create Another
          </button>
        )}
      </div>
    </form>
  );
}
