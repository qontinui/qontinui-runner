/**
 * WrappersLibraryPage — top-level page behind the "Wrappers" sidebar entry.
 *
 * Renders one of two views:
 *   - The library (default): header + Installed/Browse tabs.
 *   - The detail view (when `openWrapperId` is set): `<WrapperDetailPage>`.
 *
 * The runner UI uses tab-based navigation with no URL routing, so detail
 * navigation is local state — clicking a card's "Open" button sets
 * `openWrapperId`, the back button on the detail page clears it.
 */

import { useCallback, useEffect, useState } from "react";
import { Package } from "lucide-react";
import { listWrappers } from "@/lib/wrappers/api";
import type { InstalledWrapper } from "@/lib/wrappers/types";
import { InstalledTab } from "./InstalledTab";
import { BrowseTab } from "./BrowseTab";
import { WrapperDetailPage } from "./WrapperDetailPage";

type LibraryTab = "installed" | "browse";

export function WrappersLibraryPage() {
  const [activeTab, setActiveTab] = useState<LibraryTab>("installed");
  const [openWrapperId, setOpenWrapperId] = useState<string | null>(null);
  const [installedIds, setInstalledIds] = useState<Set<string>>(new Set());
  const [highlightedId, setHighlightedId] = useState<string | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);

  const refreshInstalledIds = useCallback(async () => {
    try {
      const list = await listWrappers();
      setInstalledIds(new Set(list.map((w) => w.id)));
    } catch {
      /* leave the set as-is; BrowseTab can still render */
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial + post-install id sync
    refreshInstalledIds();
  }, [refreshInstalledIds, refreshKey]);

  // Clear the highlight after a short delay so the cyan ring fades.
  useEffect(() => {
    if (!highlightedId) return;
    const id = window.setTimeout(() => setHighlightedId(null), 4_000);
    return () => window.clearTimeout(id);
  }, [highlightedId]);

  const handleInstalled = useCallback((wrapper: InstalledWrapper) => {
    setHighlightedId(wrapper.id);
    setActiveTab("installed");
    setRefreshKey((k) => k + 1);
  }, []);

  if (openWrapperId) {
    return <WrapperDetailPage wrapperId={openWrapperId} onBack={() => setOpenWrapperId(null)} />;
  }

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto w-full max-w-[1024px] flex flex-col gap-4 px-4 py-8 sm:px-6">
        {/* Header */}
        <div className="mb-2 flex items-center gap-3">
          <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-cyan-500/30 bg-cyan-500/10">
            <Package className="w-4 h-4 text-cyan-400" />
          </div>
          <div className="min-w-0">
            <h1 className="text-2xl font-bold tracking-tight">Wrappers</h1>
            <p className="text-xs text-muted-foreground">
              Install wrapper extensions to give the runner new typed actions — think MCP servers,
              but first-class.
            </p>
          </div>
        </div>

        {/* Tabs */}
        <div className="border-b border-border">
          <div className="flex items-center gap-1">
            <TabButton
              active={activeTab === "installed"}
              onClick={() => setActiveTab("installed")}
              count={installedIds.size}
            >
              Installed
            </TabButton>
            <TabButton active={activeTab === "browse"} onClick={() => setActiveTab("browse")}>
              Browse
            </TabButton>
          </div>
        </div>

        {/* Tab content */}
        {activeTab === "installed" ? (
          <InstalledTab
            onOpenWrapper={(id) => setOpenWrapperId(id)}
            highlightedWrapperId={highlightedId}
            refreshKey={refreshKey}
          />
        ) : (
          <BrowseTab installedIds={installedIds} onInstalled={handleInstalled} />
        )}
      </div>
    </div>
  );
}

interface TabButtonProps {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
  count?: number;
}

function TabButton({ active, onClick, children, count }: TabButtonProps) {
  return (
    <button
      onClick={onClick}
      className={`relative inline-flex items-center gap-2 px-4 py-2 text-sm font-medium transition-colors ${
        active ? "text-cyan-400" : "text-muted-foreground hover:text-foreground"
      }`}
    >
      {children}
      {typeof count === "number" && count > 0 && (
        <span
          className={`inline-flex items-center justify-center min-w-[18px] h-[18px] px-1 rounded-full text-[10px] font-semibold ${
            active ? "bg-cyan-500/20 text-cyan-300" : "bg-muted/40 text-muted-foreground"
          }`}
        >
          {count}
        </span>
      )}
      {active && <span className="absolute inset-x-3 bottom-[-1px] h-0.5 bg-cyan-400" />}
    </button>
  );
}
