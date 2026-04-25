/**
 * WrapperDetailPage — single-wrapper detail view with four tabs.
 *
 * Header shows the wrapper name + version + transport + status, plus a
 * toggle between Start / Stop. Below, custom tab buttons switch between
 * Actions / Settings / Logs / About.
 *
 * Reached from the library page via local state (`openWrapperId`); there's
 * no react-router or URL routing in the runner UI. Back navigation is via
 * the `onBack` callback.
 */

import { useCallback, useEffect, useState } from "react";
import {
  ArrowLeft,
  Loader2,
  Play,
  Square,
  RefreshCw,
  AlertTriangle,
  Package,
  ListChecks,
  KeyRound,
  ScrollText,
  Info,
} from "lucide-react";
import { getWrapper, getWrapperStatus, startWrapper, stopWrapper } from "@/lib/wrappers/api";
import type { InstalledWrapper, WrapperStatus } from "@/lib/wrappers/types";
import { StatusBadge } from "@/components/wrappers/StatusBadge";
import { TransportBadge } from "@/components/wrappers/TransportBadge";
import { ActionsTab } from "./ActionsTab";
import { SettingsTab } from "./SettingsTab";
import { LogsTab } from "./LogsTab";
import { AboutTab } from "./AboutTab";

export type WrapperDetailTab = "actions" | "settings" | "logs" | "about";

export interface WrapperDetailPageProps {
  wrapperId: string;
  onBack: () => void;
  initialTab?: WrapperDetailTab;
}

const TABS: Array<{ id: WrapperDetailTab; label: string; icon: typeof ListChecks }> = [
  { id: "actions", label: "Actions", icon: ListChecks },
  { id: "settings", label: "Settings", icon: KeyRound },
  { id: "logs", label: "Logs", icon: ScrollText },
  { id: "about", label: "About", icon: Info },
];

export function WrapperDetailPage({
  wrapperId,
  onBack,
  initialTab = "actions",
}: WrapperDetailPageProps) {
  const [wrapper, setWrapper] = useState<InstalledWrapper | null>(null);
  const [status, setStatus] = useState<WrapperStatus>("unknown");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<WrapperDetailTab>(initialTab);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const w = await getWrapper(wrapperId);
      setWrapper(w);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [wrapperId]);

  const refreshStatus = useCallback(async () => {
    try {
      const s = await getWrapperStatus(wrapperId);
      setStatus(s.status ?? "unknown");
    } catch {
      setStatus("unknown");
    }
  }, [wrapperId]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reset loading on wrapper id change
    setLoading(true);
    refresh();
  }, [refresh]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial + interval status sync
    refreshStatus();
    const id = window.setInterval(refreshStatus, 5_000);
    return () => window.clearInterval(id);
  }, [refreshStatus]);

  const isRunning = status === "running" || status === "degraded";

  const handleToggle = async () => {
    setBusy(true);
    try {
      if (isRunning) {
        await stopWrapper(wrapperId);
      } else {
        await startWrapper(wrapperId);
      }
      await refreshStatus();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  if (loading && !wrapper) {
    return (
      <div className="h-full overflow-auto">
        <div className="mx-auto w-full max-w-[820px] flex flex-col gap-4 px-4 py-8 sm:px-6">
          <BackButton onClick={onBack} />
          <div className="flex items-center justify-center py-16 gap-2 text-muted-foreground">
            <Loader2 className="w-5 h-5 animate-spin" />
            <span className="text-sm">Loading wrapper…</span>
          </div>
        </div>
      </div>
    );
  }

  if (error && !wrapper) {
    return (
      <div className="h-full overflow-auto">
        <div className="mx-auto w-full max-w-[820px] flex flex-col gap-4 px-4 py-8 sm:px-6">
          <BackButton onClick={onBack} />
          <div className="rounded-xl border border-red-500/40 bg-red-500/5 p-5 flex items-start gap-3">
            <AlertTriangle className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
            <div className="flex-1 min-w-0">
              <p className="text-sm font-medium text-red-400">Couldn't load wrapper</p>
              <p className="mt-1 text-xs text-muted-foreground break-words">{error}</p>
              <button
                onClick={() => {
                  setError(null);
                  setLoading(true);
                  refresh();
                }}
                className="mt-3 inline-flex items-center gap-1.5 px-3 py-1 rounded-md border border-border bg-card text-xs text-foreground hover:bg-muted/30 transition-colors"
              >
                <RefreshCw className="w-3.5 h-3.5" />
                Retry
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  if (!wrapper) return null;

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto w-full max-w-[820px] flex flex-col gap-4 px-4 py-8 sm:px-6">
        <BackButton onClick={onBack} />

        {/* Header */}
        <div className="rounded-xl border border-border bg-card shadow-sm">
          <div className="flex items-start gap-3 p-5">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-cyan-500/30 bg-cyan-500/10">
              <Package className="w-4 h-4 text-cyan-400" />
            </div>
            <div className="flex-1 min-w-0">
              <div className="flex items-start justify-between gap-3 flex-wrap">
                <div className="min-w-0">
                  <h1 className="text-xl font-bold tracking-tight truncate">
                    {wrapper.manifest.displayName ?? wrapper.id}
                  </h1>
                  <p className="text-xs text-muted-foreground font-mono truncate">
                    {wrapper.packageName}
                    <span className="ml-1 text-muted-foreground/70">v{wrapper.version}</span>
                  </p>
                </div>
                <button
                  onClick={handleToggle}
                  disabled={busy}
                  className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium transition-colors disabled:opacity-50 ${
                    isRunning
                      ? "border border-border bg-card text-foreground hover:bg-muted/30"
                      : "bg-gradient-to-r from-cyan-600 to-purple-600 text-white hover:from-cyan-500 hover:to-purple-500"
                  }`}
                >
                  {busy ? (
                    <Loader2 className="w-3.5 h-3.5 animate-spin" />
                  ) : isRunning ? (
                    <Square className="w-3.5 h-3.5" />
                  ) : (
                    <Play className="w-3.5 h-3.5" />
                  )}
                  {isRunning ? "Stop" : "Start"}
                </button>
              </div>
              <div className="mt-3 flex items-center gap-2 flex-wrap">
                <TransportBadge transport={wrapper.manifest.transport ?? "api"} size="md" />
                <StatusBadge status={status} size="md" />
              </div>
              {wrapper.manifest.description && (
                <p className="mt-3 text-sm text-muted-foreground">{wrapper.manifest.description}</p>
              )}
            </div>
          </div>
        </div>

        {/* Tabs */}
        <div className="border-b border-border">
          <div className="flex items-center gap-1">
            {TABS.map((tab) => {
              const isActive = activeTab === tab.id;
              const Icon = tab.icon;
              return (
                <button
                  key={tab.id}
                  onClick={() => setActiveTab(tab.id)}
                  className={`relative inline-flex items-center gap-2 px-4 py-2 text-sm font-medium transition-colors ${
                    isActive ? "text-cyan-400" : "text-muted-foreground hover:text-foreground"
                  }`}
                >
                  <Icon className="w-3.5 h-3.5" />
                  {tab.label}
                  {isActive && (
                    <span className="absolute inset-x-3 bottom-[-1px] h-0.5 bg-cyan-400" />
                  )}
                </button>
              );
            })}
          </div>
        </div>

        {/* Content */}
        {activeTab === "actions" && (
          <ActionsTab wrapperId={wrapper.id} actions={wrapper.actions ?? []} />
        )}
        {activeTab === "settings" && (
          <SettingsTab wrapperId={wrapper.id} manifest={wrapper.manifest} />
        )}
        {activeTab === "logs" && <LogsTab wrapperId={wrapper.id} />}
        {activeTab === "about" && <AboutTab wrapper={wrapper} />}
      </div>
    </div>
  );
}

function BackButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="inline-flex items-center gap-1.5 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors w-fit"
    >
      <ArrowLeft className="w-3.5 h-3.5" />
      Back to library
    </button>
  );
}
