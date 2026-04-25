/**
 * SettingsTab — wrapper credential management.
 *
 * Reads the wrapper's manifest envVars to know what's configurable, queries
 * `GET /wrappers/:id/credentials` for each name's `hasValue` status (the
 * runner NEVER returns the actual value), and exposes Edit / Clear actions
 * that PUT / DELETE through the credential endpoints.
 *
 * Editing opens an inline modal. Secret entries render as `type="password"`.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Loader2,
  Lock,
  Unlock,
  KeyRound,
  Trash2,
  AlertTriangle,
  CheckCircle2,
  X,
} from "lucide-react";
import { listCredentials, setCredential, clearCredential } from "@/lib/wrappers/api";
import type { CredentialEntry, WrapperManifest } from "@/lib/wrappers/types";

export interface SettingsTabProps {
  wrapperId: string;
  manifest: WrapperManifest;
}

interface ResolvedEntry extends CredentialEntry {
  /** Source-of-truth row coming from the wrapper's manifest envVars. */
  source: "manifest" | "credential";
}

export function SettingsTab({ wrapperId, manifest }: SettingsTabProps) {
  const [credentials, setCredentials] = useState<CredentialEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<ResolvedEntry | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const list = await listCredentials(wrapperId);
      setCredentials(list);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [wrapperId]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial credential load
    refresh();
  }, [refresh]);

  const rows = useMemo<ResolvedEntry[]>(() => {
    const byName = new Map<string, CredentialEntry>();
    for (const c of credentials) byName.set(c.name, c);

    const fromManifest = (manifest.envVars ?? []).map<ResolvedEntry>((env) => {
      const existing = byName.get(env.name);
      byName.delete(env.name);
      return {
        name: env.name,
        description: env.description,
        secret: env.secret,
        required: env.required,
        hasValue: existing?.hasValue ?? false,
        source: "manifest",
      };
    });

    // Any credentials returned by the runner that aren't in the manifest get
    // appended (forward-compat: future wrapper-set credentials).
    const extras = Array.from(byName.values()).map<ResolvedEntry>((c) => ({
      ...c,
      source: "credential",
    }));

    return [...fromManifest, ...extras];
  }, [credentials, manifest.envVars]);

  const handleClear = async (name: string) => {
    if (!window.confirm(`Clear credential "${name}"?`)) return;
    try {
      await clearCredential(wrapperId, name);
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleSave = async (name: string, value: string) => {
    try {
      await setCredential(wrapperId, name, value);
      setEditing(null);
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-16 gap-2 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin" />
        <span className="text-sm">Loading credentials…</span>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {error && (
        <div className="rounded-xl border border-red-500/40 bg-red-500/5 p-4 flex items-start gap-3">
          <AlertTriangle className="w-4 h-4 text-red-400 shrink-0 mt-0.5" />
          <p className="text-xs text-red-400 break-words flex-1 min-w-0">{error}</p>
        </div>
      )}

      <div className="rounded-xl border border-border bg-card overflow-hidden">
        <div className="px-5 py-3 border-b border-border flex items-center justify-between">
          <h3 className="text-xs font-semibold uppercase tracking-widest text-muted-foreground">
            Credentials
          </h3>
          <span className="text-xs text-muted-foreground">{rows.length}</span>
        </div>

        {rows.length === 0 ? (
          <div className="p-8 text-center">
            <p className="text-sm text-muted-foreground">
              This wrapper doesn't declare any environment variables.
            </p>
          </div>
        ) : (
          <ul>
            {rows.map((row, idx) => (
              <li
                key={row.name}
                className={`flex items-start gap-4 px-5 py-4 ${
                  idx === 0 ? "" : "border-t border-border"
                }`}
              >
                <div className="flex h-7 w-7 items-center justify-center rounded-md border border-border bg-background shrink-0 mt-0.5">
                  {row.secret ? (
                    <Lock className="w-3.5 h-3.5 text-amber-400" />
                  ) : (
                    <Unlock className="w-3.5 h-3.5 text-muted-foreground" />
                  )}
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 flex-wrap">
                    <code className="text-sm font-mono text-foreground">{row.name}</code>
                    {row.required && (
                      <span className="text-[10px] uppercase tracking-wider text-cyan-400 font-medium">
                        required
                      </span>
                    )}
                    {row.secret && (
                      <span className="text-[10px] uppercase tracking-wider text-amber-400 font-medium">
                        secret
                      </span>
                    )}
                    {row.hasValue ? (
                      <span className="inline-flex items-center gap-1 rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2 py-0.5 text-[10px] font-medium text-emerald-400">
                        <CheckCircle2 className="w-2.5 h-2.5" />
                        set
                      </span>
                    ) : (
                      <span className="inline-flex items-center rounded-full border border-border bg-muted/30 px-2 py-0.5 text-[10px] font-medium text-muted-foreground">
                        not set
                      </span>
                    )}
                  </div>
                  {row.description && (
                    <p className="mt-1 text-xs text-muted-foreground">{row.description}</p>
                  )}
                </div>
                <div className="flex items-center gap-1 shrink-0">
                  <button
                    onClick={() => setEditing(row)}
                    className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md border border-border bg-card text-xs text-foreground hover:bg-muted/30 transition-colors"
                  >
                    <KeyRound className="w-3 h-3" />
                    {row.hasValue ? "Replace" : "Set"}
                  </button>
                  {row.hasValue && (
                    <button
                      onClick={() => handleClear(row.name)}
                      className="inline-flex items-center px-2 py-1 rounded-md text-muted-foreground hover:text-red-400 hover:bg-red-500/10 transition-colors"
                      title="Clear credential"
                      aria-label={`Clear ${row.name}`}
                    >
                      <Trash2 className="w-3 h-3" />
                    </button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      {editing && (
        <CredentialEditor
          entry={editing}
          onCancel={() => setEditing(null)}
          onSave={(value) => handleSave(editing.name, value)}
        />
      )}
    </div>
  );
}

interface CredentialEditorProps {
  entry: ResolvedEntry;
  onCancel: () => void;
  onSave: (value: string) => Promise<void>;
}

function CredentialEditor({ entry, onCancel, onSave }: CredentialEditorProps) {
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!value) return;
    setSaving(true);
    try {
      await onSave(value);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 z-30 flex items-center justify-center bg-black/60 p-4">
      <form
        onSubmit={handleSubmit}
        className="w-full max-w-md rounded-xl border border-border bg-card shadow-xl"
      >
        <div className="flex items-center justify-between px-5 py-3 border-b border-border">
          <h3 className="text-sm font-semibold text-foreground">
            {entry.hasValue ? "Replace" : "Set"} credential
          </h3>
          <button
            type="button"
            onClick={onCancel}
            className="p-1 rounded-md text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors"
            aria-label="Close"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
        <div className="p-5 space-y-3">
          <div>
            <label className="block text-xs font-medium text-foreground mb-1.5">
              <code className="font-mono text-sm">{entry.name}</code>
              {entry.required && <span className="ml-1 text-cyan-400">*</span>}
            </label>
            {entry.description && (
              <p className="text-xs text-muted-foreground mb-2">{entry.description}</p>
            )}
            <input
              autoFocus
              type={entry.secret ? "password" : "text"}
              value={value}
              onChange={(e) => setValue(e.target.value)}
              placeholder={entry.secret ? "••••••••" : "value"}
              className="w-full rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground/70 focus:outline-none focus:ring-2 focus:ring-cyan-500/40 focus:border-cyan-500/60"
            />
            <p className="mt-2 text-[11px] text-muted-foreground">
              Stored encrypted in the runner's keyring. Never returned over HTTP.
            </p>
          </div>
        </div>
        <div className="flex items-center justify-end gap-2 px-5 py-3 border-t border-border">
          <button
            type="button"
            onClick={onCancel}
            disabled={saving}
            className="px-3 py-1.5 rounded-md border border-border bg-card text-sm text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={saving || !value}
            className="inline-flex items-center gap-2 px-4 py-1.5 rounded-md bg-gradient-to-r from-cyan-600 to-purple-600 text-white text-sm font-medium hover:from-cyan-500 hover:to-purple-500 transition-colors disabled:opacity-50"
          >
            {saving && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
            Save
          </button>
        </div>
      </form>
    </div>
  );
}
