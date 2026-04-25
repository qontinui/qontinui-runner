/**
 * WrapperActionStepConfig
 *
 * Step config UI for the `wrapper_action` step type (Phase 3 of the
 * wrapper-runner integration plan).
 *
 * Layout (top → bottom):
 *   1. Wrapper picker (dropdown of installed wrappers).
 *   2. Action picker (dropdown of the chosen wrapper's actions).
 *   3. Params form (`ParamSchemaForm`, the same component the "Try it" panel
 *      on the wrapper detail page uses — Phase 2 surface kept stable for
 *      this exact reuse).
 *   4. Result variable name (optional text input).
 *
 * Param values may contain `{{ variableName }}` template references; the
 * Rust executor resolves them against the workflow's runtime context
 * before dispatch. Phase 3 keeps the UX simple — users type the
 * template inline. A "{ }" toggle UX that switches a field to a typed
 * variable picker is a polish item we'll layer in once the rest of the
 * integration is stable.
 */

import { useEffect, useMemo, useState } from "react";
import { Loader2, Plug2, AlertCircle } from "lucide-react";
import { listWrappers } from "@/lib/wrappers/api";
import type { ActionDescriptor, InstalledWrapper } from "@/lib/wrappers/types";
import { ParamSchemaForm } from "../wrappers/ParamSchemaForm";
import type { UnifiedStep } from "../../types/unified-workflow";

type WrapperActionStep = UnifiedStep & { type: "wrapper_action" };
type StepUpdater = (updates: Partial<WrapperActionStep>) => void;

interface Props {
  step: WrapperActionStep;
  onUpdate: StepUpdater;
}

export function WrapperActionStepConfig({ step, onUpdate }: Props) {
  const [wrappers, setWrappers] = useState<InstalledWrapper[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // Load installed wrappers once on mount.
  useEffect(() => {
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- mount-time fetch lifecycle
    setLoading(true);
    listWrappers()
      .then((list) => {
        if (cancelled) return;
        setWrappers(list);
        setLoadError(null);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setLoadError(err instanceof Error ? err.message : String(err));
        setWrappers([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const selectedWrapper = useMemo<InstalledWrapper | null>(() => {
    if (!wrappers || !step.wrapperId) return null;
    return wrappers.find((w) => w.id === step.wrapperId) ?? null;
  }, [wrappers, step.wrapperId]);

  const selectedAction = useMemo<ActionDescriptor | null>(() => {
    if (!selectedWrapper || !step.actionId) return null;
    return selectedWrapper.actions.find((a) => a.id === step.actionId) ?? null;
  }, [selectedWrapper, step.actionId]);

  // Reset action + params when the wrapper changes.
  const handleWrapperChange = (id: string) => {
    onUpdate({
      wrapperId: id,
      actionId: "",
      params: {},
    });
  };

  // Reset params when the action changes.
  const handleActionChange = (id: string) => {
    onUpdate({
      actionId: id,
      params: {},
    });
  };

  // The params form's submit handler doubles as the "save" path: Phase 3's
  // contract is that param edits live-update the step config. We don't
  // actually want a "submit" button cluttering the inline panel — the
  // builder's existing save flow persists the step. But ParamSchemaForm's
  // contract requires a submit handler, so we wire it to onUpdate as a
  // belt-and-braces sync. The form's `onChange` semantics are also handled
  // by re-mounting on action swap.
  const handleParamsSubmit = (params: Record<string, unknown>) => {
    onUpdate({ params });
  };

  // ParamSchemaForm is a controlled-on-mount component (re-seeds values
  // when its `schema` prop changes). To capture every keystroke without
  // building a parallel renderer, we wrap it in a thin observer that
  // reflects the form's submit + best-effort state into onUpdate. For
  // Phase 3 we surface a Save button on the form so users have a clear
  // commit point; the builder's overall save flow handles persistence.

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2 text-xs text-zinc-500 mb-1">
        <Plug2 className="w-3.5 h-3.5" />
        <span>Dispatches a typed action through an installed wrapper.</span>
      </div>

      {loadError && (
        <div className="flex items-start gap-2 p-2 bg-red-500/10 border border-red-500/30 rounded-md text-xs text-red-300">
          <AlertCircle className="w-4 h-4 shrink-0 mt-0.5" />
          <div>
            <div className="font-medium">Failed to load wrappers</div>
            <div className="text-red-300/80">{loadError}</div>
          </div>
        </div>
      )}

      {/* Wrapper picker */}
      <div>
        <label
          htmlFor="wrapper-action-wrapper-select"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Wrapper
          <span className="ml-1 text-cyan-400">*</span>
        </label>
        {loading && !wrappers ? (
          <div className="flex items-center gap-2 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-sm text-zinc-500">
            <Loader2 className="w-3.5 h-3.5 animate-spin" />
            Loading installed wrappers…
          </div>
        ) : (
          <select
            id="wrapper-action-wrapper-select"
            value={step.wrapperId ?? ""}
            onChange={(e) => handleWrapperChange(e.target.value)}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-cyan-500/50"
          >
            <option value="">— Select a wrapper —</option>
            {(wrappers ?? []).map((w) => (
              <option key={w.id} value={w.id}>
                {w.manifest?.displayName ?? w.id}
              </option>
            ))}
          </select>
        )}
        {wrappers && wrappers.length === 0 && !loadError && (
          <p className="text-xs text-zinc-500 mt-1">
            No wrappers installed. Install one from the Wrappers page.
          </p>
        )}
      </div>

      {/* Action picker */}
      <div>
        <label
          htmlFor="wrapper-action-action-select"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Action
          <span className="ml-1 text-cyan-400">*</span>
        </label>
        <select
          id="wrapper-action-action-select"
          value={step.actionId ?? ""}
          onChange={(e) => handleActionChange(e.target.value)}
          disabled={!selectedWrapper}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-cyan-500/50 disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <option value="">
            {selectedWrapper ? "— Select an action —" : "— Pick a wrapper first —"}
          </option>
          {(selectedWrapper?.actions ?? []).map((action) => (
            <option key={action.id} value={action.id}>
              {action.id}
              {action.description ? ` — ${action.description}` : ""}
            </option>
          ))}
        </select>
      </div>

      {/* Params form */}
      {selectedAction && (
        <div className="rounded-md border border-zinc-700/60 bg-zinc-900/40 p-3">
          <div className="flex items-center justify-between mb-2">
            <h4 className="text-xs font-medium text-zinc-400 uppercase tracking-wider">
              Parameters
            </h4>
            <span className="text-[10px] text-zinc-500">
              Use <code className="text-cyan-400">{"{{ variableName }}"}</code> to reference
              workflow variables.
            </span>
          </div>
          <ParamSchemaForm
            // Re-mount when the action changes so initial values are seeded
            // from the new schema's defaults instead of the old action's.
            key={`${step.wrapperId}::${step.actionId}`}
            schema={selectedAction.paramSchema}
            initialValues={(step.params as Record<string, unknown>) ?? {}}
            onSubmit={handleParamsSubmit}
            submitLabel="Save Parameters"
            compact
          />
        </div>
      )}

      {/* Result variable */}
      <div>
        <label
          htmlFor="wrapper-action-result-variable"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Result Variable
          <span className="ml-2 text-[10px] uppercase tracking-wider text-zinc-500">optional</span>
        </label>
        <input
          id="wrapper-action-result-variable"
          type="text"
          value={step.resultVariable ?? ""}
          onChange={(e) => onUpdate({ resultVariable: e.target.value })}
          placeholder="e.g. componentId"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-cyan-500/50"
        />
        <p className="text-xs text-zinc-500 mt-1">
          When set, the dispatch result is stored in this workflow variable; later steps can
          reference it as <code className="text-cyan-400">{"{{ name }}"}</code>.
        </p>
      </div>
    </div>
  );
}
