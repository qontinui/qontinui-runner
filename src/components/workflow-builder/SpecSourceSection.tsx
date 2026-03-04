/**
 * SpecSourceSection.tsx
 *
 * Collapsible section for discovering and selecting semantic page specs
 * to include in AI workflow generation. Supports multi-page discovery:
 * connects to SDK apps, discovers all pages via crawl, shows per-page
 * spec groups with hierarchical checkboxes.
 */

import React, { useState, useCallback, useEffect, useMemo, useRef } from "react";
import {
  ChevronDown,
  ChevronRight,
  ShieldCheck,
  Loader2,
  Wifi,
  WifiOff,
  Trash2,
  Eye,
  EyeOff,
  Globe,
} from "lucide-react";
import { buildSpecPrompt, type DiscoveredSpec, type SpecGroup } from "@/lib/spec-prompt-builder";
import { parseDiscoveredSpecs, unwrapSpecResponse } from "@/lib/ui-bridge/spec-parser";
import { getAllSpecs } from "@/lib/spec-registry";
import { listen } from "@tauri-apps/api/event";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// =============================================================================
// Types
// =============================================================================

export interface DiscoveredPage {
  url: string;
  title: string;
}

export interface SpecSourceState {
  discoveredSpecs: DiscoveredSpec[];
  selectedGroupIds: Set<string>;
  discoveredPages: DiscoveredPage[];
  selectedPageUrls: Set<string>;
}

export interface SpecSourceSectionProps {
  onSpecsChanged: (state: SpecSourceState) => void;
}

// localStorage key
const STORAGE_KEY = "ai-generate-spec-source";
// Bump this when bundled specs change to auto-select new groups
const BUNDLED_SPEC_VERSION = 3;

interface PersistedSpecState {
  sdkUrl: string;
  discoveredSpecs: DiscoveredSpec[];
  selectedGroupIds: string[];
  bundledSpecVersion?: number;
  discoveredPages?: DiscoveredPage[];
  selectedPageUrls?: string[];
}

// =============================================================================
// Helpers
// =============================================================================

/**
 * Pass through all specs that have at least one group.
 * No longer filters by category — the UI selection handles filtering.
 */
function filterSelectedGroups(specs: DiscoveredSpec[]): DiscoveredSpec[] {
  return specs.filter((spec) => (spec.config?.groups ?? []).length > 0);
}

/** Get the page URL for a spec */
function getSpecPageUrl(spec: DiscoveredSpec): string {
  return spec.config?.metadata?.pageUrl || spec.specId;
}

// =============================================================================
// Component
// =============================================================================

export function SpecSourceSection({ onSpecsChanged }: SpecSourceSectionProps) {
  const onSpecsChangedRef = useRef(onSpecsChanged);
  onSpecsChangedRef.current = onSpecsChanged;

  const [isOpen, setIsOpen] = useState(false);

  // SDK connection state
  const [sdkUrl, setSdkUrl] = useState("");
  const [isConnected, setIsConnected] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);
  const [connectedAppName, setConnectedAppName] = useState("");
  const [connectionError, setConnectionError] = useState<string | null>(null);

  // Spec discovery state
  const [discoveredSpecs, setDiscoveredSpecs] = useState<DiscoveredSpec[]>([]);
  const [selectedGroupIds, setSelectedGroupIds] = useState<Set<string>>(new Set());
  const [isDiscovering, setIsDiscovering] = useState(false);

  // Multi-page discovery state
  const [discoveredPages, setDiscoveredPages] = useState<DiscoveredPage[]>([]);
  const [selectedPageUrls, setSelectedPageUrls] = useState<Set<string>>(new Set());
  const [isCrawling, setIsCrawling] = useState(false);
  const [crawlProgress, setCrawlProgress] = useState<string | null>(null);

  // Collapsed page URLs for hierarchical display
  const [collapsedPages, setCollapsedPages] = useState<Set<string>>(new Set());

  // Manual URL connection
  const [manualUrl, setManualUrl] = useState("");
  const [showManualConnect, setShowManualConnect] = useState(false);

  // Build full state for parent notifications
  const buildState = useCallback(
    (
      specs: DiscoveredSpec[],
      groupIds: Set<string>,
      pages: DiscoveredPage[],
      pageUrls: Set<string>,
    ): SpecSourceState => ({
      discoveredSpecs: specs,
      selectedGroupIds: groupIds,
      discoveredPages: pages,
      selectedPageUrls: pageUrls,
    }),
    [],
  );

  // Check existing SDK connection on mount
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    (async () => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/status`, {
          signal: controller.signal,
        });
        const json = await resp.json();
        if (!cancelled && json.success && json.data?.connected) {
          setIsConnected(true);
          setConnectedAppName(json.data.app?.appName || json.data.url || "SDK App");
          setSdkUrl(json.data.url || "");
        }
      } catch {
        // Runner may not be available or request was aborted
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  // Load bundled specs on mount, overlay persisted selection state and cached specs
  useEffect(() => {
    async function loadSpecs() {
      // Start with all bundled specs
      const bundled = getAllSpecs();
      const specMap = new Map(bundled.map((s) => [s.specId, s]));

      // Merge any additional specs from localStorage (e.g. from external apps)
      let persistedSelectedIds: string[] | null = null;
      let persistedPages: DiscoveredPage[] = [];
      let persistedPageUrls: string[] = [];
      try {
        const stored = localStorage.getItem(STORAGE_KEY);
        if (stored) {
          const parsed: PersistedSpecState = JSON.parse(stored);
          persistedSelectedIds = parsed.selectedGroupIds ?? null;
          persistedPages = parsed.discoveredPages ?? [];
          persistedPageUrls = parsed.selectedPageUrls ?? [];
          if (parsed.discoveredSpecs?.length > 0) {
            const filtered = filterSelectedGroups(parsed.discoveredSpecs);
            for (const spec of filtered) {
              if (!specMap.has(spec.specId)) {
                specMap.set(spec.specId, spec);
              }
            }
          }
          if (parsed.sdkUrl) {
            setSdkUrl(parsed.sdkUrl);
          }
        }
      } catch {
        // Ignore parse errors
      }

      // Merge cached specs from the runner database
      try {
        const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/cached-specs`);
        const json = await resp.json();
        if (json.success && Array.isArray(json.data)) {
          for (const cached of json.data) {
            const specId = `ext:${cached.app_name}:${cached.spec_id}`;
            if (!specMap.has(specId)) {
              try {
                const config = JSON.parse(cached.spec_json);
                specMap.set(specId, { specId, config, appName: cached.app_name || undefined });
              } catch {
                // Skip malformed spec JSON
              }
            }
          }
        }
      } catch {
        // Runner not available — ignore
      }

      const allSpecs = Array.from(specMap.values());

      // Build selected IDs
      const selectedIds = new Set<string>();

      // Check if bundled specs version changed
      let storedVersion = 0;
      try {
        const stored = localStorage.getItem(STORAGE_KEY);
        if (stored) {
          storedVersion = JSON.parse(stored).bundledSpecVersion ?? 0;
        }
      } catch {
        // ignore
      }

      const versionChanged = storedVersion !== BUNDLED_SPEC_VERSION;

      if (versionChanged || persistedSelectedIds === null) {
        for (const spec of allSpecs) {
          for (const group of spec.config?.groups ?? []) {
            selectedIds.add(group.id);
          }
        }
      } else {
        const persistedSet = new Set(persistedSelectedIds);
        for (const spec of allSpecs) {
          for (const group of spec.config?.groups ?? []) {
            if (persistedSet.has(group.id)) {
              selectedIds.add(group.id);
            }
          }
        }
      }

      const pages = persistedPages;
      const pageUrls = new Set(persistedPageUrls);

      setDiscoveredSpecs(allSpecs);
      setSelectedGroupIds(selectedIds);
      setDiscoveredPages(pages);
      setSelectedPageUrls(pageUrls);
      if (allSpecs.length > 0) {
        setIsOpen(true);
      }
      onSpecsChangedRef.current(buildState(allSpecs, selectedIds, pages, pageUrls));
    }

    loadSpecs();
  }, [buildState]);

  // Persist state changes
  useEffect(() => {
    const toSave: PersistedSpecState = {
      sdkUrl,
      discoveredSpecs,
      selectedGroupIds: Array.from(selectedGroupIds),
      bundledSpecVersion: BUNDLED_SPEC_VERSION,
      discoveredPages,
      selectedPageUrls: Array.from(selectedPageUrls),
    };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(toSave));
  }, [sdkUrl, discoveredSpecs, selectedGroupIds, discoveredPages, selectedPageUrls]);

  // Listen for auto-discovered specs from the runner (Tauri event)
  useEffect(() => {
    let unlisten: (() => void) | undefined;

    listen<{ app_url: string; app_name: string; spec_count: number }>(
      "specs-discovered",
      async () => {
        // Refresh cached specs from runner
        try {
          const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/cached-specs`);
          const json = await resp.json();
          if (json.success && Array.isArray(json.data)) {
            setDiscoveredSpecs((prev) => {
              const specMap = new Map(prev.map((s) => [s.specId, s]));
              for (const cached of json.data) {
                const specId = `ext:${cached.app_name}:${cached.spec_id}`;
                if (!specMap.has(specId)) {
                  try {
                    const config = JSON.parse(cached.spec_json);
                    specMap.set(specId, { specId, config, appName: cached.app_name || undefined });
                  } catch {
                    // skip
                  }
                }
              }
              return Array.from(specMap.values());
            });
          }
        } catch {
          // Runner not available
        }
      },
    ).then((fn) => {
      unlisten = fn;
    });

    return () => {
      unlisten?.();
    };
  }, []);

  // Notify parent when selection changes
  const notifyParent = useCallback(
    (
      specs: DiscoveredSpec[],
      groupIds: Set<string>,
      pages?: DiscoveredPage[],
      pageUrls?: Set<string>,
    ) => {
      onSpecsChanged(
        buildState(specs, groupIds, pages ?? discoveredPages, pageUrls ?? selectedPageUrls),
      );
    },
    [onSpecsChanged, buildState, discoveredPages, selectedPageUrls],
  );

  // Merge new specs helper
  const mergeSpecs = useCallback(
    (newSpecs: DiscoveredSpec[]) => {
      const existingMap = new Map(discoveredSpecs.map((s) => [s.specId, s]));
      for (const spec of newSpecs) {
        existingMap.set(spec.specId, spec);
      }
      const merged = Array.from(existingMap.values());

      const newIds = new Set(selectedGroupIds);
      for (const spec of newSpecs) {
        for (const group of spec.config?.groups ?? []) {
          newIds.add(group.id);
        }
      }

      setDiscoveredSpecs(merged);
      setSelectedGroupIds(newIds);
      notifyParent(merged, newIds);
    },
    [discoveredSpecs, selectedGroupIds, notifyParent],
  );

  // Connect to SDK app
  const handleConnect = useCallback(async (url: string) => {
    if (!url.trim()) return;
    setIsConnecting(true);
    setConnectionError(null);
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/connect`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url: url.trim() }),
      });
      const json = await resp.json();
      if (json.success) {
        setIsConnected(true);
        setConnectedAppName(json.data?.app?.appName || url.trim());
        setSdkUrl(url.trim());
      } else {
        setConnectionError(json.error || "Connection failed");
      }
    } catch (err) {
      setConnectionError(err instanceof Error ? err.message : "Connection failed");
    } finally {
      setIsConnecting(false);
    }
  }, []);

  // Disconnect from SDK app
  const handleDisconnect = useCallback(async () => {
    try {
      await tracedFetch(`${getApiBase()}/ui-bridge/sdk/disconnect`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });
    } catch {
      // Best-effort disconnect
    }
    setIsConnected(false);
    setConnectedAppName("");
  }, []);

  // Discover semantic specs from the current page
  const handleDiscoverSpecs = useCallback(async () => {
    if (!isConnected) return;
    setIsDiscovering(true);
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/discover`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "getSpecs" }),
      });
      const json = await resp.json();
      const rawSpecs = unwrapSpecResponse(json.data ?? json);
      const allSpecs = parseDiscoveredSpecs(rawSpecs);
      const semanticSpecs = filterSelectedGroups(allSpecs);
      if (semanticSpecs.length === 0) return;
      const appLabel = connectedAppName || undefined;
      const tagged = semanticSpecs.map((s) => ({ ...s, appName: appLabel }));
      mergeSpecs(tagged);
      setIsOpen(true);
    } catch {
      // Discovery failed
    } finally {
      setIsDiscovering(false);
    }
  }, [isConnected, connectedAppName, mergeSpecs]);

  // Discover all pages via crawl, then get specs for each
  const handleDiscoverAllPages = useCallback(async () => {
    if (!isConnected) return;
    setIsCrawling(true);
    setCrawlProgress("Discovering links...");
    try {
      // Step 1: Discover navigable links from the current page
      const linkResp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/discover`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ interactive_only: false }),
      });
      const linkJson = await linkResp.json();
      const rawElements = linkJson.data?.elements ?? linkJson.elements ?? [];

      // Extract link URLs from discovered elements
      const links: string[] = [];
      const seen = new Set<string>();
      for (const el of rawElements) {
        const href = el.attributes?.href || el.href;
        if (typeof href === "string" && href.startsWith("/") && !seen.has(href)) {
          seen.add(href);
          // Build full URL from SDK URL
          const origin = sdkUrl ? new URL(sdkUrl).origin : "";
          links.push(origin ? `${origin}${href}` : href);
        }
      }

      if (links.length === 0) {
        setCrawlProgress("No links found on current page.");
        setTimeout(() => setCrawlProgress(null), 2000);
        return;
      }

      // Step 2: Crawl each page for specs
      const pages: DiscoveredPage[] = [];
      const newSpecs: DiscoveredSpec[] = [];
      const newPageUrls = new Set<string>();

      for (let i = 0; i < links.length; i++) {
        const url = links[i];
        const pathname = new URL(url).pathname;
        setCrawlProgress(`Crawling ${pathname} (${i + 1}/${links.length})...`);

        try {
          // Navigate to page
          await tracedFetch(`${getApiBase()}/ui-bridge/sdk/navigate`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ url }),
          });

          // Wait for page to settle
          await new Promise((resolve) => setTimeout(resolve, 2500));

          // Discover specs on this page
          const specResp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/discover`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ action: "getSpecs" }),
          });
          const specJson = await specResp.json();
          const rawSpecs = unwrapSpecResponse(specJson.data ?? specJson);
          const specs = parseDiscoveredSpecs(rawSpecs);
          const semanticSpecs = filterSelectedGroups(specs);

          const title = pathname;
          pages.push({ url: pathname, title });
          newPageUrls.add(pathname);

          if (semanticSpecs.length > 0) {
            const appLabel = connectedAppName || undefined;
            newSpecs.push(...semanticSpecs.map((s) => ({ ...s, appName: appLabel })));
          }
        } catch {
          // Skip pages that fail
          pages.push({ url: pathname, title: pathname });
          newPageUrls.add(pathname);
        }
      }

      // Merge all discovered specs
      setDiscoveredPages(pages);
      setSelectedPageUrls(newPageUrls);

      if (newSpecs.length > 0) {
        const existingMap = new Map(discoveredSpecs.map((s) => [s.specId, s]));
        for (const spec of newSpecs) {
          existingMap.set(spec.specId, spec);
        }
        const merged = Array.from(existingMap.values());

        // Auto-select all new groups
        const newIds = new Set(selectedGroupIds);
        for (const spec of newSpecs) {
          for (const group of spec.config?.groups ?? []) {
            newIds.add(group.id);
          }
        }

        setDiscoveredSpecs(merged);
        setSelectedGroupIds(newIds);
        notifyParent(merged, newIds, pages, newPageUrls);
      } else {
        notifyParent(discoveredSpecs, selectedGroupIds, pages, newPageUrls);
      }

      setIsOpen(true);
      setCrawlProgress(`Done! Found specs on ${newSpecs.length > 0 ? pages.length : 0} pages.`);
      setTimeout(() => setCrawlProgress(null), 3000);
    } catch {
      setCrawlProgress("Crawl failed.");
      setTimeout(() => setCrawlProgress(null), 2000);
    } finally {
      setIsCrawling(false);
    }
  }, [isConnected, sdkUrl, connectedAppName, discoveredSpecs, selectedGroupIds, notifyParent]);

  // Manual connect
  const handleManualConnect = useCallback(async () => {
    const url = manualUrl.trim();
    if (!url) return;
    await handleConnect(url);
    setManualUrl("");
    setShowManualConnect(false);
  }, [manualUrl, handleConnect]);

  // Clear all accumulated specs
  const handleClearAll = useCallback(() => {
    setDiscoveredSpecs([]);
    setSelectedGroupIds(new Set());
    setDiscoveredPages([]);
    setSelectedPageUrls(new Set());
    notifyParent([], new Set(), [], new Set());
  }, [notifyParent]);

  // Toggle a spec group
  const toggleGroup = useCallback(
    (groupId: string) => {
      const next = new Set(selectedGroupIds);
      if (next.has(groupId)) {
        next.delete(groupId);
      } else {
        next.add(groupId);
      }
      setSelectedGroupIds(next);
      notifyParent(discoveredSpecs, next);
    },
    [selectedGroupIds, discoveredSpecs, notifyParent],
  );

  // Toggle all groups for a page
  const togglePage = useCallback(
    (pageUrl: string) => {
      const pageGroupIds: string[] = [];
      for (const spec of discoveredSpecs) {
        if (getSpecPageUrl(spec) === pageUrl) {
          for (const group of spec.config?.groups ?? []) {
            pageGroupIds.push(group.id);
          }
        }
      }

      const next = new Set(selectedGroupIds);
      const allSelected = pageGroupIds.every((id) => next.has(id));

      if (allSelected) {
        for (const id of pageGroupIds) next.delete(id);
      } else {
        for (const id of pageGroupIds) next.add(id);
      }

      // Update selected page URLs based on group selection
      const nextPageUrls = new Set(selectedPageUrls);
      if (allSelected) {
        nextPageUrls.delete(pageUrl);
      } else {
        nextPageUrls.add(pageUrl);
      }

      setSelectedGroupIds(next);
      setSelectedPageUrls(nextPageUrls);
      notifyParent(discoveredSpecs, next, discoveredPages, nextPageUrls);
    },
    [discoveredSpecs, selectedGroupIds, selectedPageUrls, discoveredPages, notifyParent],
  );

  // Toggle page collapse
  const togglePageCollapse = useCallback((pageUrl: string) => {
    setCollapsedPages((prev) => {
      const next = new Set(prev);
      if (next.has(pageUrl)) {
        next.delete(pageUrl);
      } else {
        next.add(pageUrl);
      }
      return next;
    });
  }, []);

  // Select all / none
  const handleSelectAll = useCallback(() => {
    const allIds = new Set<string>();
    const allPageUrls = new Set<string>();
    for (const spec of discoveredSpecs) {
      allPageUrls.add(getSpecPageUrl(spec));
      for (const group of spec.config?.groups ?? []) {
        allIds.add(group.id);
      }
    }
    setSelectedGroupIds(allIds);
    setSelectedPageUrls(allPageUrls);
    notifyParent(discoveredSpecs, allIds, discoveredPages, allPageUrls);
  }, [discoveredSpecs, discoveredPages, notifyParent]);

  const handleSelectNone = useCallback(() => {
    const empty = new Set<string>();
    setSelectedGroupIds(empty);
    setSelectedPageUrls(new Set());
    notifyParent(discoveredSpecs, empty, discoveredPages, new Set());
  }, [discoveredSpecs, discoveredPages, notifyParent]);

  // Compute summary badge
  const totalGroups = discoveredSpecs.reduce((sum, s) => sum + (s.config?.groups?.length ?? 0), 0);
  const selectedCount = selectedGroupIds.size;

  // Group specs by page URL for hierarchical display
  const specsByPage = useMemo(() => {
    const map = new Map<string, { spec: DiscoveredSpec; groups: SpecGroup[]; appName?: string }>();
    for (const spec of discoveredSpecs) {
      const pageUrl = getSpecPageUrl(spec);
      if (!map.has(pageUrl)) {
        map.set(pageUrl, { spec, groups: [], appName: spec.appName });
      }
      const entry = map.get(pageUrl)!;
      // Use the first non-empty appName found for this page
      if (!entry.appName && spec.appName) {
        entry.appName = spec.appName;
      }
      for (const group of spec.config?.groups ?? []) {
        entry.groups.push(group);
      }
    }
    return map;
  }, [discoveredSpecs]);

  // Compute page checkbox states (checked, indeterminate)
  const getPageCheckState = useCallback(
    (pageUrl: string): "checked" | "unchecked" | "indeterminate" => {
      const entry = specsByPage.get(pageUrl);
      if (!entry || entry.groups.length === 0) return "unchecked";

      const selectedInPage = entry.groups.filter((g) => selectedGroupIds.has(g.id)).length;
      if (selectedInPage === 0) return "unchecked";
      if (selectedInPage === entry.groups.length) return "checked";
      return "indeterminate";
    },
    [specsByPage, selectedGroupIds],
  );

  // Prompt preview
  const [showPromptPreview, setShowPromptPreview] = useState(false);
  const promptPreview = useMemo(() => {
    if (selectedCount === 0) return null;
    return buildSpecPrompt({ discoveredSpecs, selectedGroupIds });
  }, [discoveredSpecs, selectedGroupIds, selectedCount]);

  return (
    <div className="space-y-1">
      <button
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        className="flex items-center gap-2 text-sm text-zinc-400 hover:text-zinc-200 transition-colors"
      >
        {isOpen ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
        <ShieldCheck className="w-4 h-4" />
        Page Specs
        {selectedCount > 0 && (
          <span className="text-xs ml-1 px-1.5 py-0.5 rounded bg-zinc-700 text-zinc-300">
            {selectedCount} group{selectedCount !== 1 ? "s" : ""}
          </span>
        )}
        {isConnected && (
          <span className="flex items-center gap-1 text-xs text-green-400 ml-auto">
            <Wifi className="w-3 h-3" />
            {connectedAppName}
          </span>
        )}
      </button>

      {isOpen && (
        <div className="mt-3 space-y-3">
          {/* Connection Bar */}
          <div className="rounded-lg border border-zinc-700 bg-zinc-800/50 p-3">
            {isConnected ? (
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    <span className="w-2 h-2 rounded-full bg-green-400" />
                    <span className="text-sm text-zinc-300">{connectedAppName}</span>
                  </div>
                  <button
                    onClick={handleDisconnect}
                    className="text-xs text-zinc-500 hover:text-zinc-300"
                  >
                    Disconnect
                  </button>
                </div>
                <div className="flex items-center gap-2">
                  <button
                    onClick={handleDiscoverSpecs}
                    disabled={isDiscovering || isCrawling}
                    className="flex items-center gap-1 px-2 py-1 text-xs rounded border border-zinc-600 bg-zinc-700/50 hover:bg-zinc-700 text-zinc-300 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                  >
                    {isDiscovering ? (
                      <Loader2 className="w-3 h-3 animate-spin" />
                    ) : (
                      <ShieldCheck className="w-3 h-3" />
                    )}
                    {isDiscovering ? "Discovering..." : "Discover Page Specs"}
                  </button>
                  <button
                    onClick={handleDiscoverAllPages}
                    disabled={isDiscovering || isCrawling}
                    className="flex items-center gap-1 px-2 py-1 text-xs rounded border border-zinc-600 bg-zinc-700/50 hover:bg-zinc-700 text-zinc-300 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                  >
                    {isCrawling ? (
                      <Loader2 className="w-3 h-3 animate-spin" />
                    ) : (
                      <Globe className="w-3 h-3" />
                    )}
                    {isCrawling ? "Crawling..." : "Discover All Pages"}
                  </button>
                </div>
                {crawlProgress && <p className="text-[11px] text-zinc-500">{crawlProgress}</p>}
              </div>
            ) : (
              <div className="space-y-2">
                {isConnecting ? (
                  <div className="flex items-center gap-2 text-xs text-zinc-500">
                    <Loader2 className="w-3 h-3 animate-spin" />
                    Connecting...
                  </div>
                ) : (
                  <>
                    <div className="flex items-center gap-2">
                      <WifiOff className="w-3.5 h-3.5 text-zinc-500" />
                      <span className="text-xs text-zinc-500">No SDK app connected.</span>
                      <button
                        onClick={() => setShowManualConnect((v) => !v)}
                        className="text-xs text-zinc-500 hover:text-zinc-300 ml-auto"
                      >
                        Connect manually
                      </button>
                    </div>

                    {showManualConnect && (
                      <div className="flex gap-2">
                        <input
                          className="flex-1 px-2 py-1 bg-zinc-900 border border-zinc-700 rounded-md text-zinc-200 text-xs focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                          placeholder="http://localhost:3001"
                          value={manualUrl}
                          onChange={(e) => setManualUrl(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") handleManualConnect();
                          }}
                        />
                        <button
                          onClick={handleManualConnect}
                          disabled={!manualUrl.trim()}
                          className="px-2 py-1 text-xs rounded border border-zinc-600 bg-zinc-700/50 hover:bg-zinc-700 text-zinc-300 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                        >
                          Connect
                        </button>
                      </div>
                    )}
                  </>
                )}

                {connectionError && <p className="text-xs text-red-400">{connectionError}</p>}
              </div>
            )}
          </div>

          {/* Semantic Spec Group List — Hierarchical by Page */}
          {discoveredSpecs.length > 0 && (
            <div className="rounded-lg border border-zinc-700 bg-zinc-800/50 p-3 space-y-2">
              {/* Header with select all/none/clear */}
              <div className="flex items-center justify-between">
                <span className="text-xs font-medium text-zinc-400 uppercase tracking-wider">
                  Page Specs ({selectedCount}/{totalGroups})
                </span>
                <div className="flex items-center gap-2 text-xs">
                  <button onClick={handleSelectAll} className="text-zinc-500 hover:text-zinc-200">
                    All
                  </button>
                  <span className="text-zinc-600">/</span>
                  <button onClick={handleSelectNone} className="text-zinc-500 hover:text-zinc-200">
                    None
                  </button>
                  <span className="text-zinc-700">|</span>
                  <button
                    onClick={handleClearAll}
                    className="text-zinc-500 hover:text-red-400 flex items-center gap-1"
                  >
                    <Trash2 className="w-3 h-3" />
                    Clear All
                  </button>
                </div>
              </div>

              {/* Hierarchical page → groups display */}
              <div className="max-h-[320px] overflow-y-auto space-y-1 pr-1">
                {Array.from(specsByPage.entries()).map(([pageUrl, { groups, appName }]) => {
                  const checkState = getPageCheckState(pageUrl);
                  const isCollapsed = collapsedPages.has(pageUrl);
                  return (
                    <div key={pageUrl} className="rounded border border-zinc-700/50">
                      {/* Page row */}
                      <div className="flex items-center gap-2 p-1.5 hover:bg-zinc-700/20">
                        <button
                          onClick={() => togglePageCollapse(pageUrl)}
                          className="text-zinc-500 hover:text-zinc-300"
                        >
                          {isCollapsed ? (
                            <ChevronRight className="w-3 h-3" />
                          ) : (
                            <ChevronDown className="w-3 h-3" />
                          )}
                        </button>
                        <input
                          type="checkbox"
                          checked={checkState === "checked"}
                          ref={(el) => {
                            if (el) el.indeterminate = checkState === "indeterminate";
                          }}
                          onChange={() => togglePage(pageUrl)}
                          className="w-4 h-4 rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
                        />
                        <span className="text-xs text-zinc-300 font-medium flex-1 truncate">
                          {pageUrl}
                        </span>
                        {appName && (
                          <span className="text-[10px] px-1.5 py-0 rounded border border-zinc-600 text-zinc-500 shrink-0">
                            {appName}
                          </span>
                        )}
                        <span className="text-[10px] text-zinc-500 shrink-0">
                          {groups.length} group{groups.length !== 1 ? "s" : ""}
                        </span>
                      </div>

                      {/* Nested groups */}
                      {!isCollapsed && (
                        <div className="pl-9 pb-1">
                          {groups.map((group) => (
                            <label
                              key={group.id}
                              className="flex items-start gap-2 p-1 rounded hover:bg-zinc-700/30 cursor-pointer"
                            >
                              <input
                                type="checkbox"
                                checked={selectedGroupIds.has(group.id)}
                                onChange={() => toggleGroup(group.id)}
                                className="mt-0.5 w-4 h-4 rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
                              />
                              <span className="text-xs text-zinc-400">
                                {group.description || group.name}
                              </span>
                            </label>
                          ))}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>

              {/* Prompt Preview */}
              {promptPreview && (
                <div className="border-t border-zinc-700 pt-2">
                  <button
                    onClick={() => setShowPromptPreview((v) => !v)}
                    className="flex items-center gap-1.5 text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
                  >
                    {showPromptPreview ? (
                      <EyeOff className="w-3 h-3" />
                    ) : (
                      <Eye className="w-3 h-3" />
                    )}
                    {showPromptPreview ? "Hide" : "Preview"} AI prompt
                    <span className="text-zinc-600">
                      ({promptPreview.totalGroups} group
                      {promptPreview.totalGroups !== 1 ? "s" : ""} from {promptPreview.pageCount}{" "}
                      page
                      {promptPreview.pageCount !== 1 ? "s" : ""})
                    </span>
                  </button>
                  {showPromptPreview && (
                    <pre className="mt-2 max-h-[200px] overflow-auto rounded-md bg-zinc-900 border border-zinc-700 p-3 text-[11px] text-zinc-400 font-mono whitespace-pre-wrap">
                      {promptPreview.prompt}
                    </pre>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
