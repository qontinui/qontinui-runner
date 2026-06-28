/**
 * useSpecAuthor — Hook for saving specs via the Spec API
 *
 * Handles POSTing spec changes to /apps/:app_id/spec/author and manages
 * loading/error states. Automatically invalidates the spec cache on success
 * via SSE-driven cache invalidation (existing in use-discovered-specs).
 */

import { useState, useCallback } from "react";
import { getApiBase } from "@/lib/runner-api";

interface AuthorError {
  error: string;
  violations?: Array<{ type: string; element_id?: string }>;
  detail?: Record<string, unknown>;
}

export function useSpecAuthor(appId: string) {
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const saveSpec = useCallback(
    async (spec: any): Promise<void> => {
      setIsLoading(true);
      setError(null);

      try {
        const response = await fetch(
          `${getApiBase()}/apps/${appId}/spec/author`,
          {
            method: "POST",
            headers: {
              "Content-Type": "application/json",
            },
            body: JSON.stringify(spec),
          }
        );

        if (!response.ok) {
          const body = (await response.json()) as AuthorError;
          let errorMsg = body.error || `HTTP ${response.status}`;

          // Format validation errors
          if (body.violations && body.violations.length > 0) {
            const violationList = body.violations
              .map((v) => `${v.type}${v.element_id ? ` (${v.element_id})` : ""}`)
              .join(", ");
            errorMsg = `Validation failed: ${violationList}`;
          }

          throw new Error(errorMsg);
        }

        const result = await response.json();
        if (!result.ok) {
          throw new Error(result.reason || "Failed to save spec");
        }

        // Success — cache will auto-invalidate via SSE event
      } catch (err) {
        const msg =
          err instanceof Error
            ? err.message
            : "Unknown error saving spec";
        setError(msg);
        throw err;
      } finally {
        setIsLoading(false);
      }
    },
    [appId]
  );

  return {
    saveSpec,
    isLoading,
    error,
  };
}
