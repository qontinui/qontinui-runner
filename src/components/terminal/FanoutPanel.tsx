/**
 * The `PromptModal` "Fan out…" mode (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
 * Phase 7): one prompt template × a matrix → a preview of EVERY member, then
 * one `POST /fanout` carrying exactly the ticked preview rows.
 *
 * Every rule that decides what is shown or whether Create is enabled lives in
 * `fanoutPlan.ts` (pure, unit-tested); this component holds form state, runs
 * the per-member collision probe, and posts.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, ChevronDown, ChevronRight, Layers, Loader2 } from "lucide-react";

import { resolvePort } from "@/lib/runner-api";

import { createFanoutRun, type ConfigDirPolicy } from "./fanoutApi";
import {
  FANOUT_DEFAULT_MAX_CONCURRENT,
  PROBE_CONCURRENCY,
  alreadyCreatedReason,
  buildCreateFanoutRequest,
  collisionProbeLabel,
  defaultTitleTemplate,
  detectPathPlatform,
  fanoutGate,
  judgeFanoutCreate,
  planPreview,
  probeKey,
  promptsToProbe,
  rowErrorLabel,
  rowIsBlocked,
  rowWarningLabel,
  runWithConcurrency,
  sharedCwdWarning,
  type CollisionProbeState,
  type FanoutCreateVerdict,
} from "./fanoutPlan";
import { buildProbeBody, buildProbeUrl } from "./LaunchMenu";
import { parseMatrix, type MatrixMode } from "./promptMatrix";
import type { PromptTemplate } from "./promptLibraryApi";
import type { PromptParamValues } from "./renderPromptTemplate";
import type { ConflictReport } from "./useSessionManager";

/** One account the operator may pin every member to. */
export interface FanoutAccountOption {
  configDir: string;
  label: string;
}

/** What the Terminal page supplies to the fan-out mode. */
export interface FanoutModalContext {
  /** Configured accounts, best-headroom first (the `spawnWithPromptText` order). */
  accounts: FanoutAccountOption[];
  /** The directory a normal launch on this page opens in. */
  defaultWorkingDir: string;
  /** The tenant a normal launch on this page would stamp, when one is pinned. */
  tenantId?: string | null;
  /** Called once a run was created and verified. */
  onCreated?: (runId: string) => void;
}

/** Debounce before the per-member collision probes fire. */
const PROBE_DEBOUNCE_MS = 600;

const inputClass =
  "w-full bg-[#13141f] border border-[#2a2d3d] rounded px-2.5 py-1.5 text-[12px] text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] transition-colors";

const TONE_COLOR = { muted: "#565f89", warn: "#e0af68", ok: "#9ece6a", unknown: "#e0af68" };

export function FanoutPanel({
  template,
  fixedValues,
  context,
}: {
  template: PromptTemplate;
  fixedValues: PromptParamValues;
  context: FanoutModalContext;
}) {
  const [matrixText, setMatrixText] = useState("");
  const [mode, setMode] = useState<MatrixMode>("zip");
  const [maxConcurrentText, setMaxConcurrentText] = useState(String(FANOUT_DEFAULT_MAX_CONCURRENT));
  /** `"best"` → best headroom per member; otherwise a fixed config dir. */
  const [accountChoice, setAccountChoice] = useState<string>("best");
  /** `null` → follow the default derived from the current axes. */
  const [titleTemplateEdit, setTitleTemplateEdit] = useState<string | null>(null);
  const [workingDir, setWorkingDir] = useState(context.defaultWorkingDir);
  /** Unticked row indices, scoped to the matrix they were unticked under. */
  const [unticked, setUnticked] = useState<{ scope: string; set: Set<number> }>({
    scope: "",
    set: new Set(),
  });
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const [probes, setProbes] = useState<Record<string, CollisionProbeState>>({});
  /** Answered probes, readable by the probe effect without re-running it. */
  const probesRef = useRef(probes);
  useEffect(() => {
    probesRef.current = probes;
  }, [probes]);
  /** Probe keys with a request in flight. */
  const inFlightRef = useRef<Set<string>>(new Set());
  const [creating, setCreating] = useState(false);
  const [verdict, setVerdict] = useState<FanoutCreateVerdict | null>(null);
  /**
   * Every request body this modal created, → its run id. Never cleared by an
   * edit: reverting to a created body must not enable a second create.
   */
  const [createdRuns, setCreatedRuns] = useState<ReadonlyMap<string, string>>(new Map());

  // A changed default dir (the page resolved its home dir late) seeds the
  // field only while the operator has not typed into it.
  const [seededDir, setSeededDir] = useState(context.defaultWorkingDir);
  if (context.defaultWorkingDir !== seededDir) {
    setSeededDir(context.defaultWorkingDir);
    if (workingDir === seededDir) setWorkingDir(context.defaultWorkingDir);
  }

  // The default title names every axis, so it follows the matrix until the
  // operator edits it.
  const autoTitle = useMemo(() => {
    const parsed = parseMatrix(matrixText);
    return parsed.ok ? defaultTitleTemplate(template.name, parsed.axes) : template.name;
  }, [matrixText, template.name]);
  const titleTemplate = titleTemplateEdit ?? autoTitle;

  const preview = useMemo(
    () => planPreview({ template, fixedValues, matrixText, mode, titleTemplate }),
    [template, fixedValues, matrixText, mode, titleTemplate],
  );
  const rows = useMemo(() => (preview.kind === "ok" ? preview.rows : []), [preview]);

  // Unticks belong to one expansion; a new matrix or mode starts all-ticked.
  const scope = `${mode}\u0000${matrixText}`;
  const untickedSet = useMemo(
    () => (unticked.scope === scope ? unticked.set : new Set<number>()),
    [unticked, scope],
  );
  const ticked = useMemo(
    () => new Set(rows.map((r) => r.index).filter((i) => !untickedSet.has(i))),
    [rows, untickedSet],
  );

  const maxConcurrent = Number(maxConcurrentText);
  const policy: ConfigDirPolicy =
    accountChoice === "best"
      ? { kind: "bestHeadroom" }
      : { kind: "fixed", configDir: accountChoice };
  const platform = detectPathPlatform(
    typeof navigator === "undefined" ? undefined : navigator.platform,
  );
  const gate = fanoutGate({ rows, ticked, workingDir, maxConcurrent, policy, platform });
  // The exact body Create would post. A create verdict belongs to the body it
  // was made for: any change to any input re-enables Create.
  const request = buildCreateFanoutRequest({
    rows,
    ticked,
    templateSlug: template.name,
    templateVersion: template.version,
    maxConcurrent,
    policy,
    workingDir,
    tenantId: context.tenantId,
  });
  const requestKey = JSON.stringify(request);
  /** Create stays disabled for any body this modal already created. */
  const createdReason = alreadyCreatedReason(createdRuns, requestKey);
  const created = createdReason !== null;
  const blockedReason = gate.reason ?? createdReason;
  const isolationWarning = sharedCwdWarning(gate.tickedCount, workingDir);

  // Per-member collision probe — the LaunchMenu probe, once per ticked member.
  const cwd = workingDir.trim();
  const probeList = useMemo(() => {
    const want: string[] = [];
    for (const r of rows) {
      if (!ticked.has(r.index) || r.prompt.trim().length === 0) continue;
      if (!want.includes(r.prompt)) want.push(r.prompt);
    }
    return want;
  }, [rows, ticked]);
  // A string key, so a re-render that rebuilds an equal list does not abort
  // and re-fire the probes it already has in flight.
  const probeListKey = JSON.stringify(probeList);

  useEffect(() => {
    const wanted = JSON.parse(probeListKey) as string[];
    if (wanted.length === 0) return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      // Only prompts with no answer and no request in flight: an edit that
      // leaves most rows as they were re-probes none of them.
      const prompts = promptsToProbe(wanted, cwd, probesRef.current, inFlightRef.current);
      const port = resolvePort();
      const probeOne = async (prompt: string) => {
        const key = probeKey(cwd, prompt);
        inFlightRef.current.add(key);
        let result: CollisionProbeState | null;
        try {
          const resp = await fetch(buildProbeUrl(port), {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: buildProbeBody(prompt, cwd),
            signal: controller.signal,
          });
          if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
          const report = (await resp.json()) as ConflictReport;
          if (!Array.isArray(report?.predicted_collisions)) {
            throw new Error("unexpected probe response shape");
          }
          result = { kind: "ok", report };
        } catch (err: unknown) {
          result = controller.signal.aborted
            ? null
            : { kind: "unknown", error: err instanceof Error ? err.message : String(err) };
        } finally {
          inFlightRef.current.delete(key);
        }
        if (result && !controller.signal.aborted) {
          const settled = result;
          setProbes((prev) => ({ ...prev, [key]: settled }));
        }
      };
      void runWithConcurrency(prompts, PROBE_CONCURRENCY, probeOne, controller.signal);
    }, PROBE_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [probeListKey, cwd]);

  const toggleTick = (index: number) => {
    const next = new Set(untickedSet);
    if (next.has(index)) next.delete(index);
    else next.add(index);
    setUnticked({ scope, set: next });
    setVerdict(null);
  };

  const toggleExpand = (index: number) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(index)) next.delete(index);
      else next.add(index);
      return next;
    });

  const handleCreate = async () => {
    if (!gate.canCreate || creating || created) return;
    const req = request;
    const key = requestKey;
    setCreating(true);
    setVerdict(null);
    try {
      const result = await createFanoutRun(req);
      const v: FanoutCreateVerdict = result.ok
        ? judgeFanoutCreate(req.members.length, result.data)
        : {
            ok: false,
            message:
              result.status !== null ? `HTTP ${result.status}: ${result.error}` : result.error,
          };
      setVerdict(v);
      if (v.ok) {
        const runId = v.runId;
        setCreatedRuns((prev) => new Map(prev).set(key, runId));
        context.onCreated?.(runId);
      }
    } finally {
      setCreating(false);
    }
  };

  return (
    <div data-ui-bridge-id="terminal.fanout-preview" className="flex flex-col gap-3">
      <div className="grid grid-cols-2 gap-2.5">
        <label className="flex flex-col gap-1 col-span-2">
          <span className="text-[11px] text-[#a9b1d6]">
            Matrix <span className="text-[#565f89]">— axis:value,value;axis:value,value</span>
          </span>
          <input
            data-ui-bridge-id="terminal.fanout-matrix"
            value={matrixText}
            onChange={(e) => {
              setMatrixText(e.target.value);
              setVerdict(null);
            }}
            placeholder="platform:iOS,Android;lang:swift,kotlin"
            className={`${inputClass} font-mono`}
          />
        </label>
        <div className="flex flex-col gap-1">
          <span className="text-[11px] text-[#a9b1d6]">Combine axes</span>
          <div className="flex gap-1">
            {(["zip", "product"] as const).map((m) => (
              <button
                key={m}
                type="button"
                data-ui-bridge-id={`terminal.fanout-mode-${m}`}
                aria-pressed={mode === m}
                onClick={() => {
                  setMode(m);
                  setVerdict(null);
                }}
                className={`flex-1 px-2 py-1.5 rounded border text-[11px] transition-colors ${
                  mode === m
                    ? "border-[#7aa2f7] text-[#c0caf5] bg-[#24283b]"
                    : "border-[#2a2d3d] text-[#565f89] hover:text-[#a9b1d6]"
                }`}
                title={
                  m === "zip"
                    ? "Member i takes value i of every axis (axes must be the same length)"
                    : "Every combination of every axis"
                }
              >
                {m}
              </button>
            ))}
          </div>
        </div>
        <label className="flex flex-col gap-1">
          <span className="text-[11px] text-[#a9b1d6]">Max concurrent</span>
          <input
            data-ui-bridge-id="terminal.fanout-max-concurrent"
            type="number"
            min={1}
            value={maxConcurrentText}
            onChange={(e) => setMaxConcurrentText(e.target.value)}
            className={inputClass}
          />
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-[11px] text-[#a9b1d6]">Account</span>
          <select
            data-ui-bridge-id="terminal.fanout-account-policy"
            value={accountChoice}
            onChange={(e) => setAccountChoice(e.target.value)}
            className={inputClass}
          >
            <option value="best">Best headroom, picked per member at admission</option>
            {context.accounts.map((a) => (
              <option key={a.configDir} value={a.configDir}>
                Every member: {a.label}
              </option>
            ))}
          </select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-[11px] text-[#a9b1d6]">Title template</span>
          <input
            data-ui-bridge-id="terminal.fanout-title-template"
            value={titleTemplate}
            onChange={(e) => setTitleTemplateEdit(e.target.value)}
            className={`${inputClass} font-mono`}
          />
        </label>
        <label className="flex flex-col gap-1 col-span-2">
          <span className="text-[11px] text-[#a9b1d6]">Working directory</span>
          <input
            data-ui-bridge-id="terminal.fanout-working-dir"
            value={workingDir}
            onChange={(e) => setWorkingDir(e.target.value)}
            placeholder="Absolute path every member starts in"
            className={`${inputClass} font-mono`}
          />
        </label>
      </div>

      {preview.kind === "error" ? (
        <div
          data-ui-bridge-id="terminal.fanout-matrix-error"
          className="flex items-start gap-1.5 text-[11px] text-[#e0af68]"
        >
          <AlertTriangle className="w-3.5 h-3.5 shrink-0 mt-px" />
          <span>{preview.message}</span>
        </div>
      ) : (
        <div className="border border-[#2a2d3d] rounded overflow-hidden">
          <div className="grid grid-cols-[28px_32px_minmax(0,1fr)_minmax(0,1.4fr)_110px] gap-2 px-2 py-1.5 bg-[#13141f] text-[10px] uppercase tracking-wider text-[#565f89]">
            <span />
            <span>#</span>
            <span>Title</span>
            <span>Problems</span>
            <span>Collisions</span>
          </div>
          <div className="max-h-[260px] overflow-y-auto scrollbar-dark">
            {rows.map((r) => {
              const isTicked = ticked.has(r.index);
              const blocked = rowIsBlocked(r);
              const isOpen = expanded.has(r.index);
              const probe = collisionProbeLabel(probes[probeKey(cwd, r.prompt)]);
              const problems = [
                ...r.missing.map((m) => ({ text: `missing ${m}`, error: true })),
                ...r.errors.map((e) => ({ text: rowErrorLabel(e), error: true })),
                ...r.warnings.map((w) => ({ text: rowWarningLabel(w), error: false })),
              ];
              return (
                <div
                  key={r.index}
                  data-ui-bridge-id="terminal.fanout-preview-row"
                  data-row-index={r.index}
                  data-row-ticked={isTicked ? "true" : "false"}
                  data-row-blocked={blocked ? "true" : "false"}
                  className={`border-t border-[#2a2d3d]/60 ${isTicked ? "" : "opacity-50"}`}
                >
                  <div className="grid grid-cols-[28px_32px_minmax(0,1fr)_minmax(0,1.4fr)_110px] gap-2 px-2 py-1.5 items-start text-[11px]">
                    <input
                      type="checkbox"
                      data-ui-bridge-id="terminal.fanout-preview-row-tick"
                      data-row-index={r.index}
                      checked={isTicked}
                      onChange={() => toggleTick(r.index)}
                      className="accent-[#7aa2f7] mt-0.5"
                      aria-label={`Include member ${r.index + 1}`}
                    />
                    <button
                      type="button"
                      data-ui-bridge-id="terminal.fanout-preview-row-expand"
                      data-row-index={r.index}
                      onClick={() => toggleExpand(r.index)}
                      className="flex items-center gap-0.5 text-[#565f89] hover:text-[#a9b1d6] font-mono"
                      title="Show the rendered prompt"
                    >
                      {isOpen ? (
                        <ChevronDown className="w-3 h-3" />
                      ) : (
                        <ChevronRight className="w-3 h-3" />
                      )}
                      {r.index + 1}
                    </button>
                    <span className="truncate text-[#c0caf5]" title={r.title}>
                      {r.title || <span className="text-[#565f89]">(blank)</span>}
                    </span>
                    <span className="flex flex-wrap gap-x-2 gap-y-0.5">
                      {problems.length === 0 ? (
                        <span className="text-[#565f89]">—</span>
                      ) : (
                        problems.map((p, i) => (
                          <span key={i} className={p.error ? "text-[#f7768e]" : "text-[#e0af68]"}>
                            {p.text}
                          </span>
                        ))
                      )}
                    </span>
                    <span
                      data-ui-bridge-id="terminal.fanout-preview-row-collisions"
                      data-row-index={r.index}
                      style={{ color: TONE_COLOR[probe.tone] }}
                      title={probe.title}
                    >
                      {isTicked ? probe.text : "—"}
                    </span>
                  </div>
                  {isOpen && (
                    <pre
                      data-ui-bridge-id="terminal.fanout-preview-row-prompt"
                      data-row-index={r.index}
                      className="mx-2 mb-2 p-2.5 bg-[#13141f] border border-[#2a2d3d] rounded text-[11px] text-[#a9b1d6] whitespace-pre-wrap break-words max-h-[160px] overflow-y-auto scrollbar-dark"
                    >
                      {r.prompt}
                    </pre>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}

      {isolationWarning && (
        <div
          data-ui-bridge-id="terminal.fanout-isolation-warning"
          className="flex items-start gap-1.5 text-[10px] text-[#e0af68]"
        >
          <AlertTriangle className="w-3 h-3 shrink-0 mt-px" />
          <span>{isolationWarning}</span>
        </div>
      )}

      <div className="flex items-center gap-3">
        <button
          type="button"
          data-ui-bridge-id="terminal.fanout-create"
          onClick={() => void handleCreate()}
          disabled={!gate.canCreate || creating || created}
          title={blockedReason ?? `Queue ${gate.tickedCount} sessions, ${maxConcurrent} at a time`}
          className="flex items-center justify-center gap-2 px-4 py-2 bg-[#7aa2f7] hover:bg-[#6a92e7] disabled:bg-[#2a2d3d] disabled:text-[#565f89] text-[#1a1b26] text-sm font-medium rounded transition-colors"
        >
          {creating ? <Loader2 className="w-4 h-4 animate-spin" /> : <Layers className="w-4 h-4" />}
          {created
            ? "Created"
            : `Create ${gate.tickedCount} session${gate.tickedCount === 1 ? "" : "s"}`}
        </button>
        {blockedReason && (
          <span
            data-ui-bridge-id="terminal.fanout-create-blocked"
            className="text-[10px] text-[#565f89]"
          >
            {blockedReason}
          </span>
        )}
      </div>

      {verdict && (
        <div
          data-ui-bridge-id="terminal.fanout-create-result"
          data-result={verdict.ok ? "ok" : "failed"}
          className={`text-[11px] ${verdict.ok ? "text-[#9ece6a]" : "text-[#f7768e]"}`}
        >
          {verdict.ok ? (
            <>
              <div>
                {verdict.message} — run {verdict.runId.slice(0, 8)}. Track it in the status strip.
              </div>
              {verdict.clampNote && <div className="text-[#e0af68]">{verdict.clampNote}</div>}
            </>
          ) : (
            <div>Fan-out not created: {verdict.message}</div>
          )}
        </div>
      )}
    </div>
  );
}
