import { useState, useEffect, useRef, useMemo, useCallback, useSyncExternalStore } from "react";
import type { ReactNode } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { Search } from "lucide-react";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";
import type { TerminalTab } from "./useTerminalManager";
import { fuzzyScore } from "./commands/fuzzy";
import {
  getAll as getRegistrySnapshot,
  getRegistryPaletteActions,
  scorePaletteLabel,
  subscribe as subscribeToRegistry,
} from "./commands";

/** DOM id of the results listbox, referenced by the input's
 *  `aria-controls` / `aria-activedescendant`. */
const LISTBOX_ID = "command-palette-listbox";

/** Stable per-row DOM id + `data-page-element` token, so the keyboard
 *  selection is READABLE from outside the app. The rows used to be
 *  `role="button"` with the highlight living only in a Tailwind class —
 *  no listbox, no option, no `aria-selected`, no `data-page-element` —
 *  so nothing outside React could tell which row Enter would run. Mirrors
 *  `CommandBar.tsx`'s `optionId` / `suggestionElementId`. */
function paletteOptionId(actionId: string): string {
  return `command-palette-option-${actionId}`;
}

function renderHighlightedLabel(label: string, indices: number[]): ReactNode {
  if (indices.length === 0) return label;
  const indexSet = new Set(indices);
  return label.split("").map((char, i) =>
    indexSet.has(i) ? (
      <span key={`hl-${i}`} className="text-[#7aa2f7] font-medium">
        {char}
      </span>
    ) : (
      <span key={`ch-${i}`}>{char}</span>
    ),
  );
}

/* ── Types ───────────────────────────────────────────────────────────── */

interface PaletteAction {
  id: string;
  label: string;
  shortcut?: string;
  category: string;
  priority: number;
  action: () => void;
}

interface CommandPaletteProps {
  onClose: () => void;
  tabs: TerminalTab[];
  assignments: ZoneAssignments;
  sessionStates: Record<string, SessionState>;
  focusedZone: number;
  onFocusZone: (zoneIndex: number) => void;
  onApproveTab: (tabId: string) => void;
  onRejectTab: (tabId: string) => void;
  onRestartZone: (zoneIndex: number) => void;
  onTogglePin: (zoneIndex: number) => void;
  pinnedZones: Set<number>;
  onApproveAll: () => void;
  onSortZones: () => void;
  onExport: () => void;
  onToggleFocusMode: () => void;
  focusMode: boolean;
  onToggleAutoFocus: () => void;
  autoFocus: boolean;
  onToggleSound: () => void;
  soundEnabled: boolean;
  zoneLabels: Record<number, string>;
  onSetZoneLabel: (zoneIndex: number, label: string) => void;
  zoneCount: number;
  onCompareZones?: (z1: number, z2: number) => void;
  onSnapshotZone?: (tabId: string) => void;
  onCompareSnapshot?: (tabId: string) => void;
  snapshotZones?: Set<string>;
}

export function CommandPalette({
  onClose,
  tabs,
  assignments,
  sessionStates,
  focusedZone,
  onFocusZone,
  onApproveTab,
  onRejectTab,
  onRestartZone,
  onTogglePin,
  pinnedZones,
  onApproveAll,
  onSortZones,
  onExport,
  onToggleFocusMode,
  focusMode,
  onToggleAutoFocus,
  autoFocus,
  onToggleSound,
  soundEnabled,
  zoneLabels,
  onSetZoneLabel,
  zoneCount,
  onCompareZones,
  onSnapshotZone,
  onCompareSnapshot,
  snapshotZones,
}: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [prevQuery, setPrevQuery] = useState("");
  const [recentIds, setRecentIds] = useState<string[]>(() =>
    instanceStorage.getJSON<string[]>("zone-recent-commands", []),
  );
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Phase 7 — subscribe to the command registry so a new
  // `useCommandAction` registration appears in the palette without a
  // re-mount. Snapshot identity is stable between mutations, so this is
  // cheap re-key for the useMemo below.
  const registrySnapshot = useSyncExternalStore(subscribeToRegistry, getRegistrySnapshot);

  // Build action list
  const actions = useMemo(() => {
    const list: PaletteAction[] = [];

    // Phase 7 — prepend every registry action as a discoverability row.
    // Categorised under "Commands" so the palette's existing
    // category-tinted grouping treats them as a distinct block at the
    // top of the list. Their `priority: 0` lands them just below the
    // (deduped) `approve-all` and above the per-zone enumerations.
    for (const row of getRegistryPaletteActions()) {
      list.push(row as PaletteAction);
    }
    // Capture which registry slashes we've already projected so the
    // hard-coded equivalents (`approve-all`, etc.) below can dedupe.
    const projectedSlashes = new Set(registrySnapshot.map((a) => a.slash));

    // Per-zone actions
    for (let z = 0; z < zoneCount; z++) {
      const tabId = assignments[z];
      const tab = tabs.find((t) => t.id === tabId);
      const state = tabId ? (sessionStates[tabId] ?? "idle") : "idle";
      const label = zoneLabels[z];
      const name = tab?.title ?? `Zone ${z + 1}`;
      const isPinned = pinnedZones.has(z);

      list.push({
        id: `focus-${z}`,
        label: `Focus zone ${z + 1}: ${name}`,
        shortcut: z < 9 ? `Ctrl+${z + 1}` : undefined,
        category: "Navigation",
        priority: z === focusedZone ? 10 : 5,
        action: () => onFocusZone(z),
      });

      if (state === "needs-input" && tabId) {
        list.push({
          id: `approve-${z}`,
          label: `Approve zone ${z + 1}: ${name}`,
          category: "Actions",
          priority: 0,
          action: () => onApproveTab(tabId),
        });
        list.push({
          id: `reject-${z}`,
          label: `Reject zone ${z + 1}: ${name}`,
          category: "Actions",
          priority: 1,
          action: () => onRejectTab(tabId),
        });
      }

      if (state === "completed" || state === "error") {
        list.push({
          id: `restart-${z}`,
          label: `Restart zone ${z + 1}: ${name}`,
          shortcut: "Ctrl+Shift+R",
          category: "Actions",
          priority: 2,
          action: () => onRestartZone(z),
        });
      }

      list.push({
        id: `pin-${z}`,
        label: `${isPinned ? "Unpin" : "Pin"} zone ${z + 1}: ${name}`,
        shortcut: "Ctrl+Shift+P",
        category: "View",
        priority: 8,
        action: () => onTogglePin(z),
      });

      if (tab) {
        list.push({
          id: `label-${z}`,
          label: `Label zone ${z + 1}${label ? ` (${label})` : ""}`,
          category: "Edit",
          priority: 9,
          action: () => {
            const newLabel = prompt(`Label for zone ${z + 1}:`, label ?? "");
            if (newLabel !== null) onSetZoneLabel(z, newLabel);
          },
        });
      }

      // Snapshot current output
      if (tabId && onSnapshotZone) {
        list.push({
          id: `snapshot-${z}`,
          label: `Snapshot zone ${z + 1}: ${name}`,
          category: "Snapshot",
          priority: 4,
          action: () => onSnapshotZone(tabId),
        });
      }

      // Compare with snapshot (only if snapshot exists)
      if (tabId && snapshotZones?.has(tabId) && onCompareSnapshot) {
        list.push({
          id: `compare-snapshot-${z}`,
          label: `Compare zone ${z + 1} with snapshot`,
          category: "Snapshot",
          priority: 3,
          action: () => onCompareSnapshot(tabId),
        });
      }
    }

    // Global actions
    const needsInputCount = Object.values(sessionStates).filter((s) => s === "needs-input").length;
    // Phase 7 dedupe — only render the hard-coded `approve-all` row
    // when the registry hasn't already projected its equivalent. This
    // is the one "delete the duplicated construction" beat from the
    // plan that actually applies; the per-zone focus / restart loops
    // below stay because they carry per-tab titles the registry's
    // abstract `/focus N` row can't reproduce.
    if (needsInputCount > 0 && !projectedSlashes.has("/approve-all")) {
      list.push({
        id: "approve-all",
        label: `Approve all (${needsInputCount} waiting)`,
        shortcut: "Ctrl+Shift+Enter",
        category: "Batch",
        priority: -1,
        action: onApproveAll,
      });
    }

    list.push({
      id: "sort-zones",
      label: "Sort zones by state",
      category: "View",
      priority: 6,
      action: onSortZones,
    });

    list.push({
      id: "export",
      label: "Export all session output",
      category: "Actions",
      priority: 7,
      action: onExport,
    });

    list.push({
      id: "focus-mode",
      label: `Focus mode: ${focusMode ? "OFF" : "ON"}`,
      shortcut: "Ctrl+Shift+D",
      category: "View",
      priority: 7,
      action: onToggleFocusMode,
    });

    list.push({
      id: "auto-focus",
      label: `Auto-focus on waiting: ${autoFocus ? "OFF" : "ON"}`,
      shortcut: "Ctrl+Shift+A",
      category: "View",
      priority: 7,
      action: onToggleAutoFocus,
    });

    list.push({
      id: "sound",
      label: `Sound notifications: ${soundEnabled ? "OFF" : "ON"}`,
      shortcut: "Ctrl+Shift+S",
      category: "View",
      priority: 7,
      action: onToggleSound,
    });

    list.push({
      id: "group-by-dir",
      label: "Auto-tag zones by working directory",
      category: "Actions",
      priority: 4,
      action: () => {
        for (let z = 0; z < zoneCount; z++) {
          const tabId = assignments[z];
          if (!tabId) continue;
          const tab = tabs.find((t) => t.id === tabId);
          if (!tab) continue;

          const title = tab.title;
          let dir = title;

          // Try to extract path and get last segment
          const pathMatch = title.match(/[/\\]([^/\\]+)\s*$/);
          if (pathMatch) {
            dir = pathMatch[1];
          } else {
            // Try after colon
            const colonMatch = title.match(/:\s*(.+)/);
            if (colonMatch) {
              const afterColon = colonMatch[1].trim();
              const lastSeg = afterColon.match(/[/\\]([^/\\]+)\s*$/);
              dir = lastSeg ? lastSeg[1] : afterColon;
            }
          }

          // Get current tags and add directory tag if not present
          const currentLabel = zoneLabels[z] ?? "";
          const currentTags = currentLabel
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean);
          if (!currentTags.includes(dir)) {
            const newLabel = [...currentTags, dir].join(", ");
            onSetZoneLabel(z, newLabel);
          }
        }
      },
    });

    // Compare zones (only when 2+ occupied zones exist)
    if (onCompareZones) {
      const occupiedZones = Array.from({ length: zoneCount }, (_, z) => z).filter(
        (z) => assignments[z],
      );
      if (occupiedZones.length >= 2) {
        // Add compare actions for focused zone vs each other zone
        for (const other of occupiedZones) {
          if (other === focusedZone) continue;
          const otherTab = tabs.find((t) => t.id === assignments[other]);
          list.push({
            id: `compare-${focusedZone}-${other}`,
            label: `Compare focused with zone ${other + 1}: ${otherTab?.title ?? "unknown"}`,
            category: "Compare",
            priority: 3,
            action: () => onCompareZones(focusedZone, other),
          });
        }
      }
    }

    return list.sort((a, b) => a.priority - b.priority);
  }, [
    registrySnapshot, // Phase 7 — re-run projection when actions register/unregister
    tabs,
    assignments,
    sessionStates,
    focusedZone,
    zoneCount,
    pinnedZones,
    zoneLabels,
    focusMode,
    autoFocus,
    soundEnabled,
    onFocusZone,
    onApproveTab,
    onRejectTab,
    onRestartZone,
    onTogglePin,
    onApproveAll,
    onSortZones,
    onExport,
    onToggleFocusMode,
    onToggleAutoFocus,
    onToggleSound,
    onSetZoneLabel,
    onCompareZones,
    onSnapshotZone,
    onCompareSnapshot,
    snapshotZones,
  ]);

  // Execute action and track in recent commands
  const executeAction = useCallback(
    (action: PaletteAction) => {
      setRecentIds((prev) => {
        const next = [action.id, ...prev.filter((id) => id !== action.id)].slice(0, 5);
        instanceStorage.setJSON("zone-recent-commands", next);
        return next;
      });
      action.action();
      onClose();
    },
    [onClose],
  );

  // ONE scored pass, feeding both the ordering and the highlight
  // indices. They used to be two separate `fuzzyScore` walks over the
  // same actions — the exact shape that lets a scoring change land on
  // one and not the other. `null` means "no query": browse mode.
  const scored = useMemo(() => {
    const q = query.trim();
    if (!q) return null;

    const out: {
      action: PaletteAction;
      score: number;
      indices: number[];
      fromSlash: boolean;
    }[] = [];

    for (const action of actions) {
      // Slash-aware: a registry row's label is `"/slash — Description"`,
      // and the two halves are scored SEPARATELY so the palette ranks
      // them the way the CommandBar's `resolve()` does.
      const labelResult = scorePaletteLabel(action.label, q);
      const catResult = fuzzyScore(action.category, q);
      const best =
        labelResult && (!catResult || labelResult.score >= catResult.score) ? labelResult : null;

      if (best) {
        out.push({
          action,
          score: best.score,
          indices: best.indices,
          fromSlash: best.fromSlash,
        });
      } else if (catResult) {
        out.push({ action, score: catResult.score, indices: [], fromSlash: false });
      }
    }

    // Same sort keys as `commands/resolve.ts`, minus recency (the
    // palette pins recents in browse mode instead): score first, then
    // WHICH FIELD matched. Sorting on score alone left an exact tie to
    // array order, so `rst` topped out at `/auto-restart` here while the
    // bar completed `/restart`. Ties below both keys fall to registry
    // order, which `getRegistryPaletteActions()` now preserves.
    out.sort((a, b) => {
      if (a.score !== b.score) return b.score - a.score;
      if (a.fromSlash !== b.fromSlash) return a.fromSlash ? -1 : 1;
      return 0;
    });
    return out;
  }, [actions, query]);

  const filtered = useMemo(() => {
    if (scored) return scored.map((s) => s.action);

    // Browse mode: recent commands first, then all actions.
    const recent: PaletteAction[] = [];
    for (const rid of recentIds) {
      const action = actions.find((a) => a.id === rid);
      if (action) {
        recent.push({ ...action, category: "Recent" });
      }
    }
    // Remove duplicates from main list
    const recentIdSet = new Set(recentIds);
    const rest = actions.filter((a) => !recentIdSet.has(a.id));
    return [...recent, ...rest];
  }, [scored, actions, recentIds]);

  // Match indices for highlighting (kept out of PaletteAction), read off
  // the same pass that produced the ordering.
  const matchIndicesMap = useMemo(() => {
    const map = new Map<string, number[]>();
    for (const s of scored ?? []) map.set(s.action.id, s.indices);
    return map;
  }, [scored]);

  // Reset selected index when query changes (derived state computed during render)
  if (query !== prevQuery) {
    setPrevQuery(query);
    setSelectedIndex(0);
  }

  // Keyboard navigation
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIndex((prev) => Math.min(prev + 1, filtered.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIndex((prev) => Math.max(prev - 1, 0));
        return;
      }
      if (e.key === "Enter" && filtered.length > 0) {
        e.preventDefault();
        const action = filtered[selectedIndex];
        if (action) executeAction(action);
        return;
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [onClose, filtered, selectedIndex, executeAction]);

  // Auto-scroll selected item into view
  useEffect(() => {
    const el = listRef.current?.children[selectedIndex] as HTMLElement | undefined;
    el?.scrollIntoView({ block: "nearest" });
  }, [selectedIndex]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh] bg-black/50 backdrop-blur-sm"
      role="button"
      tabIndex={0}
      aria-label="Close command palette"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      onKeyDown={(e) => {
        if ((e.key === "Enter" || e.key === " ") && e.target === e.currentTarget) onClose();
      }}
    >
      <div
        data-page-element="command-palette"
        className="bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl w-[440px] max-h-[60vh] flex flex-col overflow-hidden"
      >
        {/* Search input */}
        <div className="flex items-center gap-2 px-4 py-3 border-b border-[#2a2d3d]">
          <Search className="w-4 h-4 text-[#565f89] shrink-0" />
          <input
            ref={inputRef}
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Type a command..."
            data-page-element="command-palette-input"
            // Stable accessible name — the placeholder is not one to rely
            // on, and the combobox/listbox pairing is what makes the
            // selected row addressable from outside.
            aria-label="Command palette"
            role="combobox"
            aria-expanded={filtered.length > 0}
            aria-autocomplete="list"
            aria-controls={filtered.length > 0 ? LISTBOX_ID : undefined}
            aria-activedescendant={
              filtered[selectedIndex] ? paletteOptionId(filtered[selectedIndex].id) : undefined
            }
            className="flex-1 bg-transparent text-sm text-[#c0caf5] placeholder-[#565f89] outline-hidden"
          />
          <kbd className="text-[9px] font-mono text-[#565f89] bg-[#2a2d3d] rounded px-1.5 py-0.5">
            Esc
          </kbd>
        </div>

        {/* Results list. `role="listbox"` + one `role="option"` per row is
            what makes the keyboard selection readable from outside — same
            contract as CommandBar's suggestion dropdown. `listRef` is on
            the listbox itself so the scroll-into-view index and the option
            index are the same list (the "Clear recent commands" button is
            a SIBLING of the listbox, not an option inside it). */}
        <div className="flex-1 overflow-y-auto scrollbar-dark py-1">
          {filtered.length === 0 ? (
            <div className="px-4 py-6 text-center text-[12px] text-[#565f89]">
              No matching commands
            </div>
          ) : (
            <>
              <div ref={listRef} id={LISTBOX_ID} role="listbox" aria-label="Palette commands">
                {filtered.map((action, i) => (
                  <div
                    key={action.id}
                    id={paletteOptionId(action.id)}
                    data-page-element={paletteOptionId(action.id)}
                    role="option"
                    aria-selected={i === selectedIndex}
                    onClick={() => executeAction(action)}
                    onMouseEnter={() => setSelectedIndex(i)}
                    // No per-row `tabIndex`/`onKeyDown`: this is the
                    // aria-activedescendant pattern, where focus stays on
                    // the input and the window-level keydown handler above
                    // owns ArrowUp/ArrowDown/Enter. Rows were previously
                    // `role="button" tabIndex={0}`, which put 100+ stops in
                    // the tab order for a list the keyboard already drives.
                    className={`flex items-center justify-between px-4 py-1.5 cursor-pointer transition-colors ${
                      i === selectedIndex
                        ? "bg-[#7aa2f7]/15 text-[#c0caf5]"
                        : "text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                    }`}
                  >
                    <div className="flex items-center gap-2 min-w-0">
                      <span
                        className={`text-[9px] uppercase tracking-wider w-14 shrink-0 ${
                          action.category === "Recent" ? "text-[#7aa2f7]" : "text-[#565f89]"
                        }`}
                      >
                        {action.category}
                      </span>
                      <span className="text-[12px] truncate">
                        {renderHighlightedLabel(action.label, matchIndicesMap.get(action.id) ?? [])}
                      </span>
                    </div>
                    {action.shortcut && (
                      <kbd className="text-[9px] font-mono text-[#565f89] bg-[#2a2d3d] border border-[#3b3d57] rounded px-1 py-0.5 ml-3 shrink-0 whitespace-nowrap">
                        {action.shortcut}
                      </kbd>
                    )}
                  </div>
                ))}
              </div>
              {!query.trim() && recentIds.length > 0 && (
                <button
                  onClick={() => {
                    setRecentIds([]);
                    instanceStorage.removeItem("zone-recent-commands");
                  }}
                  className="w-full px-4 py-1 text-[10px] text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/30 transition-colors text-left"
                >
                  Clear recent commands
                </button>
              )}
            </>
          )}
        </div>

        {/* Footer */}
        <div className="px-4 py-1.5 border-t border-[#2a2d3d] flex items-center gap-3 text-[9px] text-[#565f89]">
          <span>
            <kbd className="font-mono bg-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6]">↑↓</kbd>{" "}
            navigate
          </span>
          <span>
            <kbd className="font-mono bg-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6]">Enter</kbd>{" "}
            execute
          </span>
          <span className="ml-auto">
            {filtered.length} command{filtered.length !== 1 ? "s" : ""}
          </span>
        </div>
      </div>
    </div>
  );
}
