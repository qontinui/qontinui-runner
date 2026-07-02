/**
 * HelperTasksPage — Helper Task Queue owner surface (plan
 * 2026-06-29-helper-task-queue, Phase 1.3 Part E).
 *
 * Three sub-tabs:
 *   - "Config" — owner emit toggle (`helper_tasks.emit_enabled`, default OFF)
 *     plus per-kind checkboxes, wired to the `get_helper_tasks_settings` /
 *     `save_helper_tasks_settings` Tauri commands.
 *   - "Review" — the helper answers collected by the background poll
 *     (`get_helper_answers`), newest first.
 *   - "Invite" — static explainer + deep link to the web app's team invite
 *     page for adding a helper with the HELPER role.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import {
  CheckCircle2,
  HelpCircle,
  Loader2,
  MessageSquare,
  RefreshCw,
  Settings as SettingsIcon,
  ThumbsDown,
  ThumbsUp,
  UserPlus,
} from "lucide-react";

// ---------------------------------------------------------------------------
// Types (mirror the Rust shapes)
// ---------------------------------------------------------------------------

interface TauriResult<T> {
  success: boolean;
  message?: string;
  data?: T;
}

/** Mirrors `settings::HelperTasksSettings` (snake_case serde). */
interface HelperTasksSettings {
  emit_enabled: boolean;
  emit_kinds: string[];
}

/** Mirrors `qontinui_types::helper_task::HelperAnswer` (camelCase serde). */
interface HelperAnswer {
  id: string;
  taskId: string;
  helperUserId: string;
  verdict: string;
  reasons: string[];
  freeText?: string | null;
  createdAt: string;
}

/** Phase 1 ships spot_check only; the rest land with Phase 2/3. */
const KNOWN_KINDS: ReadonlyArray<{ id: string; label: string; available: boolean }> = [
  { id: "spot_check", label: "Spot check — “does this page look right?”", available: true },
  { id: "compare", label: "Compare — “which of these looks better?” (Phase 2)", available: false },
  { id: "walk_through", label: "Walk-through — guided flow check (Phase 2/3)", available: false },
];

const INVITE_URL = "https://qontinui.io/settings/team";

type SubTab = "config" | "review" | "invite";

const SUB_TABS: ReadonlyArray<{
  id: SubTab;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
}> = [
  { id: "config", label: "Config", icon: SettingsIcon },
  { id: "review", label: "Review", icon: MessageSquare },
  { id: "invite", label: "Invite", icon: UserPlus },
];

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export function HelperTasksPage(): React.JSX.Element {
  const [activeSubTab, setActiveSubTab] = useState<SubTab>("config");

  return (
    <div className="flex h-full flex-col">
      <div
        role="tablist"
        aria-label="Helper Tasks sub-tabs"
        className="flex items-center gap-1 border-b border-border-primary px-4"
      >
        {SUB_TABS.map((t) => {
          const Icon = t.icon;
          const isActive = t.id === activeSubTab;
          return (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={isActive}
              onClick={() => setActiveSubTab(t.id)}
              className={[
                "inline-flex items-center gap-1.5 px-3 py-2 text-sm font-medium border-b-2 -mb-px",
                isActive
                  ? "border-brand-primary text-text-primary"
                  : "border-transparent text-text-muted hover:text-text-primary",
              ].join(" ")}
            >
              <Icon className="size-3.5" />
              {t.label}
            </button>
          );
        })}
      </div>

      <div className="flex-1 overflow-auto">
        {activeSubTab === "config" && <ConfigTab />}
        {activeSubTab === "review" && <ReviewTab />}
        {activeSubTab === "invite" && <InviteTab />}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Config tab
// ---------------------------------------------------------------------------

function ConfigTab(): React.JSX.Element {
  const [settings, setSettings] = useState<HelperTasksSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedFlash, setSavedFlash] = useState(false);

  useEffect(() => {
    let cancelled = false;
    invoke<TauriResult<HelperTasksSettings>>("get_helper_tasks_settings")
      .then((result) => {
        if (cancelled) return;
        if (result?.success && result.data) {
          setSettings(result.data);
        } else {
          setError(result?.message ?? "Failed to load Helper Task settings");
        }
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const save = useCallback(async (next: HelperTasksSettings) => {
    setSettings(next);
    setSaving(true);
    setError(null);
    try {
      await invoke<TauriResult<void>>("save_helper_tasks_settings", {
        helperTasksSettings: next,
      });
      setSavedFlash(true);
      setTimeout(() => setSavedFlash(false), 1500);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }, []);

  if (loading) {
    return (
      <div className="flex items-center gap-2 p-6 text-sm text-text-muted">
        <Loader2 className="size-4 animate-spin" /> Loading settings…
      </div>
    );
  }

  const current: HelperTasksSettings = settings ?? {
    emit_enabled: false,
    emit_kinds: ["spot_check"],
  };

  const toggleKind = (kind: string, checked: boolean) => {
    const kinds = checked
      ? Array.from(new Set([...current.emit_kinds, kind]))
      : current.emit_kinds.filter((k) => k !== kind);
    void save({ ...current, emit_kinds: kinds });
  };

  return (
    <div className="max-w-2xl space-y-6 p-6">
      <div>
        <h2 className="text-base font-semibold text-text-primary">Helper Task emission</h2>
        <p className="mt-1 text-sm text-text-muted">
          When enabled, this runner emits small human-judgment tasks (e.g. “does this page look
          right?”) to your team&apos;s helpers whenever a spec-check lands in the partial-match
          (yellow) band. Answers flow back into reflection as high-signal fix targets. Default is
          off — nothing leaves the runner until you opt in.
        </p>
      </div>

      {error && (
        <div className="rounded border border-status-error/40 bg-status-error/10 p-3 text-sm text-status-error">
          {error}
        </div>
      )}

      <label className="flex items-center gap-3">
        <input
          type="checkbox"
          className="size-4"
          checked={current.emit_enabled}
          disabled={saving}
          onChange={(e) => void save({ ...current, emit_enabled: e.target.checked })}
        />
        <span className="text-sm font-medium text-text-primary">Emit helper tasks</span>
        {saving && <Loader2 className="size-3.5 animate-spin text-text-muted" />}
        {savedFlash && !saving && <CheckCircle2 className="size-3.5 text-status-success" />}
      </label>

      <div>
        <h3 className="text-sm font-medium text-text-primary">Task kinds</h3>
        <p className="mb-2 mt-0.5 text-xs text-text-muted">
          Which kinds of tasks this runner may emit.
        </p>
        <div className="space-y-2">
          {KNOWN_KINDS.map((kind) => (
            <label
              key={kind.id}
              className={["flex items-center gap-3", kind.available ? "" : "opacity-50"].join(" ")}
            >
              <input
                type="checkbox"
                className="size-4"
                checked={current.emit_kinds.includes(kind.id)}
                disabled={saving || !kind.available || !current.emit_enabled}
                onChange={(e) => toggleKind(kind.id, e.target.checked)}
              />
              <span className="text-sm text-text-primary">{kind.label}</span>
            </label>
          ))}
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Review tab
// ---------------------------------------------------------------------------

function verdictBadge(verdict: string): React.JSX.Element {
  switch (verdict) {
    case "approve":
      return (
        <span className="inline-flex items-center gap-1 rounded bg-status-success/15 px-2 py-0.5 text-xs font-medium text-status-success">
          <ThumbsUp className="size-3" /> approve
        </span>
      );
    case "reject":
      return (
        <span className="inline-flex items-center gap-1 rounded bg-status-error/15 px-2 py-0.5 text-xs font-medium text-status-error">
          <ThumbsDown className="size-3" /> reject
        </span>
      );
    default:
      return (
        <span className="inline-flex items-center gap-1 rounded bg-bg-tertiary px-2 py-0.5 text-xs font-medium text-text-muted">
          <HelpCircle className="size-3" /> {verdict}
        </span>
      );
  }
}

function ReviewTab(): React.JSX.Element {
  const {
    data,
    isFetching,
    error: queryError,
    refetch,
  } = useQuery<HelperAnswer[]>({
    queryKey: ["helper-answers"],
    queryFn: async () => {
      const result = await invoke<TauriResult<HelperAnswer[]>>("get_helper_answers");
      if (!result?.success || !result.data) {
        throw new Error(result?.message ?? "Failed to load helper answers");
      }
      return result.data;
    },
  });

  const answers = data ?? [];
  const loading = isFetching;
  const error = queryError ? String(queryError) : null;
  const refresh = () => void refetch();

  return (
    <div className="max-w-3xl space-y-4 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-base font-semibold text-text-primary">Helper answers</h2>
          <p className="mt-1 text-sm text-text-muted">
            Verdicts your helpers submitted for tasks this runner emitted. The runner polls coord
            every minute while emission is enabled; answers also feed the reflection agent
            automatically.
          </p>
        </div>
        <button
          type="button"
          onClick={refresh}
          className="inline-flex items-center gap-1.5 rounded border border-border-primary px-2.5 py-1.5 text-sm text-text-primary hover:bg-bg-tertiary"
        >
          <RefreshCw className={["size-3.5", loading ? "animate-spin" : ""].join(" ")} />
          Refresh
        </button>
      </div>

      {error && (
        <div className="rounded border border-status-error/40 bg-status-error/10 p-3 text-sm text-status-error">
          {error}
        </div>
      )}

      {!loading && answers.length === 0 && !error && (
        <div className="rounded border border-border-primary p-6 text-center text-sm text-text-muted">
          No helper answers yet. Enable emission in the Config tab and invite a helper — answers
          appear here as they come back.
        </div>
      )}

      <ul className="space-y-2">
        {[...answers].reverse().map((a) => (
          <li key={a.id} className="rounded border border-border-primary p-3">
            <div className="flex items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                {verdictBadge(a.verdict)}
                <span className="font-mono text-xs text-text-muted">task {a.taskId}</span>
              </div>
              <span className="text-xs text-text-muted">{a.createdAt}</span>
            </div>
            {a.reasons.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-1.5">
                {a.reasons.map((r) => (
                  <span
                    key={r}
                    className="rounded-full bg-bg-tertiary px-2 py-0.5 text-xs text-text-primary"
                  >
                    {r}
                  </span>
                ))}
              </div>
            )}
            {a.freeText && (
              <p className="mt-2 text-sm text-text-primary">&ldquo;{a.freeText}&rdquo;</p>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Invite tab
// ---------------------------------------------------------------------------

function InviteTab(): React.JSX.Element {
  return (
    <div className="max-w-2xl space-y-4 p-6">
      <h2 className="text-base font-semibold text-text-primary">Invite a helper</h2>
      <p className="text-sm text-text-muted">
        Helpers are non-technical teammates who answer small judgment questions — no code access, no
        dev tooling. They review a screenshot or a live page in a stripped-down portal and tap
        approve / reject / not-sure, optionally with a reason. Their verdicts flow back into this
        runner&apos;s reflection loop.
      </p>
      <ol className="list-decimal space-y-2 pl-5 text-sm text-text-primary">
        <li>
          Open your team settings in the web app and invite the person with the{" "}
          <span className="font-mono text-xs">HELPER</span> role.
        </li>
        <li>They sign in and see only the helper portal — open tasks for your apps.</li>
        <li>
          Enable “Emit helper tasks” in the Config tab here so this runner starts publishing
          spot-checks for them to answer.
        </li>
      </ol>
      <a
        href={INVITE_URL}
        target="_blank"
        rel="noreferrer"
        className="inline-flex items-center gap-1.5 rounded bg-brand-primary px-3 py-1.5 text-sm font-medium text-white hover:opacity-90"
      >
        <UserPlus className="size-3.5" />
        Open team invite page
      </a>
      <p className="text-xs text-text-muted">{INVITE_URL}</p>
    </div>
  );
}
