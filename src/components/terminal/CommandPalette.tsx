import { useState, useEffect, useRef, useMemo, useCallback } from "react";
import type { ReactNode } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { Search } from "lucide-react";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";
import type { TerminalTab } from "./useTerminalManager";

/* ── Fuzzy match scoring ─────────────────────────────────────────────── */

function fuzzyScore(text: string, query: string): { score: number; indices: number[] } | null {
  const lower = text.toLowerCase();
  const q = query.toLowerCase();

  // Exact prefix match — highest score
  if (lower.startsWith(q)) {
    return {
      score: 100 + q.length,
      indices: Array.from({ length: q.length }, (_, i) => i),
    };
  }

  // Word boundary match
  const words = lower.split(/[\s\-_:]/);
  let wordStart = 0;
  const wordBoundaryIndices: number[] = [];
  let qi = 0;
  for (const word of words) {
    if (qi < q.length && word.startsWith(q[qi])) {
      for (let wi = 0; wi < word.length && qi < q.length; wi++) {
        if (word[wi] === q[qi]) {
          wordBoundaryIndices.push(wordStart + wi);
          qi++;
        }
      }
    }
    wordStart += word.length + 1;
  }
  if (qi === q.length) {
    return { score: 70 + q.length, indices: wordBoundaryIndices };
  }

  // Sequential character match (fuzzy)
  const indices: number[] = [];
  let si = 0;
  for (let i = 0; i < lower.length && si < q.length; i++) {
    if (lower[i] === q[si]) {
      indices.push(i);
      si++;
    }
  }
  if (si === q.length) {
    const spread = indices[indices.length - 1] - indices[0];
    const compactness = Math.max(0, 50 - spread);
    return { score: 30 + compactness, indices };
  }

  return null;
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

  // Build action list
  const actions = useMemo(() => {
    const list: PaletteAction[] = [];

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
    if (needsInputCount > 0) {
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

  // Filter by query with fuzzy scoring
  const filtered = useMemo(() => {
    if (!query.trim()) {
      // Show recent commands first, then all actions
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
    }

    const q = query.toLowerCase().trim();
    const scored: { action: PaletteAction; score: number; indices: number[] }[] = [];

    for (const action of actions) {
      const labelResult = fuzzyScore(action.label, q);
      const catResult = fuzzyScore(action.category, q);
      const best =
        labelResult && catResult
          ? labelResult.score >= catResult.score
            ? labelResult
            : null
          : labelResult;

      if (best) {
        scored.push({ action, score: best.score, indices: best.indices });
      } else if (catResult) {
        scored.push({ action, score: catResult.score, indices: [] });
      }
    }

    return scored.sort((a, b) => b.score - a.score).map((s) => s.action);
  }, [actions, query, recentIds]);

  // Match indices map for highlighting (kept separate to avoid extending PaletteAction)
  const matchIndicesMap = useMemo(() => {
    const map = new Map<string, number[]>();
    if (!query.trim()) return map;

    const q = query.toLowerCase().trim();
    for (const action of actions) {
      const labelResult = fuzzyScore(action.label, q);
      if (labelResult) {
        map.set(action.id, labelResult.indices);
      }
    }
    return map;
  }, [actions, query]);

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
      <div className="bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl w-[440px] max-h-[60vh] flex flex-col overflow-hidden">
        {/* Search input */}
        <div className="flex items-center gap-2 px-4 py-3 border-b border-[#2a2d3d]">
          <Search className="w-4 h-4 text-[#565f89] shrink-0" />
          <input
            ref={inputRef}
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Type a command..."
            className="flex-1 bg-transparent text-sm text-[#c0caf5] placeholder-[#565f89] outline-none"
          />
          <kbd className="text-[9px] font-mono text-[#565f89] bg-[#2a2d3d] rounded px-1.5 py-0.5">
            Esc
          </kbd>
        </div>

        {/* Results list */}
        <div ref={listRef} className="flex-1 overflow-y-auto scrollbar-dark py-1">
          {filtered.length === 0 ? (
            <div className="px-4 py-6 text-center text-[12px] text-[#565f89]">
              No matching commands
            </div>
          ) : (
            <>
              {filtered.map((action, i) => (
                <div
                  key={action.id}
                  role="button"
                  tabIndex={0}
                  onClick={() => executeAction(action)}
                  onMouseEnter={() => setSelectedIndex(i)}
                  onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") executeAction(action); }}
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
