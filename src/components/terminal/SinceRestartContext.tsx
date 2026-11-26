/**
 * State owner for the "Since restart" roster (plan
 * `2026-10-04-runner-session-roster-restore-picker`, Phases 4-5): the ledger
 * report, the review selection, the sequential resume queue, Finish/Unfinish
 * and "Capture now".
 *
 * One provider feeds BOTH surfaces — the strip over the terminal grid and the
 * review section at the top of Previous Sessions — so a resume started from
 * one shows its progress in the other. It is its own component so its polling
 * and progress updates re-render only those two consumers, never the terminal
 * page it sits in. The decisions themselves are pure, in `sinceRestart.ts`.
 */

import { describeThrown } from "@/lib/utils";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";
import type { ResourceGuardSource } from "@/lib/resourceGuard";
import {
  captureSessionLedger,
  getSessionLedgerReport,
  type LedgerCapture,
  type LedgerGeneration,
  type LedgerReport,
  type ResumeAccount,
} from "@/lib/session-ledger";
import {
  buildRows,
  type FinishOp,
  defaultGenerationFile,
  thisBootGeneration,
  EMPTY_SELECTION,
  reconcileSelection,
  ResumeQueue,
  resumeTargetFor,
  selectedResumable,
  stripModel,
  type ResumeProgress,
  type RowContext,
  type SelectionState,
  type SinceRestartRow,
  type StripModel,
} from "./sinceRestart";
import { describeResumeFailure, type ResumeAttempt, type ResumeTarget } from "./resumeInNewTab";
import { accountRosterOf, loadDefaultConfigHome } from "./defaultConfigHome";
import type { NeedsAccountRecord } from "./useTerminalInitialization";
import { loadKnownPageIds } from "./useTerminalPages";
import { useFinishOps } from "./useFinishOps";
import type { CommandResponse } from "./types";

/** `instanceStorage` keys — per runner instance, like every view preference. */
const REVIEWED_KEY = "terminal.since-restart.reviewed";
const DISMISSED_KEY = "terminal.since-restart.dismissed";
/** Retained generations are capped at 5; a few more covers rotation churn. */
const REMEMBERED_FILES = 16;
/** The roster refreshes on its own this often while the page is mounted. */
const REPORT_POLL_MS = 60_000;

/** The panel shows the current roster ("now") or one retained generation. */
export const CURRENT_ROSTER = "now";

function loadSet(key: string): Set<string> {
  const raw = instanceStorage.getJSON<unknown>(key, []);
  return new Set(Array.isArray(raw) ? raw.filter((v): v is string => typeof v === "string") : []);
}

function saveSet(key: string, set: ReadonlySet<string>): void {
  instanceStorage.setJSON(key, [...set].slice(-REMEMBERED_FILES));
}

export interface SinceRestartValue {
  report: LedgerReport | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;

  /** The strip's model, or null when it shows nothing (dismissed included). */
  strip: StripModel | null;
  dismissStrip: () => void;
  /** Open the review panel on the strip's generation. */
  review: () => void;
  /** Moves on every `review()` — the section scrolls itself to the top on it. */
  reviewSeq: number;

  /** `CURRENT_ROSTER` or a generation file. */
  view: string;
  setView: (view: string) => void;
  generation: LedgerGeneration | null;
  rows: SinceRestartRow[];
  /** Mark the shown generation reviewed — the panel falls back to the current roster. */
  markReviewed: () => void;

  selected: ReadonlySet<string>;
  toggleSelected: (claudeSessionId: string) => void;
  /** Rows "Resume selected" would act on now. */
  resumable: SinceRestartRow[];
  resumeSelected: () => void;
  retry: (claudeSessionId: string) => void;
  progress: ReadonlyMap<string, ResumeProgress>;
  /** A resume queue is running. */
  resuming: boolean;

  /**
   * The `claude_config_dirs` roster for the account chooser, WITHOUT the default
   * home (the chooser offers that as its own entry); null until read.
   */
  accountRoster: string[] | null;
  /** The default Claude home's path (`claude_default_config_home`); null until read or unknown. */
  defaultConfigHome: string | null;
  chooseAccount: (claudeSessionId: string, account: ResumeAccount) => void;

  finishOps: ReadonlyMap<string, FinishOp>;
  setFinished: (claudeSessionId: string, finished: boolean) => void;
  /** Moves on every settled Finish/Unfinish made through this roster. */
  finishEpoch: number;

  capture: () => void;
  capturing: boolean;
  lastCapture: LedgerCapture | null;
  captureError: string | null;
}

const SinceRestartContext = createContext<SinceRestartValue | null>(null);

/** The roster state, or null outside a provider (the surfaces then render nothing). */
export function useSinceRestart(): SinceRestartValue | null {
  return useContext(SinceRestartContext);
}

export interface SinceRestartProviderProps {
  children: ReactNode;
  needsAccountRecords: readonly NeedsAccountRecord[];
  restoreCompletePages: ReadonlySet<string>;
  restoreDeferredPages: ReadonlySet<string>;
  /** THE resume path (`resumeInNewTab`), shared with every one-click Resume. */
  resume: (target: ResumeTarget, spawnSource: ResourceGuardSource) => Promise<ResumeAttempt>;
  /** Bring the Terminal page up and open Previous Sessions in its session sidebar. */
  openPrevious: () => void;
}

export function SinceRestartProvider({
  children,
  needsAccountRecords,
  restoreCompletePages,
  restoreDeferredPages,
  resume,
  openPrevious,
}: SinceRestartProviderProps) {
  const [report, setReport] = useState<LedgerReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // `load` touches state only when the read settles; `refresh` (the button)
  // also shows the spinner.
  const load = useCallback(
    () =>
      getSessionLedgerReport()
        .then((r) => {
          setReport(r);
          setError(null);
        })
        .catch((err: unknown) =>
          setError(`Could not read the session roster: ${describeThrown(err, "unknown error")}`),
        )
        .finally(() => setLoading(false)),
    [],
  );
  const refresh = useCallback(() => {
    setLoading(true);
    void load();
  }, [load]);

  // Read on mount, whenever a page's restore settles (that is what moves rows
  // from waiting to back/missing), and on a slow poll.
  useEffect(() => {
    void load();
  }, [load, restoreCompletePages, restoreDeferredPages]);
  useEffect(() => {
    const t = setInterval(() => void load(), REPORT_POLL_MS);
    return () => clearInterval(t);
  }, [load]);

  const needsAccount = useMemo(
    () => new Map(needsAccountRecords.map((r) => [r.record.claudeSessionId, r.reason])),
    [needsAccountRecords],
  );
  const [accountChoices, setAccountChoices] = useState<Map<string, ResumeAccount>>(new Map());

  const rowContext = useMemo<RowContext>(
    () => ({
      progress: {
        completePages: restoreCompletePages,
        deferredPages: restoreDeferredPages,
        knownPageIds: loadKnownPageIds(),
      },
      needsAccount,
      accountChoices,
    }),
    // `report` is a dependency on purpose: the persisted page list is re-read
    // whenever the roster is.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [restoreCompletePages, restoreDeferredPages, needsAccount, accountChoices, report],
  );

  // ── strip ────────────────────────────────────────────────────────────────
  const [dismissed, setDismissed] = useState<Set<string>>(() => loadSet(DISMISSED_KEY));
  const rawStrip = useMemo(() => stripModel(report, rowContext), [report, rowContext]);
  // Per BOOT as well as per generation: a restart whose roster equals the
  // last one retires no new generation file, and its strip must still show.
  const stripKey = rawStrip
    ? `${report?.current.bootAtMs ?? "?"}:${rawStrip.generationFile}:${rawStrip.kind}`
    : null;
  const strip = rawStrip && stripKey && !dismissed.has(stripKey) ? rawStrip : null;
  const dismissStrip = useCallback(() => {
    if (!stripKey) return;
    setDismissed((prev) => {
      const next = new Set(prev).add(stripKey);
      saveSet(DISMISSED_KEY, next);
      return next;
    });
  }, [stripKey]);

  // ── which generation the panel shows ─────────────────────────────────────
  const [reviewed, setReviewed] = useState<Set<string>>(() => loadSet(REVIEWED_KEY));
  const [chosenView, setChosenView] = useState<string | null>(null);
  const defaultView = defaultGenerationFile(report, reviewed) ?? CURRENT_ROSTER;
  // A chosen generation that rotated out of retention falls back to the default.
  const view =
    chosenView !== null &&
    (chosenView === CURRENT_ROSTER || report?.generations.some((g) => g.file === chosenView))
      ? chosenView
      : defaultView;
  const generation = report?.generations.find((g) => g.file === view) ?? null;
  const rows = useMemo(
    () => (generation ? buildRows(generation, rowContext) : []),
    [generation, rowContext],
  );

  const [reviewSeq, setReviewSeq] = useState(0);
  const review = useCallback(() => {
    if (rawStrip) setChosenView(rawStrip.generationFile);
    openPrevious();
    setReviewSeq((n) => n + 1);
  }, [rawStrip, openPrevious]);

  const markReviewed = useCallback(() => {
    if (!generation) return;
    setReviewed((prev) => {
      const next = new Set(prev).add(generation.file);
      saveSet(REVIEWED_KEY, next);
      return next;
    });
    setChosenView(CURRENT_ROSTER);
  }, [generation]);

  // ── selection: pre-checked per row as it becomes resumable, then the
  // operator's. A row first seen while its page was still restoring is
  // pre-checked when it settles; a row the operator un-checked stays so. ─────
  const [selection, setSelection] = useState<SelectionState>(EMPTY_SELECTION);
  // Only the NEWEST generation is pre-checked; an older one is the operator's
  // to tick (its sessions may have been dealt with since).
  const nextSelection = reconcileSelection(
    selection,
    generation ? generation.file : null,
    rows,
    generation !== null && generation.file === thisBootGeneration(report)?.file,
  );
  if (nextSelection !== selection) setSelection(nextSelection);
  const selected = selection.ids;
  const toggleSelected = useCallback((id: string) => {
    setSelection((prev) => {
      const ids = new Set(prev.ids);
      if (ids.has(id)) ids.delete(id);
      else ids.add(id);
      return { ...prev, ids };
    });
  }, []);

  const chooseAccount = useCallback((id: string, account: ResumeAccount) => {
    setAccountChoices((prev) => new Map(prev).set(id, account));
    // Not pre-checked UNTIL an account is chosen — choosing one is the opt-in.
    setSelection((prev) => ({ ...prev, ids: new Set(prev.ids).add(id) }));
  }, []);

  // ── the default home: a resume records it explicitly (so the account stops
  // reading unknown) and the typed command drops it (`runVerifiedResume`) ────
  const [defaultConfigHome, setDefaultConfigHome] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    void loadDefaultConfigHome().then((home) => {
      if (alive) setDefaultConfigHome(home);
    });
    return () => {
      alive = false;
    };
  }, []);

  // ── account roster for the chooser ───────────────────────────────────────
  const [configDirs, setConfigDirs] = useState<string[] | null>(null);
  const wantsRoster = rows.some((r) => r.state === "needs-account");
  useEffect(() => {
    if (!wantsRoster || configDirs !== null) return;
    invoke<CommandResponse>("get_claude_config_dirs")
      .then((res) => {
        const dirs = (res?.data as { dirs?: unknown } | undefined)?.dirs;
        setConfigDirs(
          Array.isArray(dirs) ? dirs.filter((d): d is string => typeof d === "string") : [],
        );
      })
      .catch(() => setConfigDirs([]));
  }, [wantsRoster, configDirs]);
  const accountRoster = useMemo(
    () => (configDirs === null ? null : accountRosterOf(configDirs, defaultConfigHome)),
    [configDirs, defaultConfigHome],
  );

  // ── the sequential resume queue ──────────────────────────────────────────
  const [progress, setProgress] = useState<Map<string, ResumeProgress>>(new Map());
  const [running, setRunning] = useState(0);
  // ONE queue per mount, cancelled on unmount so nothing spawns after the
  // panel is gone (the remaining items read `failed: cancelled`). Created in
  // the effect, so StrictMode's mount → unmount → mount gets a live queue.
  const queueRef = useRef<ResumeQueue | null>(null);
  useEffect(() => {
    const queue = new ResumeQueue();
    queueRef.current = queue;
    return () => {
      queue.cancel();
      if (queueRef.current === queue) queueRef.current = null;
    };
  }, []);
  const rowsRef = useRef(rows);
  useEffect(() => {
    rowsRef.current = rows;
  }, [rows]);
  const defaultHomeRef = useRef(defaultConfigHome);
  useEffect(() => {
    defaultHomeRef.current = defaultConfigHome;
  }, [defaultConfigHome]);

  const enqueue = useCallback(
    (ids: string[]) => {
      const queue = queueRef.current;
      if (!queue || ids.length === 0) return;
      // Snapshot the targets now: the rows re-derive as the roster refreshes
      // mid-queue, and a row that came back must not change what was asked.
      const targets = new Map<string, ResumeTarget>();
      for (const id of ids) {
        const row = rowsRef.current.find((r) => r.outcome.claudeSessionId === id);
        const target = row ? resumeTargetFor(row, defaultHomeRef.current) : null;
        if (target) targets.set(id, target);
      }
      const onProgress = (id: string, p: ResumeProgress) =>
        setProgress((prev) => new Map(prev).set(id, p));
      const queueIds = [...targets.keys()];
      const { admitted, done } = queue.enqueue(
        queueIds,
        async (id) => {
          const target = targets.get(id);
          if (!target) return { ok: false, reason: "no longer resumable" };
          const remaining = queueIds.length - queueIds.indexOf(id);
          const attempt = await resume(target, {
            label: "since-restart resume",
            queued: remaining,
          });
          return attempt.kind === "verified"
            ? { ok: true }
            : { ok: false, reason: describeResumeFailure(attempt.failure) };
        },
        onProgress,
      );
      if (admitted.length === 0) return;
      setRunning((n) => n + 1);
      void done.finally(() => {
        setRunning((n) => n - 1);
        refresh();
      });
    },
    [resume, refresh],
  );

  const resumable = useMemo(() => {
    const busy = (id: string) => {
      const p = progress.get(id)?.state;
      return p === "queued" || p === "resuming" || p === "back";
    };
    return selectedResumable(rows, selected).filter((r) => !busy(r.outcome.claudeSessionId));
  }, [rows, selected, progress]);

  const resumeSelected = useCallback(() => {
    enqueue(resumable.map((r) => r.outcome.claudeSessionId));
  }, [enqueue, resumable]);
  const retry = useCallback((id: string) => enqueue([id]), [enqueue]);

  // ── Finish / Unfinish ────────────────────────────────────────────────────
  // `finishEpoch` moves on every settled Finish/Unfinish, so Previous
  // Sessions' own list (a different read) can follow a change made here.
  const [finishEpoch, setFinishEpoch] = useState(0);
  const onFinishSettled = useCallback(() => {
    setFinishEpoch((n) => n + 1);
    refresh();
  }, [refresh]);
  const { ops: finishOps, setFinished } = useFinishOps(onFinishSettled);

  // ── Capture now ──────────────────────────────────────────────────────────
  const [capturing, setCapturing] = useState(false);
  const [lastCapture, setLastCapture] = useState<LedgerCapture | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);
  const capture = useCallback(() => {
    setCapturing(true);
    setCaptureError(null);
    captureSessionLedger()
      .then(setLastCapture)
      .catch((err: unknown) => setCaptureError(describeThrown(err, "capture failed")))
      .finally(() => {
        setCapturing(false);
        refresh();
      });
  }, [refresh]);

  const value = useMemo<SinceRestartValue>(
    () => ({
      report,
      loading,
      error,
      refresh,
      strip,
      dismissStrip,
      review,
      reviewSeq,
      view,
      setView: setChosenView,
      generation,
      rows,
      markReviewed,
      selected,
      toggleSelected,
      resumable,
      resumeSelected,
      retry,
      progress,
      resuming: running > 0,
      accountRoster,
      defaultConfigHome,
      chooseAccount,
      finishOps,
      setFinished,
      finishEpoch,
      capture,
      capturing,
      lastCapture,
      captureError,
    }),
    [
      report,
      loading,
      error,
      refresh,
      strip,
      dismissStrip,
      review,
      reviewSeq,
      view,
      generation,
      rows,
      markReviewed,
      selected,
      toggleSelected,
      resumable,
      resumeSelected,
      retry,
      progress,
      running,
      accountRoster,
      defaultConfigHome,
      chooseAccount,
      finishOps,
      setFinished,
      finishEpoch,
      capture,
      capturing,
      lastCapture,
      captureError,
    ],
  );

  return <SinceRestartContext.Provider value={value}>{children}</SinceRestartContext.Provider>;
}
