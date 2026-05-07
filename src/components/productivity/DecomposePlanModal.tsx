/**
 * DecomposePlanModal
 *
 * Modal triggered from the Productivity tab's Plans sub-view. Accepts a
 * plan markdown path, calls the `decompose_plan` Tauri command (Phase 3
 * of `productivity-stack-product-readiness`), and shows the resulting
 * plan ID + task count on success.
 *
 * The Tauri command:
 *   1. Reads the plan + computes a SHA-256 hash for idempotency.
 *   2. Calls the active LLM provider via `OneshotLlm` to identify
 *      phases/tasks/claims/dependencies.
 *   3. POSTs to `/plans/decompose` (the existing transactional inserter).
 *   4. Stamps the plan with a `<!-- decomposed: ... -->` marker.
 *
 * When no LLM provider is configured, the command returns an error
 * string starting with "Configure an LLM provider…" — we surface that
 * verbatim with a Settings → AI link.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { ClipboardList, Loader2, X, CheckCircle2, AlertCircle, Settings } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { decomposePlan, type DecomposeResult } from "./api";

export interface DecomposePlanModalProps {
  /** Whether the modal is open. */
  open: boolean;
  /** Called when the user dismisses the modal. */
  onClose: () => void;
  /**
   * Optional initial plan path. Falls back to the most-recently-modified
   * `*.md` under `D:/qontinui-root/plans/` when the parent has resolved
   * one; otherwise the field starts empty.
   */
  initialPlanPath?: string | null;
  /**
   * Called after a successful decomposition. The parent may use it to
   * refresh the plan list / select the freshly-decomposed plan.
   */
  onSuccess?: (result: DecomposeResult) => void;
}

const PROVIDER_HINT_PREFIX = "Configure an LLM provider";

interface FormState {
  planPath: string;
  busy: boolean;
  result: DecomposeResult | null;
  error: string | null;
}

const INITIAL: FormState = {
  planPath: "",
  busy: false,
  result: null,
  error: null,
};

export function DecomposePlanModal({
  open,
  onClose,
  initialPlanPath,
  onSuccess,
}: DecomposePlanModalProps) {
  const [state, setState] = useState<FormState>(INITIAL);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // Seed the path field whenever the modal opens — picks up parent's
  // suggestion (e.g. most-recently-modified plan). Reset busy/result/
  // error so a re-open after a previous run starts fresh. setState fires
  // once per closed→open transition (guarded by !open early return), not
  // on every render — so the cascading-render concern the rule guards
  // against doesn't apply here. The render-phase prevOpenRef alternative
  // trips react-hooks/refs, which is worse for the same outcome.
  useEffect(() => {
    if (!open) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setState({
      planPath: initialPlanPath ?? "",
      busy: false,
      result: null,
      error: null,
    });
    const t = setTimeout(() => inputRef.current?.focus(), 50);
    return () => clearTimeout(t);
  }, [open, initialPlanPath]);

  // Esc-to-close.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, onClose]);

  const handleSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      const trimmed = state.planPath.trim();
      if (!trimmed || state.busy) return;
      setState((s) => ({ ...s, busy: true, error: null, result: null }));
      try {
        const result = await decomposePlan(trimmed);
        setState((s) => ({ ...s, busy: false, result }));
        onSuccess?.(result);
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, busy: false, error: msg }));
      }
    },
    [state.planPath, state.busy, onSuccess],
  );

  if (!open) return null;

  const isProviderHint = state.error?.startsWith(PROVIDER_HINT_PREFIX);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6"
      role="dialog"
      aria-modal="true"
      aria-label="Decompose plan"
    >
      <div className="absolute inset-0 bg-black/40" onClick={onClose} aria-hidden="true" />
      <div
        className="relative w-full max-w-xl rounded-lg border border-border bg-background shadow-2xl overflow-hidden"
        data-ui-bridge-id="productivity.decompose-plan-modal"
      >
        {/* Header */}
        <div className="flex items-center justify-between gap-2 px-4 py-3 border-b border-border bg-card/40">
          <div className="flex items-center gap-2">
            <ClipboardList className="w-4 h-4 text-primary" />
            <h2 className="text-sm font-semibold text-foreground">Decompose Plan</h2>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-muted/30"
            aria-label="Close"
            data-ui-bridge-id="productivity.decompose-plan-modal-close"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Body */}
        <form onSubmit={handleSubmit} className="p-4 space-y-3">
          <p className="text-xs text-muted-foreground">
            Reads a plan markdown file, asks the active LLM provider to identify phases and tasks,
            and inserts them into the productivity stack with file-claim conflict detection.
          </p>

          <label className="block">
            <span className="block text-xs font-medium text-foreground mb-1">
              Plan markdown path
            </span>
            <input
              ref={inputRef}
              type="text"
              value={state.planPath}
              onChange={(e) => setState((s) => ({ ...s, planPath: e.target.value }))}
              placeholder="D:/qontinui-root/plans/my-plan.md"
              className="w-full px-2 py-1.5 rounded border border-border bg-background text-sm text-foreground font-mono placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-primary/40"
              disabled={state.busy}
              data-ui-bridge-id="productivity.decompose-plan-modal-path"
            />
          </label>

          {state.error && (
            <ResultBanner
              tone="error"
              Icon={AlertCircle}
              title={isProviderHint ? "LLM provider not configured" : "Decomposition failed"}
              detail={state.error}
              extra={
                isProviderHint ? (
                  <button
                    type="button"
                    onClick={() => {
                      // The Settings tab is reachable by clicking its
                      // header tab — dispatch the same event the menu
                      // bar uses. If the runner doesn't recognise the
                      // event, the user just sees the error and can
                      // navigate manually.
                      window.dispatchEvent(
                        new CustomEvent("navigate-to-settings", { detail: { section: "ai" } }),
                      );
                      onClose();
                    }}
                    className="mt-2 inline-flex items-center gap-1 text-xs text-primary hover:underline"
                    data-ui-bridge-id="productivity.decompose-plan-modal-open-settings"
                  >
                    <Settings className="w-3 h-3" />
                    Open Settings → AI
                  </button>
                ) : null
              }
            />
          )}

          {state.result && (
            <ResultBanner
              tone="success"
              Icon={CheckCircle2}
              title={
                state.result.idempotentSkip
                  ? "Plan already decomposed (no changes)"
                  : `Decomposed into ${state.result.taskCount} task${
                      state.result.taskCount === 1 ? "" : "s"
                    }`
              }
              detail={`Plan ID: ${state.result.planId}`}
              extra={
                <p className="mt-1 text-[11px] text-muted-foreground/80 font-mono break-all">
                  hash: {state.result.versionHash}
                </p>
              }
            />
          )}

          <div className="flex items-center justify-end gap-2 pt-2 border-t border-border">
            <button
              type="button"
              onClick={onClose}
              disabled={state.busy}
              className="px-3 py-1.5 rounded text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 disabled:opacity-50"
            >
              {state.result ? "Done" : "Cancel"}
            </button>
            <button
              type="submit"
              disabled={state.busy || !state.planPath.trim()}
              className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded bg-primary text-primary-foreground text-xs font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-bridge-id="productivity.decompose-plan-modal-submit"
            >
              {state.busy ? (
                <>
                  <Loader2 className="w-3 h-3 animate-spin" />
                  Decomposing…
                </>
              ) : (
                <>
                  <ClipboardList className="w-3 h-3" />
                  Decompose
                </>
              )}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

interface ResultBannerProps {
  tone: "success" | "error";
  Icon: LucideIcon;
  title: string;
  detail: string;
  extra?: React.ReactNode;
}

function ResultBanner({ tone, Icon, title, detail, extra }: ResultBannerProps) {
  const palette =
    tone === "success"
      ? "bg-emerald-500/10 border-emerald-500/40 text-emerald-300"
      : "bg-red-500/10 border-red-500/40 text-red-300";
  const iconPalette = tone === "success" ? "text-emerald-400" : "text-red-400";
  return (
    <div
      className={`flex items-start gap-2 p-2.5 rounded border text-xs ${palette}`}
      data-ui-bridge-id={`productivity.decompose-plan-modal-${tone}-banner`}
      role={tone === "error" ? "alert" : "status"}
    >
      <Icon className={`w-3.5 h-3.5 mt-0.5 shrink-0 ${iconPalette}`} />
      <div className="min-w-0 flex-1">
        <p className="font-medium">{title}</p>
        <p className="mt-0.5 text-muted-foreground break-words">{detail}</p>
        {extra}
      </div>
    </div>
  );
}

export default DecomposePlanModal;
