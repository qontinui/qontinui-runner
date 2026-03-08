import { useState, useEffect, useCallback, useMemo, useRef } from "react";

const GROUP_COLORS = ["#bb9af7", "#7aa2f7", "#9ece6a", "#e0af68", "#f7768e", "#7dcfff"];

export interface UseZoneLabelsAndTagsReturn {
  zoneLabels: Record<number, string>;
  setZoneLabel: (zoneIdx: number, label: string) => void;
  setZoneLabels: React.Dispatch<React.SetStateAction<Record<number, string>>>;
  zoneNotes: Record<number, string>;
  setZoneNote: (zoneIdx: number, note: string) => void;
  setZoneNotes: React.Dispatch<React.SetStateAction<Record<number, string>>>;
  pinnedZones: Set<number>;
  togglePin: (zoneIdx: number) => void;
  setPinnedZones: React.Dispatch<React.SetStateAction<Set<number>>>;
  activeTagFilters: Set<string>;
  setActiveTagFilters: React.Dispatch<React.SetStateAction<Set<string>>>;
  labelColorMap: Record<string, string>;
  allTags: string[];
  zoneTags: Record<number, string[]>;
}

export function useZoneLabelsAndTags(
  layoutId: string,
  assignments: Record<number, string>,
): UseZoneLabelsAndTagsReturn {
  // ── State ─────────────────────────────────────────────────────────────────

  const [zoneLabels, setZoneLabels] = useState<Record<number, string>>(() => {
    try {
      const stored = localStorage.getItem(`zone-labels-${layoutId}`);
      if (stored) return JSON.parse(stored) as Record<number, string>;
    } catch {
      // intentionally empty
    }
    return {};
  });

  const [zoneNotes, setZoneNotes] = useState<Record<number, string>>(() => {
    try {
      const stored = localStorage.getItem(`zone-notes-${layoutId}`);
      if (stored) return JSON.parse(stored) as Record<number, string>;
    } catch {
      // intentionally empty
    }
    return {};
  });

  const [pinnedZones, setPinnedZones] = useState<Set<number>>(() => {
    try {
      const stored = localStorage.getItem(`zone-pinned-${layoutId}`);
      if (stored) return new Set(JSON.parse(stored) as number[]);
    } catch {
      // intentionally empty
    }
    return new Set();
  });

  const [activeTagFilters, setActiveTagFilters] = useState<Set<string>>(new Set());

  // ── Callbacks ─────────────────────────────────────────────────────────────

  const setZoneLabel = useCallback((zoneIdx: number, label: string) => {
    setZoneLabels((prev) => {
      if (!label) {
        const next = { ...prev };
        delete next[zoneIdx];
        return next;
      }
      return { ...prev, [zoneIdx]: label };
    });
  }, []);

  const setZoneNote = useCallback((zoneIdx: number, note: string) => {
    setZoneNotes((prev) => {
      if (!note) {
        const next = { ...prev };
        delete next[zoneIdx];
        return next;
      }
      return { ...prev, [zoneIdx]: note };
    });
  }, []);

  const togglePin = useCallback((zoneIdx: number) => {
    setPinnedZones((prev) => {
      const next = new Set(prev);
      if (next.has(zoneIdx)) next.delete(zoneIdx);
      else next.add(zoneIdx);
      return next;
    });
  }, []);

  // ── Persist to localStorage ───────────────────────────────────────────────

  useEffect(() => {
    localStorage.setItem(`zone-labels-${layoutId}`, JSON.stringify(zoneLabels));
  }, [zoneLabels, layoutId]);

  useEffect(() => {
    localStorage.setItem(`zone-notes-${layoutId}`, JSON.stringify(zoneNotes));
  }, [zoneNotes, layoutId]);

  useEffect(() => {
    localStorage.setItem(`zone-pinned-${layoutId}`, JSON.stringify([...pinnedZones]));
  }, [pinnedZones, layoutId]);

  // ── Reload labels/pins when layout changes, with pin migration ────────────

  const prevLayoutIdRef2 = useRef(layoutId);
  const prevAssignmentsRef = useRef<Record<number, string>>(assignments);

  useEffect(() => {
    if (prevLayoutIdRef2.current !== layoutId) {
      // Capture which TAB IDs were pinned using the previous layout's assignments
      const pinnedTabIds = new Set<string>();
      for (const zoneIdx of pinnedZones) {
        const tabId = prevAssignmentsRef.current[zoneIdx];
        if (tabId) pinnedTabIds.add(tabId);
      }

      prevLayoutIdRef2.current = layoutId;
      prevAssignmentsRef.current = assignments;

      try {
        const storedLabels = localStorage.getItem(`zone-labels-${layoutId}`);
        setZoneLabels(storedLabels ? JSON.parse(storedLabels) : {});
      } catch {
        setZoneLabels({});
      }

      // Restore pins: merge stored pins with migrated tab-based pins
      let newPins = new Set<number>();
      try {
        const storedPins = localStorage.getItem(`zone-pinned-${layoutId}`);
        if (storedPins) newPins = new Set(JSON.parse(storedPins) as number[]);
      } catch {
        // intentionally empty
      }
      // Add pins for tabs that were pinned in the previous layout
      for (const [zoneStr, tabId] of Object.entries(assignments)) {
        if (pinnedTabIds.has(tabId)) {
          newPins.add(Number(zoneStr));
        }
      }
      setPinnedZones(newPins);

      try {
        const storedNotes = localStorage.getItem(`zone-notes-${layoutId}`);
        setZoneNotes(storedNotes ? JSON.parse(storedNotes) : {});
      } catch {
        setZoneNotes({});
      }
    } else {
      // Keep assignments ref in sync for non-layout-change updates (e.g., tab swaps)
      prevAssignmentsRef.current = assignments;
    }
  }, [layoutId, assignments]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Memos ─────────────────────────────────────────────────────────────────

  const labelColorMap = useMemo(() => {
    const map: Record<string, string> = {};
    const tagSet = new Set<string>();
    for (const label of Object.values(zoneLabels)) {
      if (!label) continue;
      for (const tag of label
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean)) {
        tagSet.add(tag);
      }
    }
    const uniqueTags = [...tagSet].sort();
    for (let i = 0; i < uniqueTags.length; i++) {
      map[uniqueTags[i]] = GROUP_COLORS[i % GROUP_COLORS.length];
    }
    return map;
  }, [zoneLabels]);

  const allTags = useMemo(() => {
    const tagSet = new Set<string>();
    for (const label of Object.values(zoneLabels)) {
      if (!label) continue;
      for (const tag of label
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean)) {
        tagSet.add(tag);
      }
    }
    return [...tagSet].sort();
  }, [zoneLabels]);

  const zoneTags = useMemo(
    () =>
      Object.fromEntries(
        Object.entries(zoneLabels).map(([z, label]) => [
          Number(z),
          label
            ? label
                .split(",")
                .map((t) => t.trim())
                .filter(Boolean)
            : [],
        ]),
      ) as Record<number, string[]>,
    [zoneLabels],
  );

  return {
    zoneLabels,
    setZoneLabel,
    setZoneLabels,
    zoneNotes,
    setZoneNote,
    setZoneNotes,
    pinnedZones,
    togglePin,
    setPinnedZones,
    activeTagFilters,
    setActiveTagFilters,
    labelColorMap,
    allTags,
    zoneTags,
  };
}
