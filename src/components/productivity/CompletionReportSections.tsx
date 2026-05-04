/**
 * Completion Report sections for the TaskDetailPanel — Phase 4 of
 * `productivity-coordinator-completion-reports.md` §5.
 *
 * Four subcomponents, all extracted as their own pieces so they're testable
 * in isolation (the runner's vitest config is `environment: "node"` with no
 * jsdom — DOM-rendering tests are not viable; we test pure helpers and
 * use fetch mocks for the manual-fire form).
 *
 *   1. <CompletionReportSection task={detail} />     — renders the report
 *   2. <UpstreamReportsPreview task={detail} />      — for pending tasks
 *   3. <BriefingPreview task={detail} />             — for ready+ tasks
 *   4. <ManualFireForm task={detail} onFired={…} />  — manual user fire
 *
 * The new spec elements per §5 are placed on the section roots:
 *   - productivity.task-completion-report
 *   - productivity.task-upstream-reports-preview
 *   - productivity.task-briefing-preview
 *   - productivity.task-manual-fire-form
 *
 * All four belong to the `productivity-plans` state.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import { ChevronDown, ChevronRight, AlertTriangle, Loader2 } from "lucide-react";
import { previewAssignmentBrief } from "./api";
import type {
  BreakingChange,
  CompletionReport,
  CompletionSource,
  Deliverable,
  DeliverableKind,
  FollowUp,
  FollowUpPriority,
  TaskDetail,
} from "./types";

// ============================================================================
// Pure helpers — exported for unit testing.
// ============================================================================

/**
 * Map a `Deliverable` to an HTTPS URL when one can be derived. Returns null
 * for kinds with no canonical URL (e.g. commit SHAs without a repo URL,
 * file paths, schema-change tags). The plan §5 calls for linkifying `pr`
 * and `endpoint`; we add `commit` only when the reference itself is
 * already a URL (so a SHA-only string is rendered as text).
 */
export function deliverableHref(d: Deliverable): string | null {
  const ref = (d.reference ?? "").trim();
  if (!ref) return null;
  // Anything that already looks like an absolute URL — render as a link
  // regardless of `kind`. Same heuristic the runner's generic-link rendering
  // uses elsewhere (see qontinui-runner/src/components/MarkdownViewer.tsx).
  if (/^https?:\/\//i.test(ref)) return ref;
  return null;
}

/**
 * Human-readable label for a `CompletionSource` enum value. The badge text
 * prefers a short capitalized form rather than the kebab-cased wire value.
 */
export function completionSourceLabel(source: CompletionSource): string {
  switch (source) {
    case "session-self-report":
      return "Session self-report";
    case "reviewer-attestation":
      return "Reviewer attestation";
    case "github-merge":
      return "GitHub merge";
    case "ci-pipeline":
      return "CI pipeline";
    case "manual-user-fire":
      return "Manual user fire";
    case "legacy-pre-completion-reports":
      return "Legacy (pre-migration)";
  }
}

/**
 * Tailwind classes for the small completion-source badge. Each source gets
 * a distinct hue so the dashboard can scan reports at a glance.
 */
export function completionSourceClasses(source: CompletionSource): string {
  switch (source) {
    case "session-self-report":
      return "bg-blue-500/10 text-blue-300 border-blue-500/40";
    case "reviewer-attestation":
      return "bg-purple-500/10 text-purple-300 border-purple-500/40";
    case "github-merge":
      return "bg-emerald-500/10 text-emerald-300 border-emerald-500/40";
    case "ci-pipeline":
      return "bg-cyan-500/10 text-cyan-300 border-cyan-500/40";
    case "manual-user-fire":
      return "bg-amber-500/10 text-amber-300 border-amber-500/40";
    case "legacy-pre-completion-reports":
      return "bg-muted/30 text-muted-foreground border-border";
  }
}

/**
 * Tailwind classes for the priority badge on a follow-up entry.
 */
export function followUpPriorityClasses(priority: string): string {
  switch (priority) {
    case "critical":
      return "bg-red-500/10 text-red-300 border-red-500/40";
    case "important":
      return "bg-amber-500/10 text-amber-300 border-amber-500/40";
    case "nice-to-have":
      return "bg-muted/30 text-muted-foreground border-border";
    default:
      return "bg-muted/30 text-muted-foreground border-border";
  }
}

/**
 * Truncate `s` to ≤ `max` characters, appending an ellipsis when trimming.
 * Used by the upstream-reports preview to keep summary previews short.
 */
export function truncatePreview(s: string, max = 300): string {
  if (s.length <= max) return s;
  return `${s.slice(0, max).trimEnd()}…`;
}

// ============================================================================
// 1. CompletionReportSection
// ============================================================================

interface CompletionReportSectionProps {
  detail: TaskDetail;
}

export function CompletionReportSection({ detail }: CompletionReportSectionProps) {
  const report = detail.completionReport;
  const source = detail.completionSource;
  if (!report) return null;

  return (
    <section data-ui-bridge-id="productivity.task-completion-report">
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
          Completion report
        </h3>
        {source ? (
          <span
            className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-medium tracking-wide ${completionSourceClasses(
              source,
            )}`}
            data-ui-bridge-id="productivity.task-completion-source-badge"
            title={`Source: ${source}`}
          >
            {completionSourceLabel(source)}
          </span>
        ) : null}
      </div>

      {report.summaryMd ? (
        <div className="mt-2 text-xs text-foreground prose prose-invert max-w-none prose-p:my-1 prose-headings:my-1">
          <ReactMarkdown>{report.summaryMd}</ReactMarkdown>
        </div>
      ) : null}

      {report.deliverables.length > 0 ? (
        <DeliverablesTable items={report.deliverables} />
      ) : null}

      {report.breakingChanges.length > 0 ? (
        <BreakingChangesList items={report.breakingChanges} />
      ) : null}

      {report.followUps.length > 0 ? <FollowUpsList items={report.followUps} /> : null}
    </section>
  );
}

function DeliverablesTable({ items }: { items: Deliverable[] }) {
  return (
    <div className="mt-2">
      <h4 className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/80">
        Deliverables
      </h4>
      <table className="mt-1 w-full text-[11px]">
        <thead>
          <tr className="text-left text-muted-foreground/70">
            <th className="font-normal pr-2 pb-1">Kind</th>
            <th className="font-normal pr-2 pb-1">Reference</th>
            <th className="font-normal pb-1">Description</th>
          </tr>
        </thead>
        <tbody>
          {items.map((d, i) => {
            const href = deliverableHref(d);
            return (
              <tr key={i} className="align-top">
                <td className="pr-2 py-0.5 font-mono text-muted-foreground">{d.kind}</td>
                <td className="pr-2 py-0.5 font-mono text-foreground break-all">
                  {href ? (
                    <a
                      href={href}
                      target="_blank"
                      rel="noreferrer noopener"
                      className="underline hover:text-primary"
                    >
                      {d.reference}
                    </a>
                  ) : (
                    d.reference
                  )}
                </td>
                <td className="py-0.5 text-foreground">{d.description}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function BreakingChangesList({ items }: { items: BreakingChange[] }) {
  return (
    <div className="mt-3 space-y-1.5">
      <h4 className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/80">
        Breaking changes
      </h4>
      {items.map((bc, i) => (
        <details
          key={i}
          className="rounded border border-amber-700/40 bg-amber-950/20 px-2 py-1.5 text-[11px] text-amber-100"
        >
          <summary className="cursor-pointer flex items-center gap-1.5 font-medium">
            <AlertTriangle className="w-3 h-3 text-amber-300" />
            <span className="font-mono text-amber-300">{bc.area}</span>
            <span className="text-amber-100">— {bc.description}</span>
          </summary>
          {bc.migrationStepsMd ? (
            <div className="mt-1.5 prose prose-invert max-w-none prose-p:my-1 prose-headings:my-1 text-amber-100/90">
              <ReactMarkdown>{bc.migrationStepsMd}</ReactMarkdown>
            </div>
          ) : (
            <p className="mt-1.5 italic text-amber-100/70">No migration steps documented.</p>
          )}
        </details>
      ))}
    </div>
  );
}

function FollowUpsList({ items }: { items: FollowUp[] }) {
  return (
    <div className="mt-3">
      <h4 className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/80">
        Follow-ups
      </h4>
      <ul className="mt-1 space-y-1 text-[11px]">
        {items.map((fu, i) => (
          <li key={i} className="flex items-start gap-1.5">
            <span className="text-foreground flex-1">{fu.description}</span>
            <span
              className={`shrink-0 inline-flex items-center rounded-full border px-1.5 py-0 text-[10px] font-medium ${followUpPriorityClasses(
                fu.priority,
              )}`}
            >
              {fu.priority}
            </span>
            {fu.blockingForDependents ? (
              <span
                className="shrink-0 inline-flex items-center rounded-full border border-red-500/50 bg-red-500/10 px-1.5 py-0 text-[10px] font-medium text-red-300"
                data-ui-bridge-id="productivity.task-followup-blocking-pill"
                title="Blocks downstream dependents"
              >
                blocks downstream
              </span>
            ) : null}
          </li>
        ))}
      </ul>
    </div>
  );
}

// ============================================================================
// 2. UpstreamReportsPreview
// ============================================================================

interface UpstreamReportsPreviewProps {
  detail: TaskDetail;
  /** Pre-loaded peer task details, keyed by task id, when available from
   *  the parent's task list. Lookup-misses are best-effort fetched per-id
   *  via `get_task_detail`. */
  peerDetailsById?: Record<string, TaskDetail>;
}

interface UpstreamSummary {
  taskId: string;
  description: string | null;
  report: CompletionReport | null;
}

export function UpstreamReportsPreview({
  detail,
  peerDetailsById,
}: UpstreamReportsPreviewProps) {
  const upstreamIds = detail.task.dependsOn;
  const shouldRender = detail.task.status === "pending" && upstreamIds.length > 0;

  const [resolved, setResolved] = useState<UpstreamSummary[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!shouldRender) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- clearing dependent state when the parent gate flips off
      setResolved([]);
      return;
    }
    let cancelled = false;
    void (async () => {
      setLoading(true);
      const out: UpstreamSummary[] = [];
      for (const upstreamId of upstreamIds) {
        const cached = peerDetailsById?.[upstreamId];
        if (cached) {
          out.push({
            taskId: upstreamId,
            description: cached.task.description,
            report: cached.completionReport,
          });
          continue;
        }
        try {
          const fetched = await invoke<TaskDetail | null>("get_task_detail", {
            taskId: upstreamId,
          });
          if (cancelled) return;
          if (fetched) {
            out.push({
              taskId: upstreamId,
              description: fetched.task.description,
              report: fetched.completionReport,
            });
          } else {
            out.push({ taskId: upstreamId, description: null, report: null });
          }
        } catch (err) {
          // Best-effort: render the row with no report on lookup failure.
          console.warn(`[productivity] upstream ${upstreamId} fetch failed:`, err);
          if (cancelled) return;
          out.push({ taskId: upstreamId, description: null, report: null });
        }
      }
      if (!cancelled) {
        setResolved(out);
        setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [shouldRender, upstreamIds, peerDetailsById]);

  if (!shouldRender) return null;

  return (
    <section data-ui-bridge-id="productivity.task-upstream-reports-preview">
      <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
        Upstream reports
      </h3>
      {loading && resolved.length === 0 ? (
        <div className="mt-2 flex items-center gap-2 text-[11px] text-muted-foreground">
          <Loader2 className="w-3 h-3 animate-spin" /> Loading upstream reports…
        </div>
      ) : (
        <ul className="mt-2 space-y-2">
          {resolved.map((u) => (
            <UpstreamSummaryCard key={u.taskId} summary={u} />
          ))}
        </ul>
      )}
    </section>
  );
}

function UpstreamSummaryCard({ summary }: { summary: UpstreamSummary }) {
  const firstLine = summary.description ? summary.description.split("\n")[0] : "(unknown task)";
  const report = summary.report;
  const blockingFollowUps = report
    ? report.followUps.filter((fu) => fu.blockingForDependents).length
    : 0;
  return (
    <li className="rounded border border-border bg-muted/10 px-2 py-1.5 text-[11px]">
      <div className="font-medium text-foreground truncate">{firstLine}</div>
      <div className="mt-0.5 font-mono text-[10px] text-muted-foreground/70 truncate">
        {summary.taskId}
      </div>
      {report ? (
        <>
          <p className="mt-1 text-muted-foreground whitespace-pre-wrap">
            {truncatePreview(report.summaryMd ?? "", 300) || "(no summary)"}
          </p>
          <p className="mt-1 text-[10px] text-muted-foreground/80">
            {report.breakingChanges.length} breaking change
            {report.breakingChanges.length === 1 ? "" : "s"} / {report.followUps.length} follow-up
            {report.followUps.length === 1 ? "" : "s"} ({blockingFollowUps} blocking)
          </p>
        </>
      ) : (
        <p className="mt-1 italic text-muted-foreground/70">No report yet — upstream still in flight.</p>
      )}
    </li>
  );
}

// ============================================================================
// 3. BriefingPreview
// ============================================================================

interface BriefingPreviewProps {
  detail: TaskDetail;
}

const BRIEFING_PREVIEW_STATUSES = new Set([
  "ready",
  "assigned",
  "running",
  "review",
  "needs_fix",
  "done",
]);

export function BriefingPreview({ detail }: BriefingPreviewProps) {
  const taskId = detail.task.id;
  const eligibleStatus = BRIEFING_PREVIEW_STATUSES.has(detail.task.status);
  const shouldFetch = eligibleStatus && detail.hasAssignmentBriefExtras;

  const [brief, setBrief] = useState<string>("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!shouldFetch) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- clearing dependent state when the gate flips off
      setBrief("");
      setError(null);
      return;
    }
    let cancelled = false;
    void (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await previewAssignmentBrief(taskId);
        if (!cancelled) setBrief(out);
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [shouldFetch, taskId]);

  if (!shouldFetch) return null;

  return (
    <section data-ui-bridge-id="productivity.task-briefing-preview">
      <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
        Briefing preview
      </h3>
      <p className="mt-1 text-[10px] text-muted-foreground/80">
        Exact Markdown the next assignment will inject into the worker's first message.
      </p>
      {loading ? (
        <div className="mt-2 flex items-center gap-2 text-[11px] text-muted-foreground">
          <Loader2 className="w-3 h-3 animate-spin" /> Composing brief…
        </div>
      ) : error ? (
        <p className="mt-2 text-[11px] text-rose-300">Failed to compose brief: {error}</p>
      ) : brief.length === 0 ? (
        <p className="mt-2 italic text-[11px] text-muted-foreground/70">
          No upstream payload — the worker will receive the task description only.
        </p>
      ) : (
        <pre className="mt-2 max-h-[320px] overflow-auto whitespace-pre-wrap font-mono text-[11px] bg-card/40 border border-border rounded p-2 text-foreground">
          {brief}
        </pre>
      )}
    </section>
  );
}

// ============================================================================
// 4. ManualFireForm
// ============================================================================

interface ManualFireFormProps {
  detail: TaskDetail;
  /** Called after a successful manual-fire so the parent can refresh the
   *  task detail panel. */
  onFired?: () => void;
}

interface DraftDeliverable {
  kind: string;
  reference: string;
  description: string;
}

interface DraftBreakingChange {
  area: string;
  description: string;
  migrationStepsMd: string;
}

interface DraftFollowUp {
  description: string;
  priority: string;
  blockingForDependents: boolean;
}

interface DraftState {
  summaryMd: string;
  deliverables: DraftDeliverable[];
  breakingChanges: DraftBreakingChange[];
  followUps: DraftFollowUp[];
}

const EMPTY_DRAFT: DraftState = {
  summaryMd: "",
  deliverables: [],
  breakingChanges: [],
  followUps: [],
};

const SUMMARY_MAX_CHARS = 4000;

const DELIVERABLE_KINDS: DeliverableKind[] = [
  "commit",
  "pr",
  "file",
  "endpoint",
  "schema-change",
  "spec",
  "plan-stamp",
  "fixture",
  "doc-update",
  "other",
];

const FOLLOW_UP_PRIORITIES: FollowUpPriority[] = ["critical", "important", "nice-to-have"];

/**
 * Strip empty rows from a draft and convert it to the wire-format
 * `CompletionReport` body. Used both at submit time and (exported) for unit
 * testing.
 */
export function buildReportFromDraft(draft: DraftState): CompletionReport {
  const deliverables: Deliverable[] = draft.deliverables
    .filter((d) => d.reference.trim() !== "" || d.description.trim() !== "")
    .map((d) => ({
      kind: d.kind,
      reference: d.reference,
      description: d.description,
    }));
  const breakingChanges: BreakingChange[] = draft.breakingChanges
    .filter(
      (bc) =>
        bc.area.trim() !== "" || bc.description.trim() !== "" || bc.migrationStepsMd.trim() !== "",
    )
    .map((bc) => ({
      area: bc.area,
      description: bc.description,
      migrationStepsMd: bc.migrationStepsMd,
    }));
  const followUps: FollowUp[] = draft.followUps
    .filter((fu) => fu.description.trim() !== "")
    .map((fu) => ({
      description: fu.description,
      priority: fu.priority,
      blockingForDependents: fu.blockingForDependents,
    }));
  return {
    summaryMd: draft.summaryMd,
    deliverables,
    breakingChanges,
    followUps,
  };
}

/**
 * Validation gate before submitting a manual-fire. Returns `null` when the
 * draft is acceptable, or an inline error string. Mirrors the server-side
 * caps in `database/pg/completion_reports.rs::validate_report` so the user
 * doesn't round-trip a 400.
 */
export function validateManualFireDraft(draft: DraftState): string | null {
  if (draft.summaryMd.trim() === "") return "summaryMd is required";
  if (draft.summaryMd.length > SUMMARY_MAX_CHARS) {
    return `summaryMd too long (${draft.summaryMd.length} > ${SUMMARY_MAX_CHARS})`;
  }
  return null;
}

export function ManualFireForm({ detail, onFired }: ManualFireFormProps) {
  const [expanded, setExpanded] = useState(false);
  const [draft, setDraft] = useState<DraftState>(EMPTY_DRAFT);
  const [submitting, setSubmitting] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);
  const taskId = detail.task.id;

  const charCount = draft.summaryMd.length;
  const charClass =
    charCount > SUMMARY_MAX_CHARS
      ? "text-red-300"
      : charCount > SUMMARY_MAX_CHARS * 0.9
        ? "text-amber-300"
        : "text-muted-foreground";

  const handleSubmit = useCallback(async () => {
    const err = validateManualFireDraft(draft);
    if (err != null) {
      setValidationError(err);
      return;
    }
    setValidationError(null);
    setSubmitError(null);
    setSuccess(false);
    setSubmitting(true);
    try {
      const port = await invoke<number>("get_api_port");
      if (!port || port <= 0) throw new Error(`invalid runner port: ${port}`);
      const body = {
        taskId,
        completionReport: buildReportFromDraft(draft),
      };
      const res = await fetch(`http://localhost:${port}/completion-sources/manual-user-fire`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const text = await res.text().catch(() => "");
        throw new Error(`HTTP ${res.status}: ${text || res.statusText}`);
      }
      setSuccess(true);
      setDraft(EMPTY_DRAFT);
      onFired?.();
    } catch (err) {
      setSubmitError(String(err));
    } finally {
      setSubmitting(false);
    }
  }, [draft, taskId, onFired]);

  // Hide entirely for `done` tasks — the manual-fire path force-flips to
  // `done` and overwrites any existing completion_report (the Rust handler
  // is by-design idempotent on done). Plan §5: the form is for tasks
  // "awaiting an external event whose adapter doesn't exist", not for
  // amending finalized work. To correct a wrong report on a done task,
  // edit `coord.tasks.completion_report` directly via SQL or extend this
  // form with an explicit "override existing report" affordance. Placed
  // here (after all hooks) to satisfy react-hooks/rules-of-hooks.
  if (detail.task.status === "done") {
    return null;
  }

  const ToggleIcon = expanded ? ChevronDown : ChevronRight;

  return (
    <section data-ui-bridge-id="productivity.task-manual-fire-form">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        className="w-full flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground hover:text-foreground transition-colors"
        aria-expanded={expanded}
        data-ui-bridge-id="productivity.task-manual-fire-toggle"
      >
        <ToggleIcon className="w-3 h-3" />
        Manually fire completion report
      </button>

      {expanded ? (
        <div className="mt-2 space-y-3 rounded border border-border bg-card/30 p-2">
          {/* summaryMd */}
          <div>
            <label className="block text-[11px] font-medium text-foreground">
              Summary (Markdown)
              <span className={`ml-2 text-[10px] ${charClass}`}>
                {charCount}/{SUMMARY_MAX_CHARS}
              </span>
            </label>
            <textarea
              className="mt-1 w-full min-h-[80px] rounded border border-border bg-background px-2 py-1 text-xs font-mono text-foreground"
              value={draft.summaryMd}
              onChange={(e) => setDraft((d) => ({ ...d, summaryMd: e.target.value }))}
              placeholder="What was produced; key learnings; what downstream needs to know."
              data-ui-bridge-id="productivity.task-manual-fire-summary"
            />
          </div>

          {/* deliverables */}
          <DraftDeliverablesEditor
            items={draft.deliverables}
            onChange={(deliverables) => setDraft((d) => ({ ...d, deliverables }))}
          />

          {/* breakingChanges */}
          <DraftBreakingChangesEditor
            items={draft.breakingChanges}
            onChange={(breakingChanges) => setDraft((d) => ({ ...d, breakingChanges }))}
          />

          {/* followUps */}
          <DraftFollowUpsEditor
            items={draft.followUps}
            onChange={(followUps) => setDraft((d) => ({ ...d, followUps }))}
          />

          {validationError ? (
            <p className="text-[11px] text-rose-300">{validationError}</p>
          ) : null}
          {submitError ? (
            <p className="text-[11px] text-rose-300">Submit failed: {submitError}</p>
          ) : null}
          {success ? (
            <p className="text-[11px] text-emerald-300">Completion report fired.</p>
          ) : null}

          <button
            type="button"
            onClick={handleSubmit}
            disabled={submitting}
            className="inline-flex items-center gap-1.5 px-2 py-1 text-[11px] rounded border border-amber-700/50 bg-amber-950/30 hover:bg-amber-900/30 text-amber-100 disabled:opacity-50 transition-colors"
            data-ui-bridge-id="productivity.task-manual-fire-submit"
          >
            {submitting ? <Loader2 className="w-3 h-3 animate-spin" /> : null}
            Fire manual completion report
          </button>
        </div>
      ) : null}
    </section>
  );
}

interface DraftListEditorProps<T> {
  items: T[];
  onChange: (items: T[]) => void;
}

function DraftDeliverablesEditor({
  items,
  onChange,
}: DraftListEditorProps<DraftDeliverable>) {
  const addRow = () =>
    onChange([...items, { kind: "other", reference: "", description: "" }]);
  return (
    <div>
      <label className="block text-[11px] font-medium text-foreground">Deliverables</label>
      <ul className="mt-1 space-y-1">
        {items.map((d, i) => (
          <li key={i} className="flex gap-1 items-start">
            <select
              className="rounded border border-border bg-background px-1 py-0.5 text-[11px] text-foreground"
              value={d.kind}
              onChange={(e) => {
                const next = [...items];
                next[i] = { ...d, kind: e.target.value };
                onChange(next);
              }}
            >
              {DELIVERABLE_KINDS.map((k) => (
                <option key={k} value={k}>
                  {k}
                </option>
              ))}
            </select>
            <input
              className="flex-1 rounded border border-border bg-background px-1 py-0.5 text-[11px] font-mono text-foreground"
              placeholder="reference (URL, SHA, path)"
              value={d.reference}
              onChange={(e) => {
                const next = [...items];
                next[i] = { ...d, reference: e.target.value };
                onChange(next);
              }}
            />
            <input
              className="flex-[2_1_0%] rounded border border-border bg-background px-1 py-0.5 text-[11px] text-foreground"
              placeholder="description"
              value={d.description}
              onChange={(e) => {
                const next = [...items];
                next[i] = { ...d, description: e.target.value };
                onChange(next);
              }}
            />
            <button
              type="button"
              onClick={() => onChange(items.filter((_, j) => j !== i))}
              className="px-1 py-0.5 text-[10px] text-muted-foreground hover:text-red-300"
              aria-label={`Remove deliverable row ${i + 1}`}
            >
              ×
            </button>
          </li>
        ))}
      </ul>
      <button
        type="button"
        onClick={addRow}
        className="mt-1 text-[10px] text-muted-foreground hover:text-foreground underline"
      >
        + Add deliverable
      </button>
    </div>
  );
}

function DraftBreakingChangesEditor({
  items,
  onChange,
}: DraftListEditorProps<DraftBreakingChange>) {
  const addRow = () =>
    onChange([...items, { area: "", description: "", migrationStepsMd: "" }]);
  return (
    <div>
      <label className="block text-[11px] font-medium text-foreground">Breaking changes</label>
      <ul className="mt-1 space-y-1">
        {items.map((bc, i) => (
          <li key={i} className="rounded border border-amber-700/40 bg-amber-950/10 p-1 space-y-1">
            <div className="flex gap-1">
              <input
                className="flex-1 rounded border border-border bg-background px-1 py-0.5 text-[11px] font-mono text-foreground"
                placeholder="area (subsystem)"
                value={bc.area}
                onChange={(e) => {
                  const next = [...items];
                  next[i] = { ...bc, area: e.target.value };
                  onChange(next);
                }}
              />
              <input
                className="flex-[2_1_0%] rounded border border-border bg-background px-1 py-0.5 text-[11px] text-foreground"
                placeholder="description"
                value={bc.description}
                onChange={(e) => {
                  const next = [...items];
                  next[i] = { ...bc, description: e.target.value };
                  onChange(next);
                }}
              />
              <button
                type="button"
                onClick={() => onChange(items.filter((_, j) => j !== i))}
                className="px-1 py-0.5 text-[10px] text-muted-foreground hover:text-red-300"
                aria-label={`Remove breaking-change row ${i + 1}`}
              >
                ×
              </button>
            </div>
            <textarea
              className="w-full min-h-[40px] rounded border border-border bg-background px-1 py-0.5 text-[11px] font-mono text-foreground"
              placeholder="migration steps (Markdown)"
              value={bc.migrationStepsMd}
              onChange={(e) => {
                const next = [...items];
                next[i] = { ...bc, migrationStepsMd: e.target.value };
                onChange(next);
              }}
            />
          </li>
        ))}
      </ul>
      <button
        type="button"
        onClick={addRow}
        className="mt-1 text-[10px] text-muted-foreground hover:text-foreground underline"
      >
        + Add breaking change
      </button>
    </div>
  );
}

function DraftFollowUpsEditor({ items, onChange }: DraftListEditorProps<DraftFollowUp>) {
  const addRow = () =>
    onChange([
      ...items,
      { description: "", priority: "nice-to-have", blockingForDependents: false },
    ]);
  return (
    <div>
      <label className="block text-[11px] font-medium text-foreground">Follow-ups</label>
      <ul className="mt-1 space-y-1">
        {items.map((fu, i) => (
          <li key={i} className="flex gap-1 items-center">
            <input
              className="flex-[2_1_0%] rounded border border-border bg-background px-1 py-0.5 text-[11px] text-foreground"
              placeholder="description"
              value={fu.description}
              onChange={(e) => {
                const next = [...items];
                next[i] = { ...fu, description: e.target.value };
                onChange(next);
              }}
            />
            <select
              className="rounded border border-border bg-background px-1 py-0.5 text-[11px] text-foreground"
              value={fu.priority}
              onChange={(e) => {
                const next = [...items];
                next[i] = { ...fu, priority: e.target.value };
                onChange(next);
              }}
            >
              {FOLLOW_UP_PRIORITIES.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
            <label className="flex items-center gap-1 text-[10px] text-muted-foreground">
              <input
                type="checkbox"
                className="accent-red-500"
                checked={fu.blockingForDependents}
                onChange={(e) => {
                  const next = [...items];
                  next[i] = { ...fu, blockingForDependents: e.target.checked };
                  onChange(next);
                }}
              />
              blocks
            </label>
            <button
              type="button"
              onClick={() => onChange(items.filter((_, j) => j !== i))}
              className="px-1 py-0.5 text-[10px] text-muted-foreground hover:text-red-300"
              aria-label={`Remove follow-up row ${i + 1}`}
            >
              ×
            </button>
          </li>
        ))}
      </ul>
      <button
        type="button"
        onClick={addRow}
        className="mt-1 text-[10px] text-muted-foreground hover:text-foreground underline"
      >
        + Add follow-up
      </button>
    </div>
  );
}

// Re-export types used by tests so the test file doesn't depend on internal
// shapes shifting.
export type {
  DraftBreakingChange,
  DraftDeliverable,
  DraftFollowUp,
  DraftState,
};
