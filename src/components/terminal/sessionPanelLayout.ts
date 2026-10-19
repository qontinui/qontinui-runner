/**
 * The Session Manager panel's width and rail mode, persisted per window (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3 — Chrome's resizable vertical tab strip and its collapse-to-icons).
 *
 * The panel was a fixed 340px. Its width is now dragged from its right edge
 * and clamped to {@link PANEL_MIN_WIDTH}..{@link PANEL_MAX_WIDTH}; rail mode
 * shrinks it to {@link RAIL_WIDTH} — one status dot plus the group stripe per
 * session. Both are stored per window, so a pop-out keeps its own.
 */

import { useCallback, useEffect, useState } from "react";

import { instanceStorage } from "@/lib/instance-storage";

export const PANEL_DEFAULT_WIDTH = 340;
export const PANEL_MIN_WIDTH = 240;
export const PANEL_MAX_WIDTH = 720;
export const RAIL_WIDTH = 44;

const STORAGE_BASE = "qontinui-session-panel-layout";

export interface SessionPanelLayout {
  width: number;
  rail: boolean;
}

export const DEFAULT_PANEL_LAYOUT: SessionPanelLayout = { width: PANEL_DEFAULT_WIDTH, rail: false };

/** The storage key for one window's layout. */
export function sessionPanelLayoutKey(windowLabel: string = "main"): string {
  return windowLabel === "main" ? STORAGE_BASE : `${STORAGE_BASE}:window:${windowLabel}`;
}

export function clampPanelWidth(width: number): number {
  if (!Number.isFinite(width)) return PANEL_DEFAULT_WIDTH;
  return Math.round(Math.min(PANEL_MAX_WIDTH, Math.max(PANEL_MIN_WIDTH, width)));
}

/** A stored layout, or the default for anything unreadable. */
export function parsePanelLayout(raw: unknown): SessionPanelLayout {
  if (raw === null || typeof raw !== "object") return DEFAULT_PANEL_LAYOUT;
  const r = raw as { width?: unknown; rail?: unknown };
  return {
    width: typeof r.width === "number" ? clampPanelWidth(r.width) : PANEL_DEFAULT_WIDTH,
    rail: r.rail === true,
  };
}

export function useSessionPanelLayout(windowLabel: string = "main"): {
  layout: SessionPanelLayout;
  setWidth: (width: number) => void;
  toggleRail: () => void;
} {
  const key = sessionPanelLayoutKey(windowLabel);
  const [layout, setLayout] = useState<SessionPanelLayout>(() =>
    parsePanelLayout(instanceStorage.getJSON<unknown>(key, null)),
  );
  useEffect(() => {
    instanceStorage.setJSON(key, layout);
  }, [key, layout]);
  const setWidth = useCallback((width: number) => {
    setLayout((prev) => ({ ...prev, width: clampPanelWidth(width) }));
  }, []);
  const toggleRail = useCallback(() => {
    setLayout((prev) => ({ ...prev, rail: !prev.rail }));
  }, []);
  return { layout, setWidth, toggleRail };
}
