import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { pageKey } from "./TerminalPageContext";
import {
  INITIAL_LAYOUT_SWITCH,
  migrateZoneMetadataByTab,
  migrateZoneSetByTab,
  anchoredToSeenTabs,
  rememberVacatedZones,
  type LayoutSwitch,
} from "./zoneAutoTrim";

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
  pageId: string = "default",
  /**
   * The last layout switch or re-deal (`useZoneLayout().layoutSwitch`). An
   * `auto` one (a grow, a trim, a quick launch, a compaction — even one that
   * keeps the layout id) moved tiles the operator did not move, so labels,
   * notes and pins follow their TAB to its new zone. Any other layout change
   * takes the per-layout path below, unchanged.
   */
  layoutSwitch: LayoutSwitch = INITIAL_LAYOUT_SWITCH,
  /**
   * The page's live tab ids (`useZoneLayout`'s `tabIds`). Only a tab seen
   * live in this mount anchors metadata for an `auto` re-deal: the persisted
   * assignments name the previous run's tab ids, whose zones' labels must
   * wait on their index for the resumed session rather than be dropped.
   */
  tabIds: readonly string[] = [],
): UseZoneLabelsAndTagsReturn {
  const pk = (key: string) => pageKey(pageId, key);
  // ── State ─────────────────────────────────────────────────────────────────

  const [zoneLabels, setZoneLabels] = useState<Record<number, string>>(() =>
    instanceStorage.getJSON<Record<number, string>>(pk(`zone-labels-${layoutId}`), {}),
  );

  const [zoneNotes, setZoneNotes] = useState<Record<number, string>>(() =>
    instanceStorage.getJSON<Record<number, string>>(pk(`zone-notes-${layoutId}`), {}),
  );

  const [pinnedZones, setPinnedZones] = useState<Set<number>>(() => {
    const stored = instanceStorage.getJSON<number[]>(pk(`zone-pinned-${layoutId}`), []);
    return new Set(stored);
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
    instanceStorage.setJSON(pk(`zone-labels-${layoutId}`), zoneLabels);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [zoneLabels, layoutId]);

  useEffect(() => {
    instanceStorage.setJSON(pk(`zone-notes-${layoutId}`), zoneNotes);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [zoneNotes, layoutId]);

  useEffect(() => {
    instanceStorage.setJSON(pk(`zone-pinned-${layoutId}`), [...pinnedZones]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pinnedZones, layoutId]);

  // ── Follow the tabs on an auto re-deal; reload on any other layout change ─
  // An `auto` re-deal (keyed on `layoutSwitch.seq`, so a compaction that keeps
  // the layout id counts) carries labels, notes and pins by tab: old zone →
  // tab → new zone (`migrateZoneMetadataByTab`). It migrates FROM the last
  // assignments plus each vacated zone's departed tab (`rememberVacatedZones`),
  // so a close and its held trim landing in different commits still drop the
  // closed tab's label instead of leaving it on the index. Only tabs seen
  // live in this mount anchor anything (`seenLiveRef`): a previous run's tab
  // id from the persisted assignments never does.
  //
  // Any other layout change (operator, boot restore, tmux enter/exit) reloads
  // the target layout's stored sets and migrates pins by tab on top. In
  // practice the persist effects above run first on that same commit and
  // write the OUTGOING labels, notes and pins under the new layout's key, so
  // the "reload" reads them straight back: labels and notes stay on their
  // zone indices, and pins keep their indices plus the tab-migrated ones.

  const prevLayoutIdRef2 = useRef(layoutId);
  const prevSwitchSeqRef = useRef(layoutSwitch.seq);
  const migrateFromRef = useRef<Record<number, string>>(assignments);
  // Every tab id live at any point in this mount. Declared before the effect
  // below so it is current when that effect runs in the same commit.
  const seenLiveRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    for (const id of tabIds) seenLiveRef.current.add(id);
  }, [tabIds]);

  useEffect(() => {
    const switched = prevSwitchSeqRef.current !== layoutSwitch.seq;
    prevSwitchSeqRef.current = layoutSwitch.seq;
    if (switched && layoutSwitch.origin === "auto") {
      const from = anchoredToSeenTabs(migrateFromRef.current, seenLiveRef.current);
      prevLayoutIdRef2.current = layoutId;
      migrateFromRef.current = assignments;
      setZoneLabels((prev) => migrateZoneMetadataByTab(prev, from, assignments));
      setZoneNotes((prev) => migrateZoneMetadataByTab(prev, from, assignments));
      setPinnedZones((prev) => migrateZoneSetByTab(prev, from, assignments));
    } else if (prevLayoutIdRef2.current !== layoutId) {
      // Capture which TAB IDs were pinned using the previous layout's assignments
      const pinnedTabIds = new Set<string>();
      for (const zoneIdx of pinnedZones) {
        const tabId = migrateFromRef.current[zoneIdx];
        if (tabId) pinnedTabIds.add(tabId);
      }

      prevLayoutIdRef2.current = layoutId;
      migrateFromRef.current = assignments;

      setZoneLabels(
        instanceStorage.getJSON<Record<number, string>>(pk(`zone-labels-${layoutId}`), {}),
      );

      // Restore pins: merge stored pins with migrated tab-based pins
      let newPins = new Set<number>(
        instanceStorage.getJSON<number[]>(pk(`zone-pinned-${layoutId}`), []),
      );
      // Add pins for tabs that were pinned in the previous layout
      for (const [zoneStr, tabId] of Object.entries(assignments)) {
        if (pinnedTabIds.has(tabId)) {
          newPins.add(Number(zoneStr));
        }
      }
      setPinnedZones(newPins);

      setZoneNotes(
        instanceStorage.getJSON<Record<number, string>>(pk(`zone-notes-${layoutId}`), {}),
      );
    } else {
      // No layout change (a swap, a close, a fill): track the assignments,
      // remembering each vacated zone's departed tab for the next re-deal.
      migrateFromRef.current = rememberVacatedZones(
        migrateFromRef.current,
        assignments,
        seenLiveRef.current,
      );
    }
  }, [layoutId, assignments, layoutSwitch.seq]); // eslint-disable-line react-hooks/exhaustive-deps

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
