/**
 * SpawnFromPlanModal — runner-side mirror of the web SpawnModal.
 *
 * Plan `2026-05-19-coordinator-production-readiness.md` Phase 4 (Wave 4).
 *
 * Inputs match the qontinui-web SpawnModal verbatim so operators have
 * the same affordance whether they're in the browser console or the
 * Tauri runner dashboard:
 *
 *   - plan_slug (free-text — runner has no /admin/coord/plans listing
 *     to pre-seed from; operator types the slug they want to spawn for)
 *   - plan_phase
 *   - repos (multi-select checkbox list of known repos)
 *   - intent
 *   - declared_overlap_paths (optional, newline-delimited)
 *   - initial_prompt
 *
 * Submit → `spawnFromPlan` in coordinatorApi.ts (POSTs to coord direct).
 * On success the banner shows the returned agent_id; on failure the
 * banner shows the HTTP error string.
 */

import { useCallback, useEffect, useState } from "react";
import { AlertCircle, CheckCircle2, Loader2, Rocket, X } from "lucide-react";
import { spawnFromPlan, type SpawnAgentResult } from "./coordinatorApi";

export interface SpawnFromPlanModalProps {
  /** Whether the modal is open. */
  open: boolean;
  /** Called when the user dismisses the modal. */
  onClose: () => void;
  /** Optional plan slug pre-seed. */
  initialPlanSlug?: string | null;
  /** Optional plan phase pre-seed. */
  initialPhase?: string | null;
  /** Called after a successful spawn with the coord response. */
  onSuccess?: (result: SpawnAgentResult) => void;
}

/** Canonical repo slug list — mirrors the qontinui-web SpawnModal. */
const KNOWN_REPOS = [
  "qontinui-web",
  "qontinui-runner",
  "qontinui-coord",
  "qontinui-schemas",
  "qontinui-mobile",
  "qontinui-ui-bridge",
  "qontinui-dev-notes",
] as const;

export function SpawnFromPlanModal({
  open,
  onClose,
  initialPlanSlug,
  initialPhase,
  onSuccess,
}: SpawnFromPlanModalProps) {
  const [planSlug, setPlanSlug] = useState("");
  const [phase, setPhase] = useState("");
  const [selectedRepos, setSelectedRepos] = useState<string[]>([]);
  const [otherRepos, setOtherRepos] = useState("");
  const [intent, setIntent] = useState("");
  const [overlapPaths, setOverlapPaths] = useState("");
  const [initialPrompt, setInitialPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<SpawnAgentResult | null>(null);

  // Reset on open.
  useEffect(() => {
    if (!open) return;
    setPlanSlug(initialPlanSlug ?? "");
    setPhase(initialPhase ?? "");
    setSelectedRepos([]);
    setOtherRepos("");
    setIntent("");
    setOverlapPaths("");
    setInitialPrompt("");
    setBusy(false);
    setError(null);
    setResult(null);
  }, [open, initialPlanSlug, initialPhase]);

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

  const toggleRepo = useCallback((repo: string) => {
    setSelectedRepos((prev) =>
      prev.includes(repo) ? prev.filter((r) => r !== repo) : [...prev, repo],
    );
  }, []);

  const handleSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      setError(null);
      setResult(null);
      setBusy(true);
      try {
        const extras = otherRepos
          .split(/[,\s]+/)
          .map((s) => s.trim())
          .filter(Boolean);
        const repos = Array.from(new Set([...selectedRepos, ...extras]));
        const paths = overlapPaths
          .split("\n")
          .map((s) => s.trim())
          .filter(Boolean);
        const out = await spawnFromPlan(
          planSlug.trim(),
          phase.trim(),
          repos,
          intent.trim(),
          initialPrompt.trim(),
          paths,
        );
        setResult(out);
        onSuccess?.(out);
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setError(msg);
      } finally {
        setBusy(false);
      }
    },
    [
      planSlug,
      phase,
      selectedRepos,
      otherRepos,
      intent,
      overlapPaths,
      initialPrompt,
      onSuccess,
    ],
  );

  if (!open) return null;

  const canSubmit =
    !busy &&
    planSlug.trim().length > 0 &&
    phase.trim().length > 0 &&
    intent.trim().length > 0 &&
    initialPrompt.trim().length > 0 &&
    (selectedRepos.length > 0 || otherRepos.trim().length > 0);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6"
      role="dialog"
      aria-modal="true"
      aria-label="Spawn from plan"
    >
      <div
        className="absolute inset-0 bg-black/40"
        onClick={onClose}
        aria-hidden="true"
      />
      <div
        className="relative w-full max-w-2xl max-h-[90vh] overflow-y-auto rounded-lg border border-border bg-background shadow-2xl"
        data-ui-bridge-id="productivity.spawn-from-plan-modal"
      >
        <div className="flex items-center justify-between gap-2 px-4 py-3 border-b border-border bg-card/40 sticky top-0">
          <div className="flex items-center gap-2">
            <Rocket className="w-4 h-4 text-primary" />
            <h2 className="text-sm font-semibold text-foreground">
              Spawn agent from plan
            </h2>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-muted/30"
            aria-label="Close"
            data-ui-bridge-id="productivity.spawn-from-plan-modal-close"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        <form onSubmit={handleSubmit} className="p-4 space-y-3">
          <p className="text-xs text-muted-foreground">
            Mint a coord agent pinned to this device. Coord acquires claims
            and delivers the initial prompt on first tick.
          </p>

          <label className="block">
            <span className="block text-xs font-medium text-foreground mb-1">
              Plan slug
            </span>
            <input
              type="text"
              value={planSlug}
              onChange={(e) => setPlanSlug(e.target.value)}
              placeholder="2026-05-19-coordinator-production-readiness"
              className="w-full px-2 py-1.5 rounded border border-border bg-background text-sm text-foreground font-mono placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-primary/40"
              disabled={busy}
              data-ui-bridge-id="productivity.spawn-from-plan-modal-slug"
            />
          </label>

          <label className="block">
            <span className="block text-xs font-medium text-foreground mb-1">
              Phase
            </span>
            <input
              type="text"
              value={phase}
              onChange={(e) => setPhase(e.target.value)}
              placeholder='e.g. "Phase 4" or "Wave 4 — spawn UI"'
              className="w-full px-2 py-1.5 rounded border border-border bg-background text-sm text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-primary/40"
              disabled={busy}
              data-ui-bridge-id="productivity.spawn-from-plan-modal-phase"
            />
          </label>

          <fieldset className="block">
            <legend className="block text-xs font-medium text-foreground mb-1">
              Repos
            </legend>
            <div
              className="grid grid-cols-2 gap-1.5 rounded border border-border p-2"
              data-ui-bridge-id="productivity.spawn-from-plan-modal-repos"
            >
              {KNOWN_REPOS.map((repo) => (
                <label
                  key={repo}
                  className="flex items-center gap-2 text-xs cursor-pointer"
                >
                  <input
                    type="checkbox"
                    checked={selectedRepos.includes(repo)}
                    onChange={() => toggleRepo(repo)}
                    disabled={busy}
                    data-ui-bridge-id={`productivity.spawn-from-plan-modal-repo-${repo}`}
                  />
                  <span className="font-mono">{repo}</span>
                </label>
              ))}
            </div>
            <input
              type="text"
              value={otherRepos}
              onChange={(e) => setOtherRepos(e.target.value)}
              placeholder="other repos (comma-separated)"
              className="mt-1 w-full px-2 py-1.5 rounded border border-border bg-background text-xs text-foreground font-mono placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-primary/40"
              disabled={busy}
              data-ui-bridge-id="productivity.spawn-from-plan-modal-other-repos"
            />
          </fieldset>

          <label className="block">
            <span className="block text-xs font-medium text-foreground mb-1">
              Intent
            </span>
            <input
              type="text"
              value={intent}
              onChange={(e) => setIntent(e.target.value)}
              placeholder="One-liner describing what this agent will do"
              className="w-full px-2 py-1.5 rounded border border-border bg-background text-sm text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-primary/40"
              disabled={busy}
              data-ui-bridge-id="productivity.spawn-from-plan-modal-intent"
            />
          </label>

          <label className="block">
            <span className="block text-xs font-medium text-foreground mb-1">
              Declared overlap paths (one per line, optional)
            </span>
            <textarea
              value={overlapPaths}
              onChange={(e) => setOverlapPaths(e.target.value)}
              rows={3}
              placeholder={"backend/app/api/v1/endpoints/operations.py"}
              className="w-full px-2 py-1.5 rounded border border-border bg-background text-xs text-foreground font-mono placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-primary/40"
              disabled={busy}
              data-ui-bridge-id="productivity.spawn-from-plan-modal-overlap-paths"
            />
          </label>

          <label className="block">
            <span className="block text-xs font-medium text-foreground mb-1">
              Initial prompt
            </span>
            <textarea
              value={initialPrompt}
              onChange={(e) => setInitialPrompt(e.target.value)}
              rows={6}
              placeholder="You are Wave N of plan X. Your scope: ..."
              className="w-full px-2 py-1.5 rounded border border-border bg-background text-xs text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-primary/40"
              disabled={busy}
              data-ui-bridge-id="productivity.spawn-from-plan-modal-initial-prompt"
            />
          </label>

          {error && (
            <div
              className="flex items-start gap-2 p-2.5 rounded border bg-red-500/10 border-red-500/40 text-red-300 text-xs"
              role="alert"
              data-ui-bridge-id="productivity.spawn-from-plan-modal-error"
            >
              <AlertCircle className="w-3.5 h-3.5 mt-0.5 shrink-0 text-red-400" />
              <div className="min-w-0 flex-1">
                <p className="font-medium">Spawn failed</p>
                <p className="mt-0.5 break-words">{error}</p>
              </div>
            </div>
          )}

          {result && (
            <div
              className="flex items-start gap-2 p-2.5 rounded border bg-emerald-500/10 border-emerald-500/40 text-emerald-300 text-xs"
              role="status"
              data-ui-bridge-id="productivity.spawn-from-plan-modal-success"
            >
              <CheckCircle2 className="w-3.5 h-3.5 mt-0.5 shrink-0 text-emerald-400" />
              <div className="min-w-0 flex-1">
                <p className="font-medium">
                  Spawned {result.agentId ?? "agent"}
                </p>
                <p className="mt-0.5 text-muted-foreground font-mono break-all">
                  session: {result.agentSessionId ?? "(unknown)"}
                </p>
              </div>
            </div>
          )}

          <div className="flex items-center justify-end gap-2 pt-2 border-t border-border">
            <button
              type="button"
              onClick={onClose}
              disabled={busy}
              className="px-3 py-1.5 rounded text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 disabled:opacity-50"
              data-ui-bridge-id="productivity.spawn-from-plan-modal-cancel"
            >
              {result ? "Done" : "Cancel"}
            </button>
            <button
              type="submit"
              disabled={!canSubmit}
              className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded bg-primary text-primary-foreground text-xs font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-bridge-id="productivity.spawn-from-plan-modal-submit"
            >
              {busy ? (
                <>
                  <Loader2 className="w-3 h-3 animate-spin" />
                  Spawning…
                </>
              ) : (
                <>
                  <Rocket className="w-3 h-3" />
                  Spawn
                </>
              )}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

export default SpawnFromPlanModal;
