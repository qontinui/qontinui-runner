/**
 * AppFreshnessSettings.tsx
 *
 * Per-app auto-fresh configuration for this machine's App Registry — the
 * operator surface for "when the auto-fresh engine pulls new code for app X,
 * what does it run afterwards?".
 *
 * Closes the last gap in the fleet-fresh test-target routing feature: the
 * engine (`fleet.rs::spawn_auto_fresh_engine`) has read `update_strategy` /
 * `build_command` / `start_command` off `project.apps` since it landed, but
 * nothing in any UI could set them — they were reachable only by hand-crafting
 * a `PATCH /apps/:app_id`. A designated `auto_fresh` host whose app needs a
 * rebuild would therefore pull source and stop, and every test routed to it by
 * the `fresh_only` dispatcher would run against the previously-built artifact.
 *
 * Runner-side rather than in qontinui-web on purpose: the App Registry lives in
 * the runner's own `project.apps`, which the web backend cannot see. The config
 * is edited where the registry is, over `GET /apps` + `PATCH /apps/:app_id`.
 *
 * The two strategies, and why the distinction is load-bearing:
 *
 * - **pull_only** — the source tree IS the deployment. Pulling makes it
 *   current. Right for repo-test-only apps.
 * - **pull_build** — pulling is not enough: the running instance keeps serving
 *   the old artifact until it is rebuilt and restarted. Without these commands
 *   a Playwright or Vision test passes against stale code while every
 *   git-level signal reads "up to date".
 *
 * Every gate here (Save enabled, the warnings) is derived from
 * `effectiveAfterSave` — the row as it will exist after the PATCH — never from
 * the raw form. The server applies COALESCE, so the two differ precisely where
 * a blank field is omitted.
 */

import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Loader2, PackageCheck, RefreshCw, Save } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getApiBase } from "@/lib/runner-api";
import { tracedFetch } from "@/lib/traced-fetch";
import type { LogFunction } from "./types";
import {
  buildPatchBody,
  effectiveAfterSave,
  formOf,
  isFalselyFresh,
  isKnownStrategy,
  isUpdateStrategy,
  sameForm,
  type AppFreshnessForm,
  type AppListResponse,
  type RegisteredApp,
} from "./appFreshnessHelpers";

/** Cap on a quoted error body, matching `DevLoopSettings`. */
const ERROR_BODY_CHARS = 200;

/**
 * Quote the API's `{ ok, reason, detail }` envelope so the operator sees WHICH
 * failure occurred. `app-not-found` (the row on screen is a ghost),
 * `invalid-update-strategy`, and `invalid-repo-root` (the catch-all that also
 * carries PG pool failures) are otherwise indistinguishable from the status
 * code alone — and this line is the panel's only diagnostic surface.
 */
async function describeFailure(resp: Response, what: string): Promise<string> {
  const body = await resp.text().catch(() => "");
  const excerpt = body.slice(0, ERROR_BODY_CHARS).trim();
  return excerpt
    ? `${what} returned ${resp.status}: ${excerpt}`
    : `${what} returned ${resp.status}`;
}

interface AppFreshnessSettingsProps {
  onLog: LogFunction;
}

export function AppFreshnessSettings({ onLog }: AppFreshnessSettingsProps) {
  // `null` means UNKNOWN (never loaded, or the load failed) — deliberately
  // distinct from `[]`, "the registry is empty". A failed fetch must not be
  // rendered as a verdict about the registry.
  const [apps, setApps] = useState<RegisteredApp[] | null>(null);
  const [forms, setForms] = useState<Record<string, AppFreshnessForm>>({});
  const [loading, setLoading] = useState(true);
  // A Set, not a scalar: two rows can be in flight at once, and a single slot
  // would clear the first row's spinner the moment the second started.
  const [savingAppIds, setSavingAppIds] = useState<ReadonlySet<string>>(new Set());
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const resp = await tracedFetch(`${getApiBase()}/apps`);
      if (!resp.ok) throw new Error(await describeFailure(resp, "GET /apps"));
      const body: AppListResponse = await resp.json();
      const list = body.apps ?? [];
      setApps(list);
      setForms(Object.fromEntries(list.map((a) => [a.appId, formOf(a)])));
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async fetch updates state in a callback, not synchronously
    void load();
  }, [load]);

  const patchForm = useCallback((appId: string, patch: Partial<AppFreshnessForm>) => {
    setForms((prev) => {
      const current = prev[appId];
      if (!current) return prev;
      return { ...prev, [appId]: { ...current, ...patch } };
    });
  }, []);

  const save = useCallback(
    async (app: RegisteredApp) => {
      const form = forms[app.appId];
      if (!form) return;
      setSavingAppIds((prev) => new Set(prev).add(app.appId));
      try {
        const resp = await tracedFetch(`${getApiBase()}/apps/${encodeURIComponent(app.appId)}`, {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(buildPatchBody(form)),
        });
        if (!resp.ok) {
          throw new Error(await describeFailure(resp, `PATCH /apps/${app.appId}`));
        }
        // The handler returns the updated `App`. Apply it to THIS row only —
        // re-listing would rebuild every form and discard unsaved edits the
        // operator has made to other rows.
        const updated: RegisteredApp | undefined = await resp.json();
        if (updated?.appId !== app.appId) {
          // A 200 whose body isn't the app we patched (proxy interception, a
          // future shape change) would otherwise write a junk `forms[undefined]`
          // entry and leave the row looking unsaved. Fall back to a full read
          // rather than trusting the cast — and say so, or a click that got a
          // 200 produces no feedback at all.
          onLog(
            "warning",
            `App "${app.appId}" saved, but the runner returned an unexpected body — re-reading the registry.`,
          );
          await load();
          return;
        }
        setApps((prev) =>
          prev ? prev.map((a) => (a.appId === updated.appId ? updated : a)) : prev,
        );
        setForms((prev) => {
          // Only reset the row if the operator hasn't kept typing during the
          // request — otherwise the response snaps their in-flight edit back.
          const current = prev[updated.appId];
          if (current && !sameForm(current, form)) return prev;
          return { ...prev, [updated.appId]: formOf(updated) };
        });
        onLog("success", `Updated auto-fresh config for app "${app.appId}"`);
        setError(null);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        onLog("error", `Failed to update app "${app.appId}": ${message}`);
      } finally {
        setSavingAppIds((prev) => {
          const next = new Set(prev);
          next.delete(app.appId);
          return next;
        });
      }
    },
    [forms, load, onLog],
  );

  return (
    <div data-testid="app-freshness-settings">
      <SectionHeader
        title="App Freshness"
        description="What the auto-fresh engine runs after pulling new code for each registered app, so tests routed to this machine exercise the code that just landed."
        icon={<PackageCheck />}
      />

      <div className="mb-3">
        <button
          onClick={() => void load()}
          disabled={loading}
          className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground disabled:opacity-50"
          data-testid="app-freshness-refresh"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>

      {error && (
        <p className="text-sm text-destructive mb-3" data-testid="app-freshness-error">
          {error}
        </p>
      )}

      {apps === null ? (
        // UNKNOWN, not empty. Saying "no apps are registered" here would be a
        // verdict about the registry derived from failing to reach it.
        <p className="text-sm text-muted-foreground" data-testid="app-freshness-unknown">
          {loading ? "Loading apps…" : "Could not read the app registry — see the error above."}
        </p>
      ) : apps.length === 0 ? (
        <p className="text-sm text-muted-foreground italic" data-testid="app-freshness-empty">
          No apps are registered on this machine. Register one with{" "}
          <code className="font-mono">POST /apps</code> before configuring how it stays fresh.
        </p>
      ) : (
        <div className="space-y-4">
          {apps.map((app) => {
            const form = forms[app.appId];
            if (!form) return null;
            const stored = formOf(app);
            const effective = effectiveAfterSave(app, form);
            const falselyFresh = isFalselyFresh(effective);
            const dirty = !sameForm(effective, stored);
            // Commands that survive the save but are NOT visible in their input
            // (the operator cleared it). Naming them is the only view they have
            // of a stored command they just deleted from the box.
            const retained = (
              [
                ["build", form.buildCommand.trim() ? "" : effective.buildCommand],
                ["start", form.startCommand.trim() ? "" : effective.startCommand],
              ] as Array<[string, string]>
            ).filter(([, value]) => value.length > 0);
            const saving = savingAppIds.has(app.appId);
            const isBuild = form.updateStrategy === "pull_build";
            return (
              <div
                key={app.appId}
                className="rounded-lg border border-border p-3 space-y-3"
                data-testid="app-freshness-row"
              >
                <div className="flex items-center justify-between gap-2">
                  <div className="min-w-0">
                    <div className="text-sm font-medium truncate">
                      {app.displayName || app.appId}
                    </div>
                    <div className="text-xs text-muted-foreground font-mono truncate">
                      {app.repoRoot}
                    </div>
                  </div>
                  <button
                    onClick={() => void save(app)}
                    disabled={!dirty || falselyFresh || saving}
                    className="inline-flex items-center gap-1 rounded-md bg-primary px-2 py-1 text-xs text-primary-foreground disabled:opacity-50"
                    data-testid="app-freshness-save"
                  >
                    {saving ? (
                      <Loader2 className="w-3 h-3 animate-spin" />
                    ) : (
                      <Save className="w-3 h-3" />
                    )}
                    Save
                  </button>
                </div>

                {!isKnownStrategy(app) && (
                  <p
                    className="flex items-start gap-1 text-xs text-amber-500"
                    data-testid="app-freshness-unknown-strategy"
                  >
                    <AlertTriangle className="w-3 h-3 mt-0.5 shrink-0" />
                    <span>
                      Stored strategy <code className="font-mono">{app.updateStrategy}</code> is not
                      recognised by this build, so the selector below shows{" "}
                      <code className="font-mono">pull_only</code> instead. Choosing a strategy here
                      and saving <strong>will overwrite</strong> the stored value, and this build
                      cannot restore it — update the runner, or PATCH the registry directly.
                    </span>
                  </p>
                )}

                <label className="block text-xs space-y-1">
                  <span className="text-muted-foreground">Update strategy</span>
                  <select
                    value={form.updateStrategy}
                    onChange={(e) => {
                      if (isUpdateStrategy(e.target.value)) {
                        patchForm(app.appId, { updateStrategy: e.target.value });
                      }
                    }}
                    className="w-full rounded-md border border-border bg-background px-2 py-1"
                    data-testid="app-freshness-strategy"
                  >
                    <option value="pull_only">pull_only — source is the deployment</option>
                    <option value="pull_build">
                      pull_build — rebuild and restart after pulling
                    </option>
                  </select>
                </label>

                {isBuild && (
                  <div className="space-y-2">
                    <label className="block text-xs space-y-1">
                      <span className="text-muted-foreground">Build command</span>
                      <input
                        value={form.buildCommand}
                        onChange={(e) => patchForm(app.appId, { buildCommand: e.target.value })}
                        placeholder="npm run build"
                        className="w-full rounded-md border border-border bg-background px-2 py-1 font-mono"
                        data-testid="app-freshness-build-command"
                      />
                    </label>
                    <label className="block text-xs space-y-1">
                      <span className="text-muted-foreground">
                        Start command — restarts the deployed instance
                      </span>
                      <input
                        value={form.startCommand}
                        onChange={(e) => patchForm(app.appId, { startCommand: e.target.value })}
                        placeholder="npm run start"
                        className="w-full rounded-md border border-border bg-background px-2 py-1 font-mono"
                        data-testid="app-freshness-start-command"
                      />
                    </label>
                    {falselyFresh && (
                      <p
                        className="flex items-start gap-1 text-xs text-destructive"
                        data-testid="app-freshness-no-commands"
                      >
                        <AlertTriangle className="w-3 h-3 mt-0.5 shrink-0" />
                        <span>
                          pull_build with no command would mark this app <em>fresh</em> after
                          pulling without building anything, and the dispatcher would route tests to
                          it. Set at least one command to save.
                        </span>
                      </p>
                    )}
                    <p className="text-xs text-muted-foreground">
                      Commands run in the app&apos;s repo root via the platform shell. Clearing a
                      field does not clear the stored command — the registry API has no way to unset
                      one — so a blank field is left as-is.
                    </p>
                    {retained.length > 0 && (
                      <p
                        className="text-xs text-muted-foreground"
                        data-testid="app-freshness-retained"
                      >
                        Saving keeps{" "}
                        {retained.map(([label, value], i) => (
                          <span key={label}>
                            {i > 0 && ", "}
                            {label} <code className="font-mono">{value}</code>
                          </span>
                        ))}
                        .
                      </p>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
