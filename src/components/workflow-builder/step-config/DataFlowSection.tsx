import { useState } from "react";
import { ChevronDown, ChevronRight, Plus, Trash2 } from "lucide-react";
import type { UnifiedStep, BaseStep } from "../../../types/unified-workflow";

interface DataFlowSectionProps {
  step: UnifiedStep;
  onUpdate: (updates: Partial<UnifiedStep>) => void;
  allSteps: { id: string; name: string }[];
}

export function DataFlowSection({ step, onUpdate, allSteps }: DataFlowSectionProps) {
  const [isOpen, setIsOpen] = useState(false);

  const baseStep = step as BaseStep;
  const inputs = (baseStep as unknown as Record<string, unknown>).inputs as
    | Record<string, string>
    | undefined;
  const extract = (baseStep as unknown as Record<string, unknown>).extract as
    | Record<string, string>
    | undefined;
  const dependsOn = (baseStep as unknown as Record<string, unknown>).depends_on as
    | string[]
    | undefined;
  const required = (baseStep as unknown as Record<string, unknown>).required as boolean | undefined;

  const inputEntries = inputs ? Object.entries(inputs) : [];
  const extractEntries = extract ? Object.entries(extract) : [];
  const dependsOnList = dependsOn || [];

  const handleAddInput = () => {
    const newInputs = { ...(inputs || {}), "": "" };
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleRemoveInput = (key: string) => {
    const newInputs = { ...(inputs || {}) };
    delete newInputs[key];
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleUpdateInputKey = (oldKey: string, newKey: string) => {
    const newInputs: Record<string, string> = {};
    for (const [k, v] of Object.entries(inputs || {})) {
      if (k === oldKey) {
        newInputs[newKey] = v;
      } else {
        newInputs[k] = v;
      }
    }
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleUpdateInputValue = (key: string, value: string) => {
    const newInputs = { ...(inputs || {}), [key]: value };
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleAddExtract = () => {
    const newExtract = { ...(extract || {}), "": "" };
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleRemoveExtract = (key: string) => {
    const newExtract = { ...(extract || {}) };
    delete newExtract[key];
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleUpdateExtractKey = (oldKey: string, newKey: string) => {
    const newExtract: Record<string, string> = {};
    for (const [k, v] of Object.entries(extract || {})) {
      if (k === oldKey) {
        newExtract[newKey] = v;
      } else {
        newExtract[k] = v;
      }
    }
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleUpdateExtractValue = (key: string, value: string) => {
    const newExtract = { ...(extract || {}), [key]: value };
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleToggleDependency = (stepId: string) => {
    const updated = dependsOnList.includes(stepId)
      ? dependsOnList.filter((id) => id !== stepId)
      : [...dependsOnList, stepId];
    onUpdate({ depends_on: updated } as Partial<UnifiedStep>);
  };

  const otherSteps = allSteps.filter((s) => s.id !== step.id);

  return (
    <div className="mt-4 pt-4 border-t border-zinc-700">
      <button
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        className="flex items-center gap-2 text-xs font-medium text-zinc-500 uppercase tracking-wider hover:text-zinc-400 transition-colors"
      >
        {isOpen ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        Data Flow
      </button>

      {isOpen && (
        <div className="mt-3 space-y-4">
          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">Inputs (from other steps)</h4>
            {inputEntries.map(([key, value], idx) => (
              <div key={`input-${key}-${idx}`} className="flex gap-2 mb-2">
                <input
                  id={`dataflow-input-key-${idx}`}
                  type="text"
                  value={key}
                  onChange={(e) => handleUpdateInputKey(key, e.target.value)}
                  placeholder="Variable name"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm focus:outline-hidden focus:ring-1 focus:ring-blue-500/50"
                />
                <input
                  id={`dataflow-input-value-${idx}`}
                  type="text"
                  value={value}
                  onChange={(e) => handleUpdateInputValue(key, e.target.value)}
                  placeholder="step_id.output_key"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm font-mono focus:outline-hidden focus:ring-1 focus:ring-blue-500/50"
                />
                <button
                  onClick={() => handleRemoveInput(key)}
                  className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </div>
            ))}
            <button
              onClick={handleAddInput}
              className="flex items-center gap-1 text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
            >
              <Plus className="w-3 h-3" />
              Add input
            </button>
          </div>

          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">
              Extract (from this step's output)
            </h4>
            {extractEntries.map(([key, value], idx) => (
              <div key={`extract-${key}-${idx}`} className="flex gap-2 mb-2">
                <input
                  id={`dataflow-extract-key-${idx}`}
                  type="text"
                  value={key}
                  onChange={(e) => handleUpdateExtractKey(key, e.target.value)}
                  placeholder="Output name"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm focus:outline-hidden focus:ring-1 focus:ring-blue-500/50"
                />
                <input
                  id={`dataflow-extract-value-${idx}`}
                  type="text"
                  value={value}
                  onChange={(e) => handleUpdateExtractValue(key, e.target.value)}
                  placeholder="$.data.result"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm font-mono focus:outline-hidden focus:ring-1 focus:ring-blue-500/50"
                />
                <button
                  onClick={() => handleRemoveExtract(key)}
                  className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </div>
            ))}
            <button
              onClick={handleAddExtract}
              className="flex items-center gap-1 text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
            >
              <Plus className="w-3 h-3" />
              Add extraction
            </button>
          </div>

          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">Dependencies</h4>
            {otherSteps.length === 0 ? (
              <p className="text-xs text-zinc-500 italic">No other steps available</p>
            ) : (
              <div className="space-y-1 max-h-32 overflow-y-auto border border-zinc-700 rounded-md p-2 bg-zinc-800/50">
                {otherSteps.map((s) => (
                  <label
                    key={s.id}
                    htmlFor={`dep-${s.id}`}
                    className="flex items-center gap-2 p-1 rounded hover:bg-zinc-700/50 cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      id={`dep-${s.id}`}
                      checked={dependsOnList.includes(s.id)}
                      onChange={() => handleToggleDependency(s.id)}
                      className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
                    />
                    <span className="text-sm text-zinc-300 truncate">{s.name}</span>
                  </label>
                ))}
              </div>
            )}
          </div>

          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="step_required"
              checked={required !== false}
              onChange={(e) => onUpdate({ required: e.target.checked } as Partial<UnifiedStep>)}
              className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
            />
            <label htmlFor="step_required" className="text-sm text-zinc-300">
              Required (workflow fails if this step fails)
            </label>
          </div>
        </div>
      )}
    </div>
  );
}
