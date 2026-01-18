/**
 * useVerificationData Hook
 *
 * Fetches and manages verification result data for the widget.
 * Currently returns mock/placeholder data - will integrate with verification API later.
 */

import { useState, useEffect, useCallback } from "react";
import type { VerificationData } from "./types";
import { DEFAULT_VERIFICATION_DATA } from "./types";

const API_BASE = "http://localhost:9876";

/**
 * Response shape from the verification log API.
 */
interface VerificationLogEntry {
  timestamp: string;
  type: "start" | "progress" | "result" | "evidence";
  data: {
    status?: string;
    test_type?: string;
    test_name?: string;
    description?: string;
    duration_ms?: number;
    error?: string;
    evidence_type?: string;
    evidence_path?: string;
    evidence_content?: string;
  };
}

interface VerificationLogResponse {
  entries: VerificationLogEntry[];
  is_running: boolean;
  status: string;
  duration_ms: number;
}

/**
 * Parse log entries into VerificationData structure.
 */
function parseLogEntries(response: VerificationLogResponse): VerificationData {
  const { entries, is_running, status, duration_ms } = response;

  const result: VerificationData = {
    ...DEFAULT_VERIFICATION_DATA,
    status: is_running ? "running" : (status as VerificationData["status"]) || "pending",
    durationMs: duration_ms,
  };

  for (const entry of entries) {
    switch (entry.type) {
      case "start":
        if (entry.data.test_type) {
          result.testType = entry.data.test_type as VerificationData["testType"];
        }
        if (entry.data.test_name) {
          result.testName = entry.data.test_name;
        }
        if (entry.data.description) {
          result.description = entry.data.description;
        }
        break;

      case "result":
        if (entry.data.status) {
          result.status = entry.data.status as VerificationData["status"];
        }
        if (entry.data.error) {
          result.error = entry.data.error;
        }
        if (entry.data.duration_ms) {
          result.durationMs = entry.data.duration_ms;
        }
        break;

      case "evidence":
        result.evidence.push({
          type: entry.data.evidence_type as VerificationData["evidence"][0]["type"],
          path: entry.data.evidence_path,
          content: entry.data.evidence_content,
          timestamp: new Date(entry.timestamp).getTime(),
        });
        break;
    }
  }

  return result;
}

/**
 * Hook to fetch verification result data.
 * Polls every 2 seconds while mounted.
 *
 * Returns VerificationData directly for consistency with widget registry's useData pattern.
 */
export function useVerificationData(): VerificationData {
  const [data, setData] = useState<VerificationData>(DEFAULT_VERIFICATION_DATA);

  const fetchData = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/logs/verification`);

      if (!response.ok) {
        // If endpoint doesn't exist yet, return defaults
        if (response.status === 404) {
          setData(DEFAULT_VERIFICATION_DATA);
          return;
        }
        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
      }

      const json: VerificationLogResponse = await response.json();
      const parsed = parseLogEntries(json);
      setData(parsed);
    } catch {
      // Silently handle errors - API may not be available
      setData(DEFAULT_VERIFICATION_DATA);
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
export function useVerificationDataWithStatus(): {
  data: VerificationData;
  isLoading: boolean;
  error: string | null;
} {
  const [data, setData] = useState<VerificationData>(DEFAULT_VERIFICATION_DATA);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/logs/verification`);

      if (!response.ok) {
        if (response.status === 404) {
          setData(DEFAULT_VERIFICATION_DATA);
          setError(null);
          return;
        }
        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
      }

      const json: VerificationLogResponse = await response.json();
      const parsed = parseLogEntries(json);
      setData(parsed);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to fetch verification data");
      setData(DEFAULT_VERIFICATION_DATA);
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

export default useVerificationData;
