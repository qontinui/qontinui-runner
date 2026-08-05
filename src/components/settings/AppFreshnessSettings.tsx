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
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2, PackageCheck, RefreshCw, Save } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getApiBase } from "@/lib/runner-api";
import type { LogFunction } from "./types";
import {
  buildPatchBody,
  formOf,
  isUpdateStrategy,
  sameForm,
  type AppFreshnessForm,
  type AppListResponse,
  type RegisteredApp,
} from "./appFreshnessHelpers";

interface AppFreshnessSettingsProps {
  onLog: LogFunction;
}

export function AppFreshnessSettings({ onLog }: AppFreshnessSettingsProps) {
  const [apps, setApps] = useState<RegisteredApp[] | null>(null);
  const [forms, setForms] = useState<Record<string, AppFreshnessForm>>({});
  const [loading, setLoading] = useState(true);
  const [savingAppId, setSavingAppId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const resp = await fetch(`${getApiBase()}/apps`);
      if (!resp.ok) throw new Error(`GET /apps returned ${resp.status}`);
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
      setSavingAppId(app.appId);
      try {
        // See `buildPatchBody` for why a blank command is omitted rather than
        // sent as an empty string.
        const body = buildPatchBody(form);

        const resp = await fetch(`${getApiBase()}/apps/${encodeURIComponent(app.appId)}`, {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });
        if (!resp.ok) {
          throw new Error(`PATCH /apps/${app.appId} returned ${resp.status}`);
        }
        onLog("success", `Updated auto-fresh config for app "${app.appId}"`);
        setError(null);
        await load();
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        onLog("error", `Failed to update app "${app.appId}": ${message}`);
      } finally {
        setSavingAppId(null);
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
          className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
          data-testid="app-freshness-refresh"
        >
          <RefreshCw className="w-3 h-3" />
          Refresh
        </button>
      </div>

      {error && (
        <p className="text-sm text-destructive mb-3" data-testid="app-freshness-error">
          {error}
        </p>
      )}

      {loading && apps === null ? (
        <p className="text-sm text-muted-foreground">Loading apps…</p>
      ) : (apps ?? []).length === 0 ? (
        <p className="text-sm text-muted-foreground italic" data-testid="app-freshness-empty">
          No apps are registered on this machine. Register one with{" "}
          <code className="font-mono">POST /apps</code> before configuring how it stays fresh.
        </p>
      ) : (
        <div className="space-y-4">
          {(apps ?? []).map((app) => {
            const form = forms[app.appId];
            if (!form) return null;
            const dirty = !sameForm(form, formOf(app));
            const saving = savingAppId === app.appId;
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
                    disabled={!dirty || saving}
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
                    {!form.buildCommand.trim() && !form.startCommand.trim() && (
                      <p className="text-xs text-amber-500" data-testid="app-freshness-no-commands">
                        pull_build with neither command set behaves exactly like pull_only — the
                        engine pulls and runs nothing, so the deployed artifact stays stale.
                      </p>
                    )}
                    <p className="text-xs text-muted-foreground">
                      Commands run in the app&apos;s repo root via the platform shell. Saving a
                      blank field leaves the stored command unchanged — the registry API has no way
                      to clear one back to empty.
                    </p>
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
