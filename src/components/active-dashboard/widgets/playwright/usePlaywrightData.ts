/**
 * usePlaywrightData Hook
 *
 * Fetches and manages Playwright test execution data for the widget.
 * Polls the runner API every 2 seconds for updates.
 */

import { useState, useEffect, useCallback } from "react";
import type { PlaywrightData, TestSpec, TestStatus } from "./types";
import { DEFAULT_PLAYWRIGHT_DATA } from "./types";
import { getApiBase } from "@/lib/runner-api";

/**
 * Response shape from the playwright log API.
 */
interface PlaywrightLogEntry {
  timestamp: string;
  type: "test_start" | "test_end" | "spec_result" | "console" | "summary";
  data: {
    test?: string;
    status?: string;
    duration_ms?: number;
    error?: string;
    message?: string;
    passed?: number;
    failed?: number;
    skipped?: number;
    total?: number;
    specs?: Array<{
      title: string;
      status: string;
      duration_ms: number;
      error?: string;
    }>;
  };
}

interface PlaywrightLogResponse {
  entries: PlaywrightLogEntry[];
  is_running: boolean;
  total_duration_ms: number;
}

/**
 * Parse log entries into PlaywrightData structure.
 */
function parseLogEntries(response: PlaywrightLogResponse): PlaywrightData {
  const { entries, is_running, total_duration_ms } = response;

  const tests: TestSpec[] = [];
  const consoleOutput: string[] = [];
  let currentTest: string | null = null;
  let passedTests = 0;
  let failedTests = 0;
  let skippedTests = 0;

  for (const entry of entries) {
    switch (entry.type) {
      case "test_start":
        if (entry.data.test) {
          currentTest = entry.data.test;
        }
        break;

      case "test_end":
        currentTest = null;
        break;

      case "spec_result":
        if (entry.data.specs) {
          for (const spec of entry.data.specs) {
            const status = spec.status.toLowerCase() as TestStatus;
            tests.push({
              title: spec.title,
              status,
              durationMs: spec.duration_ms || 0,
              error: spec.error,
            });

            if (status === "passed") passedTests++;
            else if (status === "failed") failedTests++;
            else if (status === "skipped") skippedTests++;
          }
        }
        break;

      case "console":
        if (entry.data.message) {
          consoleOutput.push(entry.data.message);
        }
        break;

      case "summary":
        // Update counts from summary if available
        if (entry.data.passed !== undefined) passedTests = entry.data.passed;
        if (entry.data.failed !== undefined) failedTests = entry.data.failed;
        if (entry.data.skipped !== undefined) skippedTests = entry.data.skipped;
        break;
    }
  }

  // If still running, mark the current test
  if (is_running && currentTest) {
    const runningTest = tests.find((t) => t.title === currentTest);
    if (runningTest) {
      runningTest.status = "running";
    }
  }

  return {
    tests,
    currentTest: is_running ? currentTest : null,
    totalTests: tests.length || passedTests + failedTests + skippedTests,
    passedTests,
    failedTests,
    skippedTests,
    consoleOutput,
    screenshots: [], // TODO: Fetch from screenshots API
    isRunning: is_running,
    durationMs: total_duration_ms,
  };
}

/**
 * Hook to fetch Playwright test execution data.
 * Polls every 2 seconds while mounted.
 *
 * Returns PlaywrightData directly (with isLoading/error embedded) for consistency
 * with widget registry's useData pattern.
 */
export function usePlaywrightData(): PlaywrightData {
  const [data, setData] = useState<PlaywrightData>(DEFAULT_PLAYWRIGHT_DATA);

  const fetchData = useCallback(async () => {
    try {
      const response = await fetch(`${getApiBase()}/logs/playwright`);

      if (!response.ok) {
        // If endpoint doesn't exist yet, return defaults
        if (response.status === 404) {
          setData(DEFAULT_PLAYWRIGHT_DATA);
          return;
        }
        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
      }

      const json: PlaywrightLogResponse = await response.json();
      const parsed = parseLogEntries(json);
      setData(parsed);
    } catch {
      // Silently handle errors - API may not be available
      setData(DEFAULT_PLAYWRIGHT_DATA);
    }
  }, []);

  // Initial fetch and polling
  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 2000);
    return () => clearInterval(interval);
  }, [fetchData]);

  return data;
}

/**
 * Hook with full result pattern (for components that need loading/error state).
 */
export function usePlaywrightDataWithStatus(): {
  data: PlaywrightData;
  isLoading: boolean;
  error: string | null;
} {
  const [data, setData] = useState<PlaywrightData>(DEFAULT_PLAYWRIGHT_DATA);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    try {
      const response = await fetch(`${getApiBase()}/logs/playwright`);

      if (!response.ok) {
        if (response.status === 404) {
          setData(DEFAULT_PLAYWRIGHT_DATA);
          setError(null);
          return;
        }
        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
      }

      const json: PlaywrightLogResponse = await response.json();
      const parsed = parseLogEntries(json);
      setData(parsed);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to fetch Playwright data");
      setData(DEFAULT_PLAYWRIGHT_DATA);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 2000);
    return () => clearInterval(interval);
  }, [fetchData]);

  return { data, isLoading, error };
}

export default usePlaywrightData;
