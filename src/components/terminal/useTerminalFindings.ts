/**
 * useTerminalFindings
 *
 * Intercepts raw PTY output, strips ANSI escape sequences, buffers partial
 * lines, and feeds complete lines through FindingsTracker. Returns findings
 * scoped to the currently active terminal tab.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { findingsTracker } from "@/services/FindingsTracker";
import type { Finding } from "@/types/findings";

/** Matches all common ANSI escape sequences (CSI, OSC, single-char). */
// eslint-disable-next-line no-control-regex
const ANSI_RE = /\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\].*?(?:\x07|\x1B\\))/g;

export function useTerminalFindings(activeTerminalId: string | null) {
  const lineBuffers = useRef(new Map<string, string>());
  const [activeFindings, setActiveFindings] = useState<Finding[]>([]);
  const [allFindings, setAllFindings] = useState<Finding[]>([]);

  /** Process a chunk of raw PTY output for a given terminal tab. */
  const processOutput = useCallback((terminalId: string, text: string) => {
    const clean = text.replace(ANSI_RE, "");
    const buf = (lineBuffers.current.get(terminalId) ?? "") + clean;
    const lines = buf.split("\n");
    // Keep the incomplete last segment as the pending buffer
    lineBuffers.current.set(terminalId, lines.pop() ?? "");

    for (const line of lines) {
      if (line.trim()) {
        const finding = findingsTracker.processLine(line);
        if (finding) {
          finding.sourceSessionId = terminalId;
          // Auto-resolve: if this finding has a resolution, resolve matching needs_input finding
          if (finding.resolution && finding.status !== "needs_input") {
            findingsTracker.resolveMatchingFinding(finding.categoryId, finding.title);
          }
        }
      }
    }
  }, []);

  /** Subscribe to tracker events and filter findings by active tab. */
  useEffect(() => {
    const update = () => {
      const all = findingsTracker.getFindingsNeedingInput();
      setAllFindings(all);
      if (!activeTerminalId) {
        setActiveFindings([]);
        return;
      }
      setActiveFindings(all.filter((f) => f.sourceSessionId === activeTerminalId));
    };

    const unsub = findingsTracker.subscribe((event) => {
      if (
        [
          "finding_detected",
          "finding_updated",
          "finding_resolved",
          "finding_removed",
          "findings_cleared",
        ].includes(event.type)
      ) {
        update();
      }
    });

    update();
    return unsub;
  }, [activeTerminalId]);

  return { processOutput, activeFindings, allFindings };
}
