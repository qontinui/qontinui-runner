/**
 * SessionBriefingPanel — READ-ONLY view of the system-prompt text THIS runner
 * will inject into the sessions it hosts.
 *
 * Plan `2026-08-20-runner-session-briefing-versioned-and-operator-editable`,
 * Phase 4 (runner half).
 *
 * Why this panel exists: before the plan, the only way an operator could read
 * the instructions their fleet's agents were actually running under was to
 * `printenv QONTINUI_RUNNER_CONTEXT` inside a live session, or read the Rust
 * source of the build that happened to be running.
 *
 * It renders whatever `GET /session-briefing` returns, which in turn renders by
 * CALLING `terminal::runner_context()` — never by re-deriving the text — so the
 * panel and the prompt cannot disagree. Editing happens in qontinui-web at
 * `/admin/coord/prompt-documents`; there is deliberately no write path here.
 *
 * Collapsed by default and fetched on EXPAND, not on mount: the route's
 * `ai_session_rules` half probes the supervisor, and this panel sits inside a
 * settings tab an operator opens for unrelated reasons.
 */

import { useCallback, useState } from "react";
import { ChevronDown, ChevronRight, FileText, RefreshCw } from "lucide-react";
import { tracedFetch, useApiBase } from "@/lib/runner-api";
import {
  describePlanCaptureClause,
  formatDocumentVersion,
  formatLastConfirmed,
  provenanceClasses,
} from "./sessionBriefingHelpers";

/**
 * Where a block's text came from, as `GET /session-briefing` reports it.
 *
 * Split out of [`BriefingBlock`] because the plan-capture clause carries all of
 * this and no `text` of its own — its body is either already inside the
 * briefing block above (fleet dial at `record`) or not injected at all.
 */
interface BriefingDocumentState {
  /** `coord` | `cached` | `builtin`. */
  provenance: string;
  /** The same fact spelled out, e.g. `builtin-fallback (rejected coord v7)`. */
  provenance_detail: string;
  /** The version RENDERED, or null when the builtin was. */
  document_version: number | null;
  fetched_at: string | null;
  document?: string;
}

/** One rendered block as `GET /session-briefing` reports it. */
interface BriefingBlock extends BriefingDocumentState {
  text: string;
}

interface PlanCaptureClauseState extends BriefingDocumentState {
  /** Is the fleet dial at `record`, i.e. is the clause actually appended? */
  included: boolean;
}

interface SessionBriefingResponse extends BriefingBlock {
  /**
   * The flat boolean Phase 4 of the plan specifies. The route builds it and
   * `plan_capture_clause.included` from the same value, and this panel reads
   * the nested one — the clause view takes the whole clause. Declared anyway
   * because it is part of the published route shape that `curl` readers and
   * scripts use, not an accident.
   */
  plan_capture_clause_included: boolean;
  plan_capture_clause: PlanCaptureClauseState;
  api_port: number;
  ai_session_rules: BriefingBlock;
}

interface ApiEnvelope<T> {
  success: boolean;
  data?: T;
  error?: string;
}

function ProvenanceBadge({ detail, provenance }: { detail: string; provenance: string }) {
  return (
    <span
      className={`shrink-0 max-w-[16rem] truncate px-2 py-0.5 rounded-full text-[10px] font-medium ${provenanceClasses(provenance)}`}
      title={detail}
    >
      {detail}
    </span>
  );
}

/**
 * Which document a block came from, which generation of it, and when the runner
 * last confirmed that against coord.
 *
 * Its own component because the plan-capture clause needs exactly this row and
 * has no text to put beside it. Both absent-value rules live in
 * `sessionBriefingHelpers` rather than inline here — they are honesty rules
 * about what the runner can and cannot state, and they were the half of this
 * panel that nothing could test.
 */
function BlockMetaRow({ block }: { block: BriefingDocumentState }) {
  return (
    <div className="flex flex-wrap gap-x-4 gap-y-1 text-[10px] text-muted-foreground">
      {block.document && <span>document: {block.document}</span>}
      <span>version: {formatDocumentVersion(block.document_version, block.provenance)}</span>
      <span>last confirmed: {formatLastConfirmed(block.fetched_at, block.provenance)}</span>
    </div>
  );
}

function BlockView({
  title,
  subtitle,
  block,
}: {
  title: string;
  subtitle: string;
  block: BriefingBlock;
}) {
  return (
    <div className="rounded-lg bg-muted/30 p-3 space-y-2">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h5 className="text-sm font-medium">{title}</h5>
          <p className="text-[10px] text-muted-foreground">{subtitle}</p>
        </div>
        <ProvenanceBadge detail={block.provenance_detail} provenance={block.provenance} />
      </div>
      <BlockMetaRow block={block} />
      <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-md bg-background/60 p-2 text-[11px] leading-relaxed">
        {block.text}
      </pre>
    </div>
  );
}

/**
 * The fleet-gated plan-capture clause.
 *
 * Rendered the SAME WAY in both arms, on purpose. `GET /session-briefing`
 * reports this document's provenance, version and stamp whether or not the
 * clause is in force, because the fleet dial and the coord document are two
 * independent facts and only one of them is what an operator is usually asking
 * about. The panel used to print the document state when the clause was
 * included and drop it entirely when it was omitted — i.e. exactly in the arm
 * where "why is my edited clause not in the prompt?" gets asked, leaving the
 * three fields the route serves for that reader unread.
 */
function PlanCaptureClauseView({ clause }: { clause: PlanCaptureClauseState }) {
  return (
    <div className="rounded-lg bg-muted/20 p-3 space-y-2">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h5 className="text-sm font-medium">Plan-capture clause</h5>
          <p className="text-[10px] text-muted-foreground">
            {describePlanCaptureClause(clause.included, clause.provenance)}
          </p>
        </div>
        <ProvenanceBadge detail={clause.provenance_detail} provenance={clause.provenance} />
      </div>
      <BlockMetaRow block={clause} />
    </div>
  );
}

export function SessionBriefingPanel() {
  // `useApiBase`, not `getApiBase`: a late `api-ready` event must re-render, or
  // a pop-out realm keeps addressing the default 9876 (runner-api.ts:8-13).
  const apiBase = useApiBase();
  const [expanded, setExpanded] = useState(false);
  const [data, setData] = useState<SessionBriefingResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const response = await tracedFetch(`${apiBase}/session-briefing`);
      if (!response.ok) {
        throw new Error(`runner API answered HTTP ${response.status}`);
      }
      const envelope = (await response.json()) as ApiEnvelope<SessionBriefingResponse>;
      if (!envelope.success || !envelope.data) {
        throw new Error(envelope.error ?? "the runner returned no briefing");
      }
      setData(envelope.data);
    } catch (e) {
      // An unreachable local API is UNKNOWN, never "there is no briefing" —
      // say which it is rather than rendering an empty panel.
      setError(e instanceof Error ? e.message : String(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  }, [apiBase]);

  const toggle = useCallback(() => {
    const next = !expanded;
    setExpanded(next);
    // Fetch on first expand, and never on mount.
    if (next && !data && !loading) {
      void load();
    }
  }, [expanded, data, loading, load]);

  return (
    <div className="rounded-lg bg-card/50 overflow-hidden">
      <button
        onClick={toggle}
        className="w-full p-4 flex items-center justify-between hover:bg-muted/30 transition-colors"
      >
        <div className="flex items-center gap-3">
          <FileText className="w-5 h-5 text-primary" />
          <div className="text-left">
            <h4 className="font-medium text-sm">Session Briefing (read-only)</h4>
            <p className="text-xs text-muted-foreground">
              The exact system-prompt text this runner appends to every session it hosts. Edit it in
              the web console under Coord → Prompt Documents; edits reach sessions spawned after the
              next runner poll (≤ 45 s), and sessions already running keep the prompt they started
              with.
            </p>
          </div>
        </div>
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
        )}
      </button>

      {expanded && (
        <div className="px-4 pb-4 space-y-3">
          <div className="flex justify-end">
            <button
              onClick={() => void load()}
              disabled={loading}
              className="px-2.5 py-1.5 text-xs rounded-md bg-muted/50 hover:bg-muted transition-colors disabled:opacity-50 flex items-center gap-1.5"
            >
              <RefreshCw className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`} />
              Refresh
            </button>
          </div>

          {error && (
            <p className="text-xs text-destructive">
              Could not read the briefing from the runner API: {error}. This says nothing about what
              the briefing contains — only that this panel could not reach the runner.
            </p>
          )}

          {loading && !data && (
            <p className="text-xs text-muted-foreground">Reading the briefing from the runner…</p>
          )}

          {data && (
            <>
              <BlockView
                title="Interactive + autonomous sessions"
                subtitle={`Injected as QONTINUI_RUNNER_CONTEXT and --append-system-prompt · API port ${data.api_port}`}
                block={data}
              />
              <PlanCaptureClauseView clause={data.plan_capture_clause} />
              <BlockView
                title="Runner-triggered AI sessions"
                subtitle="A separate, higher-authority rules block on a spawn seam the briefing above never reaches"
                block={data.ai_session_rules}
              />
            </>
          )}
        </div>
      )}
    </div>
  );
}
