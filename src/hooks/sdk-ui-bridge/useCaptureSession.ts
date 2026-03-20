/**
 * useCaptureSession — Capture sessions and fingerprinting sub-hook.
 *
 * Manages capture session lifecycle, co-occurrence data generation,
 * and module-level screenshot persistence.
 */

import { useState, useCallback, useRef } from "react";
import type {
  CaptureSessionStatus,
  CooccurrenceExport,
  CaptureSessionRef,
} from "./types";
// extractFingerprintHashes is used by useElements (auto-capture) and useStateExploration

/**
 * Module-level storage for capture screenshots that survives React route changes.
 * During self-connect exploration, the runner navigates away from the States page
 * which unmounts all React components. localStorage can't hold screenshots (too large).
 * This module variable persists across route changes since it's in the module scope.
 */
let pendingCaptureScreenshots: CooccurrenceExport["captureScreenshots"] | undefined;

/** Retrieve and consume pending capture screenshots saved during exploration. */
export function consumePendingCaptureScreenshots(): CooccurrenceExport["captureScreenshots"] | undefined {
  const data = pendingCaptureScreenshots;
  pendingCaptureScreenshots = undefined;
  return data;
}

/** Set pending capture screenshots (used by exploration sub-hook). */
export function setPendingCaptureScreenshots(
  data: CooccurrenceExport["captureScreenshots"] | undefined,
): void {
  pendingCaptureScreenshots = data;
}

export interface UseCaptureSessionReturn {
  captureSession: CaptureSessionStatus;
  setCaptureSession: React.Dispatch<React.SetStateAction<CaptureSessionStatus>>;
  captureSessionRef: React.MutableRefObject<CaptureSessionRef | null>;
  cooccurrenceData: CooccurrenceExport | null;
  isLoadingCooccurrence: boolean;
  startCaptureSession: () => void;
  stopCaptureSession: () => void;
  generateCooccurrenceExport: () => Promise<CooccurrenceExport | null>;
}

export function useCaptureSession(
  fetchElements: () => Promise<void>,
): UseCaptureSessionReturn {
  const [captureSession, setCaptureSession] = useState<CaptureSessionStatus>({ active: false });
  const captureSessionRef = useRef<CaptureSessionRef | null>(null);
  const [cooccurrenceData, setCooccurrenceData] = useState<CooccurrenceExport | null>(null);
  const [isLoadingCooccurrence, setIsLoadingCooccurrence] = useState(false);

  const startCaptureSession = useCallback(() => {
    const sessionId = `sdk-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;

    captureSessionRef.current = {
      sessionId,
      startedAt: Date.now(),
      captures: [],
      fingerprintCatalog: {},
      lastCaptureId: null,
    };

    setCaptureSession({
      active: true,
      sessionId,
      startedAt: Date.now(),
      captureCount: 0,
      uniqueFingerprints: 0,
    });
    setCooccurrenceData(null);

    // Immediately capture current state
    fetchElements();

    console.log(`[useCaptureSession] Capture session started: ${sessionId}`);
  }, [fetchElements]);

  const stopCaptureSession = useCallback(() => {
    captureSessionRef.current = null;
    setCaptureSession({ active: false });
    console.log("[useCaptureSession] Capture session stopped");
  }, []);

  const generateCooccurrenceExport = useCallback(async (): Promise<CooccurrenceExport | null> => {
    const session = captureSessionRef.current;
    if (!session || session.captures.length === 0) {
      console.warn("[useCaptureSession] No capture data to export");
      return null;
    }

    setIsLoadingCooccurrence(true);

    try {
      const { sessionId, captures, fingerprintCatalog } = session;

      // Collect all unique fingerprints
      const allFingerprintsSet = new Set<string>();
      for (const cap of captures) {
        for (const fp of cap.elementFingerprints) {
          allFingerprintsSet.add(fp);
        }
      }
      const allFingerprints = Array.from(allFingerprintsSet).sort();

      // Build presence matrix
      const presenceMatrix = captures.map((cap, index) => ({
        captureId: cap.captureId,
        captureIndex: index,
        timestamp: cap.timestamp,
        url: cap.url,
        title: cap.title,
        fingerprints: [...cap.elementFingerprints].sort(),
      }));

      // Build co-occurrence counts
      const cooccurrenceCounts: Record<string, Record<string, number>> = {};
      for (const capture of captures) {
        const fps = capture.elementFingerprints;
        for (let i = 0; i < fps.length; i++) {
          for (let j = i + 1; j < fps.length; j++) {
            const [fp1, fp2] = [fps[i], fps[j]].sort();
            if (!cooccurrenceCounts[fp1]) cooccurrenceCounts[fp1] = {};
            cooccurrenceCounts[fp1][fp2] = (cooccurrenceCounts[fp1][fp2] || 0) + 1;
          }
        }
      }

      // Build fingerprint statistics
      const fingerprintStats: CooccurrenceExport["fingerprintStats"] = {};
      for (const fp of allFingerprints) {
        const captureIds: string[] = [];
        let firstSeen = -1;
        let lastSeen = -1;

        captures.forEach((cap, index) => {
          if (cap.elementFingerprints.includes(fp)) {
            captureIds.push(cap.captureId);
            if (firstSeen === -1) firstSeen = index;
            lastSeen = index;
          }
        });

        fingerprintStats[fp] = {
          hash: fp,
          totalAppearances: captureIds.length,
          captureIds,
          firstSeen,
          lastSeen,
        };
      }

      // Build transitions from captures that have triggeredBy
      const transitions: CooccurrenceExport["transitions"] = [];
      for (const cap of captures) {
        if (!cap.triggeredBy) continue;

        const beforeCapture = captures.find(
          (c) => c.captureId === cap.triggeredBy!.previousCaptureId,
        );
        if (!beforeCapture) continue;

        const beforeSet = new Set(beforeCapture.elementFingerprints);
        const afterSet = new Set(cap.elementFingerprints);

        transitions.push({
          actionId: `action-${transitions.length + 1}`,
          actionType: cap.triggeredBy.actionType,
          targetFingerprint: cap.triggeredBy.targetFingerprint,
          beforeCaptureId: cap.triggeredBy.previousCaptureId,
          afterCaptureId: cap.captureId,
          appearedFingerprints: [...afterSet].filter((h) => !beforeSet.has(h)),
          disappearedFingerprints: [...beforeSet].filter((h) => !afterSet.has(h)),
          timestamp: cap.timestamp,
        });
      }

      // Find state candidates (groups of fingerprints that always appear together)
      const stateCandidates: CooccurrenceExport["stateCandidates"] = [];
      const processedGroups = new Set<string>();

      for (const fp1 of allFingerprints) {
        const stats1 = fingerprintStats[fp1];
        if (stats1.totalAppearances < 2) continue;

        const group = [fp1];

        for (const fp2 of allFingerprints) {
          if (fp1 >= fp2) continue;

          const stats2 = fingerprintStats[fp2];
          if (stats2.totalAppearances !== stats1.totalAppearances) continue;

          const [sorted1, sorted2] = [fp1, fp2].sort();
          const coCount = cooccurrenceCounts[sorted1]?.[sorted2] || 0;

          if (coCount === stats1.totalAppearances) {
            group.push(fp2);
          }
        }

        if (group.length > 1) {
          const groupKey = group.sort().join(",");
          if (!processedGroups.has(groupKey)) {
            processedGroups.add(groupKey);
            stateCandidates.push({
              fingerprints: group,
              cooccurrenceRate: 1.0,
              appearanceCount: stats1.totalAppearances,
            });
          }
        }
      }

      stateCandidates.sort((a, b) => b.appearanceCount - a.appearanceCount);

      const result: CooccurrenceExport = {
        sessionId,
        exportedAt: Date.now(),
        allFingerprints,
        fingerprintDetails: fingerprintCatalog,
        presenceMatrix,
        cooccurrenceCounts,
        fingerprintStats,
        transitions,
        stateCandidates,
        elementThumbnails: session.elementThumbnails,
        captureScreenshots: session.captureScreenshots,
      };

      console.log(
        `[useCaptureSession] Co-occurrence export: ${allFingerprints.length} fingerprints, ${captures.length} captures, ${stateCandidates.length} state candidates`,
      );

      setCooccurrenceData(result);
      return result;
    } catch (err) {
      console.error("[useCaptureSession] Failed to generate co-occurrence export:", err);
      return null;
    } finally {
      setIsLoadingCooccurrence(false);
    }
  }, []);

  return {
    captureSession,
    setCaptureSession,
    captureSessionRef,
    cooccurrenceData,
    isLoadingCooccurrence,
    startCaptureSession,
    stopCaptureSession,
    generateCooccurrenceExport,
  };
}
