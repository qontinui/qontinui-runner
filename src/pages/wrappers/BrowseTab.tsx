/**
 * BrowseTab — registry-backed wrapper discovery.
 *
 * Fetches `GET /wrappers/registry` (proxy on the runner; falls back to the
 * GitHub raw URL with a 24h cache — see `api.ts::listRegistry`). Renders a
 * search box + the registry entries. Each entry has an Install button that
 * POSTs `/wrappers/install`.
 *
 * On successful install, the parent's `onInstalled(id)` callback is fired so
 * `WrappersLibraryPage` can switch to Installed and highlight the new card.
 */

import { useEffect, useMemo, useState } from "react";
import {
  Loader2,
  Search,
  Download,
  RefreshCw,
  AlertTriangle,
  ExternalLink,
  ShieldCheck,
} from "lucide-react";
import { listRegistry, installWrapper } from "@/lib/wrappers/api";
import type { RegistryWrapperEntry, InstalledWrapper } from "@/lib/wrappers/types";
import { TransportBadge } from "@/components/wrappers/TransportBadge";

export interface BrowseTabProps {
  installedIds: Set<string>;
  onInstalled: (wrapper: InstalledWrapper) => void;
}

export function BrowseTab({ installedIds, onInstalled }: BrowseTabProps) {
  const [entries, setEntries] = useState<RegistryWrapperEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [installing, setInstalling] = useState<Record<string, boolean>>({});
  const [installError, setInstallError] = useState<Record<string, string>>({});

  const load = async (force = false) => {
    setLoading(true);
    try {
      const list = await listRegistry({ force });
      setEntries(list);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter((e) => {
      const haystack = [
        e.id,
        e.package,
        e.displayName,
        e.description ?? "",
        ...(e.categories ?? []),
        e.author?.name ?? "",
      ]
        .join(" ")
        .toLowerCase();
      return haystack.includes(q);
    });
  }, [entries, query]);

  const handleInstall = async (entry: RegistryWrapperEntry) => {
    setInstalling((p) => ({ ...p, [entry.id]: true }));
    setInstallError((p) => {
      const n = { ...p };
      delete n[entry.id];
      return n;
    });
    try {
      const wrapper = await installWrapper(entry.package);
      onInstalled(wrapper);
    } catch (err) {
      setInstallError((p) => ({
        ...p,
        [entry.id]: err instanceof Error ? err.message : String(err),
      }));
    } finally {
      setInstalling((p) => {
        const n = { ...p };
        delete n[entry.id];
        return n;
      });
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search wrappers by name, category, or author"
            className="w-full pl-9 pr-3 py-2 rounded-md border border-border bg-background text-sm text-foreground placeholder:text-muted-foreground/70 focus:outline-none focus:ring-2 focus:ring-cyan-500/40 focus:border-cyan-500/60"
          />
        </div>
        <button
          onClick={() => load(true)}
          disabled={loading}
          className="inline-flex items-center gap-1.5 px-3 py-2 rounded-md border border-border bg-card text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors disabled:opacity-50"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>

      {loading ? (
        <div className="flex items-center justify-center py-16 gap-2 text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin" />
          <span className="text-sm">Loading registry…</span>
        </div>
      ) : error ? (
        <div className="rounded-xl border border-red-500/40 bg-red-500/5 p-5 flex items-start gap-3">
          <AlertTriangle className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
          <div className="flex-1 min-w-0">
            <p className="text-sm font-medium text-red-400">Couldn't reach the registry</p>
            <p className="mt-1 text-xs text-muted-foreground break-words">{error}</p>
          </div>
        </div>
      ) : filtered.length === 0 ? (
        <div className="rounded-xl border border-dashed border-border bg-card/40 p-10 text-center">
          <p className="text-sm text-muted-foreground">
            {query.trim()
              ? `No wrappers match "${query.trim()}".`
              : "The registry is empty (or hasn't been populated yet)."}
          </p>
        </div>
      ) : (
        <ul className="space-y-3">
          {filtered.map((entry) => {
            const isInstalled = installedIds.has(entry.id);
            const isInstalling = Boolean(installing[entry.id]);
            const err = installError[entry.id];
            return (
              <li
                key={entry.id}
                className="rounded-xl border border-border bg-card p-5 shadow-sm flex items-start gap-4"
              >
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 flex-wrap">
                    <h3 className="text-sm font-semibold text-foreground">{entry.displayName}</h3>
                    {entry.verified && (
                      <span className="inline-flex items-center gap-1 rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2 py-0.5 text-[10px] font-medium text-emerald-400 uppercase tracking-wider">
                        <ShieldCheck className="w-3 h-3" />
                        verified
                      </span>
                    )}
                    {entry.author?.name && (
                      <span className="text-xs text-muted-foreground">by {entry.author.name}</span>
                    )}
                  </div>
                  <p className="mt-1 text-xs text-muted-foreground font-mono">
                    {entry.package}
                    {entry.version && (
                      <span className="ml-1 text-muted-foreground/70">{entry.version}</span>
                    )}
                  </p>
                  {entry.description && (
                    <p className="mt-2 text-xs text-muted-foreground">{entry.description}</p>
                  )}
                  <div className="mt-3 flex items-center gap-2 flex-wrap">
                    {entry.categories?.map((cat) => (
                      <span
                        key={cat}
                        className="inline-flex items-center rounded-full border border-border bg-muted/30 px-2 py-0.5 text-[10px] font-medium text-muted-foreground uppercase tracking-wider"
                      >
                        {cat}
                      </span>
                    ))}
                    {entry.repo && (
                      <a
                        href={entry.repo}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="inline-flex items-center gap-1 text-[11px] text-muted-foreground hover:text-cyan-400 transition-colors"
                      >
                        <ExternalLink className="w-3 h-3" />
                        repo
                      </a>
                    )}
                  </div>
                  {err && <p className="mt-2 text-xs text-red-400">{err}</p>}
                </div>
                <div className="flex flex-col items-end gap-2 shrink-0">
                  <TransportBadge transport={(entry as { transport?: "api" }).transport ?? "api"} />
                  <button
                    onClick={() => handleInstall(entry)}
                    disabled={isInstalling || isInstalled}
                    className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium transition-colors disabled:opacity-50 ${
                      isInstalled
                        ? "border border-border bg-muted/30 text-muted-foreground cursor-not-allowed"
                        : "bg-gradient-to-r from-cyan-600 to-purple-600 text-white hover:from-cyan-500 hover:to-purple-500"
                    }`}
                  >
                    {isInstalling ? (
                      <>
                        <Loader2 className="w-3.5 h-3.5 animate-spin" />
                        Installing…
                      </>
                    ) : isInstalled ? (
                      "Installed"
                    ) : (
                      <>
                        <Download className="w-3.5 h-3.5" />
                        Install
                      </>
                    )}
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
