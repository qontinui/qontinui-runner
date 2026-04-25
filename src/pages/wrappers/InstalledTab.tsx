/**
 * InstalledTab — grid of locally-installed wrappers.
 *
 * Loads `GET /wrappers`, polls each wrapper's status every 10s while mounted,
 * and renders a grid of `WrapperCard`s. Lifecycle actions (start/stop/update/
 * uninstall) trigger the relevant runner endpoint and refresh state.
 *
 * After install completes in the Browse tab, the parent
 * `WrappersLibraryPage` switches to this tab with `highlightedWrapperId` set
 * so the new card is briefly outlined in cyan.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Loader2, Package, RefreshCw, AlertTriangle } from "lucide-react";
import {
  listWrappers,
  getWrapperStatus,
  startWrapper,
  stopWrapper,
  updateWrapper,
  uninstallWrapper,
} from "@/lib/wrappers/api";
import type { InstalledWrapper, WrapperStatus } from "@/lib/wrappers/types";
import { WrapperCard } from "@/components/wrappers/WrapperCard";

export interface InstalledTabProps {
  onOpenWrapper: (id: string) => void;
  highlightedWrapperId: string | null;
  /** Bumped by the parent after install to force a refresh. */
  refreshKey?: number;
}

export function InstalledTab({
  onOpenWrapper,
  highlightedWrapperId,
  refreshKey = 0,
}: InstalledTabProps) {
  const [wrappers, setWrappers] = useState<InstalledWrapper[]>([]);
  const [statuses, setStatuses] = useState<Record<string, WrapperStatus>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const loadWrappers = useCallback(async () => {
    try {
      const list = await listWrappers();
      if (!mountedRef.current) return;
      setWrappers(list);
      setError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mountedRef.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reset loading state on refresh trigger
    setLoading(true);
    loadWrappers();
  }, [loadWrappers, refreshKey]);

  // Poll status for every installed wrapper.
  useEffect(() => {
    if (wrappers.length === 0) return;
    let cancelled = false;
    const tick = async () => {
      const next: Record<string, WrapperStatus> = {};
      await Promise.all(
        wrappers.map(async (w) => {
          try {
            const info = await getWrapperStatus(w.id);
            next[w.id] = info.status ?? "unknown";
          } catch {
            next[w.id] = "unknown";
          }
        }),
      );
      if (!cancelled && mountedRef.current) {
        setStatuses(next);
      }
    };
    tick();
    const interval = window.setInterval(tick, 10_000);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [wrappers]);

  const setBusyFor = (id: string, value: boolean) => {
    setBusy((prev) => ({ ...prev, [id]: value }));
  };

  const handleStart = useCallback(async (id: string) => {
    setBusyFor(id, true);
    try {
      await startWrapper(id);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyFor(id, false);
      const info = await getWrapperStatus(id).catch(() => null);
      if (info) setStatuses((p) => ({ ...p, [id]: info.status ?? "unknown" }));
    }
  }, []);

  const handleStop = useCallback(async (id: string) => {
    setBusyFor(id, true);
    try {
      await stopWrapper(id);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyFor(id, false);
      const info = await getWrapperStatus(id).catch(() => null);
      if (info) setStatuses((p) => ({ ...p, [id]: info.status ?? "unknown" }));
    }
  }, []);

  const handleUpdate = useCallback(
    async (id: string) => {
      setBusyFor(id, true);
      try {
        await updateWrapper(id);
        await loadWrappers();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusyFor(id, false);
      }
    },
    [loadWrappers],
  );

  const handleUninstall = useCallback(
    async (id: string) => {
      const w = wrappers.find((x) => x.id === id);
      const label = w?.manifest?.displayName ?? id;
      if (
        !window.confirm(
          `Uninstall "${label}"? This removes the wrapper but keeps your stored credentials.`,
        )
      ) {
        return;
      }
      setBusyFor(id, true);
      try {
        await uninstallWrapper(id);
        await loadWrappers();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusyFor(id, false);
      }
    },
    [wrappers, loadWrappers],
  );

  const sorted = useMemo(
    () =>
      [...wrappers].sort((a, b) =>
        (a.manifest?.displayName ?? a.id).localeCompare(b.manifest?.displayName ?? b.id),
      ),
    [wrappers],
  );

  if (loading) {
    return (
      <div className="flex items-center justify-center py-16 gap-2 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin" />
        <span className="text-sm">Loading installed wrappers…</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="rounded-xl border border-red-500/40 bg-red-500/5 p-5 flex items-start gap-3">
        <AlertTriangle className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
        <div className="flex-1 min-w-0">
          <p className="text-sm font-medium text-red-400">Failed to load installed wrappers</p>
          <p className="mt-1 text-xs text-muted-foreground break-words">{error}</p>
          <button
            onClick={() => {
              setError(null);
              setLoading(true);
              loadWrappers();
            }}
            className="mt-3 inline-flex items-center gap-1.5 px-3 py-1 rounded-md border border-border bg-card text-xs text-foreground hover:bg-muted/30 transition-colors"
          >
            <RefreshCw className="w-3.5 h-3.5" />
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (sorted.length === 0) {
    return (
      <div className="rounded-xl border border-dashed border-border bg-card/40 p-10 flex flex-col items-center text-center gap-3">
        <div className="flex h-12 w-12 items-center justify-center rounded-xl border border-cyan-500/30 bg-cyan-500/10">
          <Package className="w-5 h-5 text-cyan-400" />
        </div>
        <div>
          <h3 className="text-sm font-semibold text-foreground">No wrappers installed yet</h3>
          <p className="mt-1 text-xs text-muted-foreground max-w-xs">
            Switch to the Browse tab to discover wrappers from the public registry, or install one
            directly via the API.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="grid gap-4 grid-cols-1 md:grid-cols-2">
      {sorted.map((wrapper) => (
        <WrapperCard
          key={wrapper.id}
          wrapper={wrapper}
          status={statuses[wrapper.id] ?? "unknown"}
          busy={Boolean(busy[wrapper.id])}
          highlighted={highlightedWrapperId === wrapper.id}
          onOpen={onOpenWrapper}
          onStart={handleStart}
          onStop={handleStop}
          onUpdate={handleUpdate}
          onUninstall={handleUninstall}
        />
      ))}
    </div>
  );
}
