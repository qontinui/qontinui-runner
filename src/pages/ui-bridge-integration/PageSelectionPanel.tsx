/**
 * PageSelectionPanel — Discover and select pages for AI-powered preparation.
 *
 * Shows a checklist of discovered page/route components with their current
 * integration status (registrations, specs, tutorials). Users select which
 * pages to process and choose generation options.
 */

import { useState, useCallback, useEffect } from "react";
import { FileCode, CheckCircle2, XCircle, Loader2, RefreshCw, Sparkles } from "lucide-react";
import type {
  PageComponent,
  DiscoverPagesResult,
  PageGenerationOptions,
  ApiResponse,
} from "./types";
import { getApiBase } from "@/lib/runner-api";

interface PageSelectionPanelProps {
  projectPath: string;
  onStartGeneration: (selectedPages: PageComponent[], options: PageGenerationOptions) => void;
  disabled?: boolean;
}

export function PageSelectionPanel({
  projectPath,
  onStartGeneration,
  disabled,
}: PageSelectionPanelProps) {
  const [pages, setPages] = useState<PageComponent[]>([]);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [options, setOptions] = useState<PageGenerationOptions>({
    generateRegistrations: true,
    generateDataPageIds: true,
    generateSpecs: true,
    generateTutorials: false,
    generateDemoVideos: false,
    generateProductTours: false,
  });

  const discoverPages = useCallback(async () => {
    if (!projectPath) return;
    setLoading(true);
    setError(null);
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/discover-pages`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ project_path: projectPath }),
      });
      const data: ApiResponse<DiscoverPagesResult> = await resp.json();
      if (data.success && data.data) {
        setPages(data.data.pages);
        // Auto-select pages that need work
        const needsWork = new Set(
          data.data.pages
            .filter(
              (p) => !p.has_registrations || !p.has_data_page_id || !p.has_spec || !p.has_tutorial,
            )
            .map((p) => p.component_path),
        );
        setSelectedIds(needsWork);
      } else {
        setError(data.error || "Discovery failed");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Discovery failed");
    } finally {
      setLoading(false);
    }
  }, [projectPath]);

  useEffect(() => {
    if (projectPath) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      discoverPages();
    }
  }, [projectPath, discoverPages]);

  const togglePage = (path: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const selectAll = () => setSelectedIds(new Set(pages.map((p) => p.component_path)));
  const deselectAll = () => setSelectedIds(new Set());

  const selectedPages = pages.filter((p) => selectedIds.has(p.component_path));

  const statusBadge = (page: PageComponent) => {
    const total =
      (page.has_registrations ? 1 : 0) +
      (page.has_data_page_id ? 1 : 0) +
      (page.has_spec ? 1 : 0) +
      (page.has_tutorial ? 1 : 0);
    if (total === 4)
      return (
        <span className="text-xs px-1.5 py-0.5 rounded bg-green-500/20 text-green-400">
          Complete
        </span>
      );
    if (total > 0)
      return (
        <span className="text-xs px-1.5 py-0.5 rounded bg-amber-500/20 text-amber-400">
          Partial
        </span>
      );
    return <span className="text-xs px-1.5 py-0.5 rounded bg-blue-500/20 text-blue-400">New</span>;
  };

  const checkIcon = (has: boolean) =>
    has ? (
      <CheckCircle2 className="h-3.5 w-3.5 text-green-500" />
    ) : (
      <XCircle className="h-3.5 w-3.5 text-muted-foreground/40" />
    );

  if (pages.length === 0 && !loading && !error) return null;

  return (
    <div className="border border-border rounded-md p-4 space-y-3">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <FileCode className="h-4 w-4 text-cyan-400" />
          <h3 className="text-sm font-semibold">Page Discovery</h3>
          {pages.length > 0 && (
            <span className="text-xs text-muted-foreground">
              ({pages.length} pages, {selectedIds.size} selected)
            </span>
          )}
        </div>
        <button
          onClick={discoverPages}
          disabled={loading}
          className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
        >
          <RefreshCw className={`h-3 w-3 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>

      {error && (
        <div className="text-xs text-destructive bg-destructive/10 p-2 rounded">{error}</div>
      )}

      {loading && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground py-4 justify-center">
          <Loader2 className="h-4 w-4 animate-spin" />
          Discovering pages...
        </div>
      )}

      {/* Page list */}
      {pages.length > 0 && (
        <>
          {/* Select all / deselect all */}
          <div className="flex gap-2 text-xs">
            <button onClick={selectAll} className="text-primary hover:underline">
              Select all
            </button>
            <span className="text-muted-foreground">|</span>
            <button onClick={deselectAll} className="text-primary hover:underline">
              Deselect all
            </button>
          </div>

          {/* Table */}
          <div className="border border-border rounded overflow-hidden">
            <table className="w-full text-xs">
              <thead>
                <tr className="bg-muted/50 text-muted-foreground">
                  <th className="p-2 text-left w-8"></th>
                  <th className="p-2 text-left">Route</th>
                  <th className="p-2 text-left">Component</th>
                  <th className="p-2 text-center w-14">Regs</th>
                  <th className="p-2 text-center w-14" title="data-page-id attribute">
                    Page ID
                  </th>
                  <th className="p-2 text-center w-14">Spec</th>
                  <th className="p-2 text-center w-14">Tutorial</th>
                  <th className="p-2 text-right w-20">Status</th>
                </tr>
              </thead>
              <tbody>
                {pages.map((page) => (
                  <tr
                    key={page.component_path}
                    className="border-t border-border hover:bg-accent/30 cursor-pointer"
                    onClick={() => togglePage(page.component_path)}
                  >
                    <td className="p-2 text-center">
                      <input
                        type="checkbox"
                        checked={selectedIds.has(page.component_path)}
                        onChange={() => togglePage(page.component_path)}
                        className="rounded"
                      />
                    </td>
                    <td className="p-2 font-mono">{page.route}</td>
                    <td className="p-2 text-muted-foreground">{page.component_name}</td>
                    <td className="p-2 text-center">{checkIcon(page.has_registrations)}</td>
                    <td className="p-2 text-center">{checkIcon(page.has_data_page_id)}</td>
                    <td className="p-2 text-center">{checkIcon(page.has_spec)}</td>
                    <td className="p-2 text-center">{checkIcon(page.has_tutorial)}</td>
                    <td className="p-2 text-right">{statusBadge(page)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {/* Generation options */}
          <div className="flex gap-4 text-xs">
            <label className="flex items-center gap-1.5 cursor-pointer">
              <input
                type="checkbox"
                checked={options.generateRegistrations}
                onChange={(e) =>
                  setOptions({
                    ...options,
                    generateRegistrations: e.target.checked,
                  })
                }
                className="rounded"
              />
              Registrations
            </label>
            <label
              className="flex items-center gap-1.5 cursor-pointer"
              title="Add data-page-id attributes to page root containers for state machine detection"
            >
              <input
                type="checkbox"
                checked={options.generateDataPageIds}
                onChange={(e) =>
                  setOptions({
                    ...options,
                    generateDataPageIds: e.target.checked,
                  })
                }
                className="rounded"
              />
              Page IDs
            </label>
            <label className="flex items-center gap-1.5 cursor-pointer">
              <input
                type="checkbox"
                checked={options.generateSpecs}
                onChange={(e) => setOptions({ ...options, generateSpecs: e.target.checked })}
                className="rounded"
              />
              Page Specs
            </label>
            <label className="flex items-center gap-1.5 cursor-pointer">
              <input
                type="checkbox"
                checked={options.generateTutorials}
                onChange={(e) =>
                  setOptions({
                    ...options,
                    generateTutorials: e.target.checked,
                  })
                }
                className="rounded"
              />
              Tutorials
            </label>
            <label className="flex items-center gap-1.5 cursor-pointer">
              <input
                type="checkbox"
                checked={options.generateDemoVideos}
                onChange={(e) =>
                  setOptions({
                    ...options,
                    generateDemoVideos: e.target.checked,
                  })
                }
                className="rounded"
              />
              Demo Videos
            </label>
            <label className="flex items-center gap-1.5 cursor-pointer">
              <input
                type="checkbox"
                checked={options.generateProductTours}
                onChange={(e) =>
                  setOptions({
                    ...options,
                    generateProductTours: e.target.checked,
                  })
                }
                className="rounded"
              />
              Product Tours
            </label>
          </div>

          {/* Generate button */}
          <button
            onClick={() => onStartGeneration(selectedPages, options)}
            disabled={
              disabled ||
              selectedPages.length === 0 ||
              (!options.generateRegistrations &&
                !options.generateSpecs &&
                !options.generateTutorials &&
                !options.generateDemoVideos &&
                !options.generateProductTours)
            }
            className="flex items-center gap-2 px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
          >
            <Sparkles className="h-4 w-4" />
            Generate for {selectedPages.length} page
            {selectedPages.length !== 1 ? "s" : ""}
          </button>
        </>
      )}
    </div>
  );
}
