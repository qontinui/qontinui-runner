/**
 * useElements — Element management sub-hook.
 *
 * Manages element state, selection, fetching, and fingerprinting.
 * Coordinates with capture session for auto-capture on element refresh.
 */

import { useState, useCallback } from "react";
import type { ExternalElement, CaptureRecord, CaptureSessionStatus, SdkAppInfo, CaptureSessionRef } from "./types";
import { mapSdkElement } from "./utils";
import {
  generateFingerprints,
  extractFingerprintHashes,
} from "../../lib/ui-bridge/fingerprintGenerator";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

export interface UseElementsReturn {
  elements: ExternalElement[];
  selectedElementId: string | null;
  selectedElement: ExternalElement | null;
  isLoadingElements: boolean;
  fetchElements: () => Promise<void>;
  refreshElements: () => Promise<void>;
  selectElement: (id: string | null) => void;
  setElements: React.Dispatch<React.SetStateAction<ExternalElement[]>>;
  setSelectedElementId: React.Dispatch<React.SetStateAction<string | null>>;
}

export function useElements(
  connectedAppRef: React.MutableRefObject<SdkAppInfo | null>,
  captureSessionRef: React.MutableRefObject<CaptureSessionRef | null>,
  setCaptureSession: React.Dispatch<React.SetStateAction<CaptureSessionStatus>>,
): UseElementsReturn {
  const [elements, setElements] = useState<ExternalElement[]>([]);
  const [selectedElementId, setSelectedElementId] = useState<string | null>(null);
  const [isLoadingElements, setIsLoadingElements] = useState(false);

  // Derived: selected element
  const selectedElement = selectedElementId
    ? (elements.find((e) => e.id === selectedElementId) ?? null)
    : null;

  const fetchElements = useCallback(async () => {
    setIsLoadingElements(true);
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/elements`);
      const json = await resp.json();

      if (json.success === false) {
        return;
      }

      // The response might have elements at different locations depending on SDK
      const rawElements: unknown[] = json.data?.elements || json.elements || json.data || [];

      const mapped = (rawElements as Record<string, unknown>[]).map(mapSdkElement);

      // Generate fingerprints for all elements
      const { catalog } = generateFingerprints(mapped);

      setElements(mapped);

      // Auto-capture if session is active
      const session = captureSessionRef.current;
      const connectedApp = connectedAppRef.current;
      if (session) {
        const hashes = extractFingerprintHashes(mapped);
        const captureId = `cap-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;

        const capture: CaptureRecord = {
          captureId,
          timestamp: Date.now(),
          url: connectedApp?.port ? `http://localhost:${connectedApp.port}` : "unknown",
          title: connectedApp?.appName || "SDK App",
          elementFingerprints: hashes,
          elementCount: mapped.length,
        };

        session.captures.push(capture);
        session.lastCaptureId = captureId;

        // Merge new fingerprints into catalog
        Object.assign(session.fingerprintCatalog, catalog);

        setCaptureSession({
          active: true,
          sessionId: session.sessionId,
          startedAt: session.startedAt,
          captureCount: session.captures.length,
          uniqueFingerprints: Object.keys(session.fingerprintCatalog).length,
        });
      }
    } catch {
      // Error handled silently — caller can check elements state
    } finally {
      setIsLoadingElements(false);
    }
  }, [connectedAppRef, captureSessionRef, setCaptureSession]);

  const refreshElements = useCallback(async () => {
    await fetchElements();
  }, [fetchElements]);

  const selectElement = useCallback((id: string | null) => {
    setSelectedElementId(id);
  }, []);

  return {
    elements,
    selectedElementId,
    selectedElement,
    isLoadingElements,
    fetchElements,
    refreshElements,
    selectElement,
    setElements,
    setSelectedElementId,
  };
}
