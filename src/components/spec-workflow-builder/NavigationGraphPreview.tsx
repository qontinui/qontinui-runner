/**
 * NavigationGraphPreview
 *
 * Simple visualization of navigation states and transitions.
 * Uses a list-based layout rather than ReactFlow for simplicity.
 */

import { ArrowRight, Circle, GitBranch } from "lucide-react";
import type { NonVisualState, NonVisualTransition } from "./types";

interface NavigationGraphPreviewProps {
  states: NonVisualState[];
  transitions: NonVisualTransition[];
}

export function NavigationGraphPreview({ states, transitions }: NavigationGraphPreviewProps) {
  if (states.length === 0) return null;

  return (
    <div className="p-4 space-y-3">
      <div className="flex items-center gap-2 text-sm font-medium text-neutral-200">
        <GitBranch className="w-4 h-4 text-emerald-400" />
        Navigation Flow ({states.length} states, {transitions.length} transitions)
      </div>

      <div className="space-y-2">
        {states.map((state) => {
          const outgoing = transitions.filter((t) => t.fromStateId === state.id);
          return (
            <div
              key={state.id}
              className="p-3 bg-neutral-800/50 rounded-lg border border-neutral-700"
            >
              <div className="flex items-center gap-2">
                <Circle className="w-3 h-3 text-emerald-400 fill-emerald-400" />
                <span className="text-sm font-medium text-neutral-200">{state.name}</span>
                {state.pageUrl && (
                  <span className="text-xs text-blue-400 truncate">{state.pageUrl}</span>
                )}
                <span className="text-xs text-neutral-500 ml-auto">
                  {state.elementIds.length} elements
                </span>
              </div>

              {outgoing.length > 0 && (
                <div className="mt-2 pl-5 space-y-1">
                  {outgoing.map((t) => {
                    const target = states.find((s) => s.id === t.toStateId);
                    return (
                      <div key={t.id} className="flex items-center gap-2 text-xs">
                        <ArrowRight className="w-3 h-3 text-neutral-500" />
                        <span className="text-yellow-400">{t.triggerAction}</span>
                        <span className="text-neutral-400">
                          {t.triggerLabel || t.triggerElementId}
                        </span>
                        <ArrowRight className="w-2 h-2 text-neutral-600" />
                        <span className="text-blue-400">{target?.name || t.toStateId}</span>
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
