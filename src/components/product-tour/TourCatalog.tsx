/**
 * TourCatalog
 *
 * Main page for generating, browsing, and launching product tours.
 * Combines tour generation (from specs) with a catalog of saved tours.
 */

import { useState, useEffect, useCallback } from "react";
import {
  Compass,
  Sparkles,
  Loader2,
  AlertCircle,
  Download,
  RefreshCw,
  CheckSquare,
  Square,
} from "lucide-react";
import { useUIComponent } from "ui-bridge";
import type { SpecConfig } from "@/lib/spec-prompt-builder";
import type {
  ProductTour,
  TourAudience,
  TourGenerationState,
} from "@/types/product-tour";
import {
  INITIAL_GENERATION_STATE,
  PRODUCT_TOURS_STORAGE_KEY,
} from "@/types/product-tour";
import { fetchRegisteredElements, generateTour } from "@/lib/product-tour/tour-generator";
import { exportTour } from "@/lib/product-tour/tour-exporter";
import { instanceStorage } from "@/lib/instance-storage";
import { TourCard } from "./TourCard";
import { TourPlayer } from "./TourPlayer";

// =============================================================================
// Spec Discovery (same pattern as DemoVideoPanel)
// =============================================================================

interface DiscoveredSpecEntry {
  specId: string;
  config: SpecConfig;
}

async function discoverSpecs(): Promise<DiscoveredSpecEntry[]> {
  try {
    const modules = import.meta.glob("../../specs/*.spec.uibridge.json", {
      eager: true,
    }) as Record<string, { default: SpecConfig }>;
    return Object.entries(modules).map(([path, mod]) => {
      const fileName = path.split("/").pop()?.replace(".spec.uibridge.json", "") ?? "unknown";
      return { specId: fileName, config: mod.default };
    });
  } catch {
    return [];
  }
}

// =============================================================================
// Tour Persistence
// =============================================================================

function loadSavedTours(): ProductTour[] {
  return instanceStorage.getJSON<ProductTour[]>(PRODUCT_TOURS_STORAGE_KEY, []);
}

function saveTours(tours: ProductTour[]): void {
  instanceStorage.setJSON(PRODUCT_TOURS_STORAGE_KEY, tours);
}

// =============================================================================
// TourCatalog Component
// =============================================================================

export function TourCatalog() {
  useUIComponent({
    id: "product-tour-catalog",
    name: "Product Tours",
    description: "Generate and manage interactive product tours from page specs",
    actions: [],
  });

  // Spec discovery
  const [specs, setSpecs] = useState<DiscoveredSpecEntry[]>([]);
  const [selectedSpecIds, setSelectedSpecIds] = useState<Set<string>>(new Set());
  const [audience, setAudience] = useState<TourAudience>("new-user");

  // Generation
  const [genState, setGenState] = useState<TourGenerationState>(INITIAL_GENERATION_STATE);

  // Catalog
  const [savedTours, setSavedTours] = useState<ProductTour[]>([]);

  // Player
  const [activeTour, setActiveTour] = useState<ProductTour | null>(null);

  // Load on mount
  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect -- data loading on mount */
    discoverSpecs().then(setSpecs);
    setSavedTours(loadSavedTours());
    /* eslint-enable react-hooks/set-state-in-effect */
  }, []);

  const toggleSpec = useCallback((id: string) => {
    setSelectedSpecIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const toggleAll = useCallback(() => {
    setSelectedSpecIds((prev) =>
      prev.size === specs.length ? new Set() : new Set(specs.map((s) => s.specId)),
    );
  }, [specs]);

  // -------------------------------------------------------------------------
  // Generate Tours
  // -------------------------------------------------------------------------
  const handleGenerate = useCallback(async () => {
    const selected = specs.filter((s) => selectedSpecIds.has(s.specId));
    if (selected.length === 0) return;

    setGenState({ phase: "generating", tours: [], error: null });

    try {
      const elements = await fetchRegisteredElements();
      const tours: ProductTour[] = [];

      for (const entry of selected) {
        const tour = await generateTour(entry.config, elements, audience);
        tours.push(tour);
      }

      setGenState({ phase: "previewing", tours, error: null });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setGenState({ phase: "error", tours: [], error: msg });
    }
  }, [specs, selectedSpecIds, audience]);

  // -------------------------------------------------------------------------
  // Save Generated Tours
  // -------------------------------------------------------------------------
  const handleSave = useCallback(() => {
    const existing = loadSavedTours();
    const newIds = new Set(genState.tours.map((t) => t.id));
    // Replace existing tours with same ID, append new ones
    const merged = [
      ...existing.filter((t) => !newIds.has(t.id)),
      ...genState.tours,
    ];
    saveTours(merged);
    setSavedTours(merged);
    setGenState({ phase: "done", tours: genState.tours, error: null });
  }, [genState.tours]);

  // -------------------------------------------------------------------------
  // Export
  // -------------------------------------------------------------------------
  const handleExport = useCallback((tour: ProductTour, format: "html" | "json" | "markdown" = "html") => {
    const exports = exportTour(tour);
    const formatMap = {
      html: { content: exports.html, mime: "text/html", ext: "html" },
      json: { content: exports.json, mime: "application/json", ext: "json" },
      markdown: { content: exports.markdown, mime: "text/markdown", ext: "md" },
    };
    const { content, mime, ext } = formatMap[format];
    const blob = new Blob([content], { type: mime });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${tour.id}.${ext}`;
    a.click();
    URL.revokeObjectURL(url);
  }, []);

  // -------------------------------------------------------------------------
  // Delete
  // -------------------------------------------------------------------------
  const handleDelete = useCallback((tourId: string) => {
    const updated = loadSavedTours().filter((t) => t.id !== tourId);
    saveTours(updated);
    setSavedTours(updated);
  }, []);

  // -------------------------------------------------------------------------
  // Reset
  // -------------------------------------------------------------------------
  const handleReset = useCallback(() => {
    setGenState(INITIAL_GENERATION_STATE);
  }, []);

  // =========================================================================
  // Group saved tours by audience
  // =========================================================================
  const toursByAudience: Record<TourAudience, ProductTour[]> = {
    "new-user": savedTours.filter((t) => t.targetAudience === "new-user"),
    "power-user": savedTours.filter((t) => t.targetAudience === "power-user"),
    evaluator: savedTours.filter((t) => t.targetAudience === "evaluator"),
  };

  // =========================================================================
  // Render
  // =========================================================================
  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex items-center gap-3 p-4 border-b border-border shrink-0">
        <Compass className="h-5 w-5 text-primary" />
        <h2 className="text-lg font-semibold">Product Tours</h2>
        {genState.phase !== "idle" && (
          <button
            onClick={handleReset}
            className="ml-auto text-xs text-muted-foreground hover:text-foreground flex items-center gap-1"
          >
            <RefreshCw className="h-3 w-3" />
            Reset
          </button>
        )}
      </div>

      <div className="flex-1 overflow-y-auto p-4 space-y-6">
        {/* ---- GENERATION SECTION ---- */}
        <div className="space-y-4">
          <h3 className="text-sm font-medium">Generate New Tours</h3>

          {/* Page multi-select */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <label className="text-xs text-muted-foreground">Select Pages</label>
              <button
                onClick={toggleAll}
                disabled={specs.length === 0}
                className="text-xs text-muted-foreground hover:text-foreground"
              >
                {selectedSpecIds.size === specs.length ? "Deselect all" : "Select all"}
              </button>
            </div>
            {specs.length === 0 ? (
              <p className="text-xs text-muted-foreground">No specs found</p>
            ) : (
              <div className="max-h-36 overflow-y-auto border border-border rounded-md divide-y divide-border">
                {specs.map((s) => {
                  const selected = selectedSpecIds.has(s.specId);
                  return (
                    <button
                      key={s.specId}
                      onClick={() => toggleSpec(s.specId)}
                      className={`w-full flex items-center gap-2 px-3 py-1.5 text-left text-xs hover:bg-accent/50 ${
                        selected ? "bg-accent/30" : ""
                      }`}
                    >
                      {selected ? (
                        <CheckSquare className="h-3.5 w-3.5 text-primary shrink-0" />
                      ) : (
                        <Square className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                      )}
                      <span className="truncate">
                        {s.config.metadata?.component || s.specId}
                      </span>
                    </button>
                  );
                })}
              </div>
            )}
          </div>

          {/* Audience selector */}
          <div className="space-y-1">
            <label className="text-xs text-muted-foreground">Target Audience</label>
            <select
              value={audience}
              onChange={(e) => setAudience(e.target.value as TourAudience)}
              className="w-full rounded border border-border bg-background px-2 py-1.5 text-sm"
            >
              <option value="new-user">New User — basics & common workflows</option>
              <option value="power-user">Power User — advanced features</option>
              <option value="evaluator">Evaluator — differentiators & ROI</option>
            </select>
          </div>

          {/* Generate button */}
          <div className="flex items-center gap-3">
            {(genState.phase === "idle" || genState.phase === "error" || genState.phase === "done") && (
              <button
                onClick={handleGenerate}
                disabled={selectedSpecIds.size === 0}
                className="flex items-center gap-2 px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
              >
                <Sparkles className="h-4 w-4" />
                Generate Tour{selectedSpecIds.size > 1 ? "s" : ""}
              </button>
            )}

            {genState.phase === "previewing" && (
              <>
                <button
                  onClick={handleSave}
                  className="flex items-center gap-2 px-4 py-2 rounded-md bg-green-600 text-white text-sm font-medium hover:bg-green-700"
                >
                  <Download className="h-4 w-4" />
                  Save {genState.tours.length} Tour{genState.tours.length !== 1 ? "s" : ""}
                </button>
                <button
                  onClick={handleGenerate}
                  className="flex items-center gap-2 px-3 py-2 rounded-md border border-border text-sm hover:bg-accent"
                >
                  <RefreshCw className="h-3.5 w-3.5" />
                  Regenerate
                </button>
              </>
            )}
          </div>

          {/* Status */}
          {genState.phase === "generating" && (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" />
              Generating tour{selectedSpecIds.size > 1 ? "s" : ""} with AI...
            </div>
          )}
          {genState.phase === "error" && genState.error && (
            <div className="flex items-start gap-2 text-sm text-red-400 bg-red-500/10 border border-red-500/20 rounded-md p-3">
              <AlertCircle className="h-4 w-4 shrink-0 mt-0.5" />
              <span>{genState.error}</span>
            </div>
          )}
          {genState.phase === "done" && (
            <p className="text-sm text-green-400">
              {genState.tours.length} tour{genState.tours.length !== 1 ? "s" : ""} saved
            </p>
          )}

          {/* Preview generated tours */}
          {genState.phase === "previewing" && genState.tours.length > 0 && (
            <div className="space-y-3">
              <h4 className="text-xs font-medium text-muted-foreground">Preview</h4>
              <div className="grid gap-3">
                {genState.tours.map((tour) => (
                  <TourCard
                    key={tour.id}
                    tour={tour}
                    onLaunch={setActiveTour}
                    onExport={handleExport}
                  />
                ))}
              </div>
            </div>
          )}
        </div>

        {/* ---- SAVED TOURS CATALOG ---- */}
        {savedTours.length > 0 && (
          <div className="space-y-4 border-t border-border pt-6">
            <h3 className="text-sm font-medium">Saved Tours</h3>

            {(["new-user", "power-user", "evaluator"] as TourAudience[]).map((aud) => {
              const tours = toursByAudience[aud];
              if (tours.length === 0) return null;
              return (
                <div key={aud} className="space-y-2">
                  <h4 className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
                    {aud === "new-user"
                      ? "Getting Started"
                      : aud === "power-user"
                        ? "Power Features"
                        : "Product Overview"}
                  </h4>
                  <div className="grid gap-3 sm:grid-cols-2">
                    {tours.map((tour) => (
                      <div key={tour.id} className="relative group">
                        <TourCard
                          tour={tour}
                          onLaunch={setActiveTour}
                          onExport={handleExport}
                        />
                        <button
                          onClick={() => handleDelete(tour.id)}
                          className="absolute top-2 right-2 text-xs text-muted-foreground hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
                        >
                          Remove
                        </button>
                      </div>
                    ))}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* ---- TOUR PLAYER ---- */}
      {activeTour && (
        <TourPlayer
          tour={activeTour}
          onClose={() => setActiveTour(null)}
          onComplete={() => setActiveTour(null)}
        />
      )}
    </div>
  );
}
