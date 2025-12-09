/**
 * useExtractionState
 *
 * Hook for persisting extraction tab state across tab switches.
 * Uses localStorage for persistence across sessions.
 */

import { useState, useEffect, useCallback } from "react";

const STORAGE_KEY = "qontinui-extraction-state";

interface ExtractionState {
  urls: string[];
  selectedMonitor: number;
  captureHoverStates: boolean;
  captureFocusStates: boolean;
  captureScrollStates: boolean;
  maxDepth: number;
  maxPages: number;
  showAdvanced: boolean;
}

const defaultState: ExtractionState = {
  urls: [],
  selectedMonitor: 0,
  captureHoverStates: true,
  captureFocusStates: true,
  captureScrollStates: true,
  maxDepth: 5,
  maxPages: 100,
  showAdvanced: false,
};

export function useExtractionState() {
  // Load initial state from localStorage
  const [state, setState] = useState<ExtractionState>(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY);
      if (stored) {
        const parsed = JSON.parse(stored);
        return { ...defaultState, ...parsed };
      }
    } catch (e) {
      console.error("[EXTRACTION_STATE] Failed to load state:", e);
    }
    return defaultState;
  });

  // Persist state to localStorage whenever it changes
  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
    } catch (e) {
      console.error("[EXTRACTION_STATE] Failed to save state:", e);
    }
  }, [state]);

  // Individual setters
  const setUrls = useCallback((urls: string[]) => {
    setState((prev) => ({ ...prev, urls }));
  }, []);

  const addUrl = useCallback((url: string) => {
    setState((prev) => ({ ...prev, urls: [...prev.urls, url] }));
  }, []);

  const removeUrl = useCallback((index: number) => {
    setState((prev) => ({
      ...prev,
      urls: prev.urls.filter((_, i) => i !== index),
    }));
  }, []);

  const setSelectedMonitor = useCallback((selectedMonitor: number) => {
    setState((prev) => ({ ...prev, selectedMonitor }));
  }, []);

  const setCaptureHoverStates = useCallback((captureHoverStates: boolean) => {
    setState((prev) => ({ ...prev, captureHoverStates }));
  }, []);

  const setCaptureFocusStates = useCallback((captureFocusStates: boolean) => {
    setState((prev) => ({ ...prev, captureFocusStates }));
  }, []);

  const setCaptureScrollStates = useCallback((captureScrollStates: boolean) => {
    setState((prev) => ({ ...prev, captureScrollStates }));
  }, []);

  const setMaxDepth = useCallback((maxDepth: number) => {
    setState((prev) => ({ ...prev, maxDepth }));
  }, []);

  const setMaxPages = useCallback((maxPages: number) => {
    setState((prev) => ({ ...prev, maxPages }));
  }, []);

  const setShowAdvanced = useCallback((showAdvanced: boolean) => {
    setState((prev) => ({ ...prev, showAdvanced }));
  }, []);

  const resetState = useCallback(() => {
    setState(defaultState);
  }, []);

  return {
    // State values
    urls: state.urls,
    selectedMonitor: state.selectedMonitor,
    captureHoverStates: state.captureHoverStates,
    captureFocusStates: state.captureFocusStates,
    captureScrollStates: state.captureScrollStates,
    maxDepth: state.maxDepth,
    maxPages: state.maxPages,
    showAdvanced: state.showAdvanced,

    // Setters
    setUrls,
    addUrl,
    removeUrl,
    setSelectedMonitor,
    setCaptureHoverStates,
    setCaptureFocusStates,
    setCaptureScrollStates,
    setMaxDepth,
    setMaxPages,
    setShowAdvanced,
    resetState,
  };
}
