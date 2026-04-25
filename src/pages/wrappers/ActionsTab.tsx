/**
 * ActionsTab — table of the wrapper's actions with inline "Try it" forms.
 *
 * For each `ActionDescriptor`, shows:
 *   - id, optional description (description isn't carried in the manifest
 *     today; we surface it if the runner ever adds it).
 *   - "Try it" button. Clicking expands an inline panel with `ParamSchemaForm`
 *     for that action. Submitting the form calls
 *     `POST /wrappers/:id/dispatch` and pretty-prints the result/error JSON
 *     beneath the form.
 */

import { useState } from "react";
import { ChevronDown, ChevronRight, Lock, Loader2 } from "lucide-react";
import { ParamSchemaForm } from "@/components/wrappers/ParamSchemaForm";
import { dispatchAction } from "@/lib/wrappers/api";
import type { ActionDescriptor, DispatchResult } from "@/lib/wrappers/types";

export interface ActionsTabProps {
  wrapperId: string;
  actions: ActionDescriptor[];
}

interface DispatchState {
  status: "idle" | "running" | "success" | "error";
  result?: unknown;
  error?: string;
  startedAt?: number;
  finishedAt?: number;
}

export function ActionsTab({ wrapperId, actions }: ActionsTabProps) {
  const [openId, setOpenId] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState<Record<string, boolean>>({});
  const [dispatch, setDispatch] = useState<Record<string, DispatchState>>({});

  if (!actions || actions.length === 0) {
    return (
      <div className="rounded-xl border border-dashed border-border bg-card/40 p-8 text-center">
        <p className="text-sm text-muted-foreground">This wrapper has no actions registered.</p>
      </div>
    );
  }

  const toggle = (id: string) => {
    setOpenId((prev) => (prev === id ? null : id));
  };

  const handleSubmit = async (action: ActionDescriptor, params: Record<string, unknown>) => {
    setSubmitting((p) => ({ ...p, [action.id]: true }));
    setDispatch((p) => ({
      ...p,
      [action.id]: { status: "running", startedAt: Date.now() },
    }));
    try {
      const res: DispatchResult = await dispatchAction(wrapperId, action.id, params);
      const finishedAt = Date.now();
      if (res.error) {
        setDispatch((p) => ({
          ...p,
          [action.id]: {
            status: "error",
            error: res.error,
            startedAt: p[action.id]?.startedAt,
            finishedAt,
          },
        }));
      } else {
        setDispatch((p) => ({
          ...p,
          [action.id]: {
            status: "success",
            result: res.result,
            startedAt: p[action.id]?.startedAt,
            finishedAt,
          },
        }));
      }
    } catch (err) {
      setDispatch((p) => ({
        ...p,
        [action.id]: {
          status: "error",
          error: err instanceof Error ? err.message : String(err),
          startedAt: p[action.id]?.startedAt,
          finishedAt: Date.now(),
        },
      }));
    } finally {
      setSubmitting((p) => {
        const n = { ...p };
        delete n[action.id];
        return n;
      });
    }
  };

  return (
    <div className="rounded-xl border border-border bg-card overflow-hidden">
      <div className="px-5 py-3 border-b border-border flex items-center justify-between">
        <h3 className="text-xs font-semibold uppercase tracking-widest text-muted-foreground">
          Actions
        </h3>
        <span className="text-xs text-muted-foreground">{actions.length}</span>
      </div>
      <ul>
        {actions.map((action, idx) => {
          const isOpen = openId === action.id;
          const isSubmitting = Boolean(submitting[action.id]);
          const last = dispatch[action.id];
          return (
            <li key={action.id} className={idx === 0 ? "" : "border-t border-border"}>
              <button
                onClick={() => toggle(action.id)}
                className="w-full flex items-center gap-3 px-5 py-3 text-left hover:bg-muted/20 transition-colors"
              >
                {isOpen ? (
                  <ChevronDown className="w-3.5 h-3.5 text-muted-foreground" />
                ) : (
                  <ChevronRight className="w-3.5 h-3.5 text-muted-foreground" />
                )}
                <code className="text-sm font-mono text-foreground">{action.id}</code>
                {action.exclusive && (
                  <span className="inline-flex items-center gap-1 rounded-full border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-[10px] font-medium text-amber-400 uppercase tracking-wider">
                    <Lock className="w-2.5 h-2.5" />
                    exclusive
                  </span>
                )}
                {action.description && (
                  <span className="ml-2 flex-1 text-xs text-muted-foreground truncate">
                    {action.description}
                  </span>
                )}
                <span className="ml-auto text-xs text-cyan-400 font-medium">
                  {isOpen ? "Close" : "Try it"}
                </span>
              </button>

              {isOpen && (
                <div className="border-t border-border bg-muted/10 px-5 py-4 space-y-4">
                  <ParamSchemaForm
                    schema={action.paramSchema}
                    onSubmit={(params) => handleSubmit(action, params)}
                    submitLabel="Run"
                    submitting={isSubmitting}
                    compact
                  />

                  {last && last.status !== "idle" && (
                    <div className="space-y-2">
                      <div className="flex items-center gap-2 text-xs">
                        {last.status === "running" && (
                          <>
                            <Loader2 className="w-3.5 h-3.5 animate-spin text-cyan-400" />
                            <span className="text-cyan-400 font-medium">Running…</span>
                          </>
                        )}
                        {last.status === "success" && (
                          <span className="text-emerald-400 font-medium uppercase tracking-wider">
                            Success
                          </span>
                        )}
                        {last.status === "error" && (
                          <span className="text-red-400 font-medium uppercase tracking-wider">
                            Error
                          </span>
                        )}
                        {last.startedAt && last.finishedAt && (
                          <span className="text-muted-foreground">
                            {(last.finishedAt - last.startedAt).toLocaleString()} ms
                          </span>
                        )}
                      </div>
                      {last.status === "error" && last.error && (
                        <pre className="rounded-md border border-red-500/30 bg-red-500/5 p-3 text-[11px] font-mono text-red-300 whitespace-pre-wrap break-words">
                          {last.error}
                        </pre>
                      )}
                      {last.status === "success" && (
                        <pre className="rounded-md border border-border bg-background p-3 text-[11px] font-mono text-foreground whitespace-pre-wrap break-words max-h-80 overflow-auto">
                          {JSON.stringify(last.result, null, 2)}
                        </pre>
                      )}
                    </div>
                  )}
                </div>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
