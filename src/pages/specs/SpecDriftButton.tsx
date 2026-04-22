/**
 * SpecDriftButton — scans the runner source tree for `useUIElement`
 * registrations and compares them against assertions in every spec file,
 * reporting drift (missing assertions / orphan assertions).
 *
 * Read-only: does not modify spec files. Developers use the report to decide
 * where to paste the suggested assertions.
 */

import { useCallback, useState } from "react";
import { useUIElement } from "@qontinui/ui-bridge";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, CheckCircle2, Loader2, Search, X, Copy } from "lucide-react";

interface RegisteredElement {
  id: string;
  label: string | null;
  file: string;
  line: number;
  suggested_assertion: string;
}

interface SpecOrphan {
  element_id: string;
  spec_id: string;
  assertion_id: string;
  label: string | null;
}

interface DriftReport {
  scanned_at: string;
  source_files_scanned: number;
  spec_files_scanned: number;
  total_registered_elements: number;
  total_asserted_ids: number;
  missing_from_spec: RegisteredElement[];
  orphans_in_spec: SpecOrphan[];
}

export function SpecDriftButton() {
  const [scanning, setScanning] = useState(false);
  const [report, setReport] = useState<DriftReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);

  const { ref: btnRef } = useUIElement({
    id: "specs-btn-check-drift",
    type: "button",
    label: "Check spec drift",
    actions: ["click"],
  });

  const runScan = useCallback(async () => {
    setScanning(true);
    setError(null);
    try {
      // projectRoot must be the runner's source root; we use the well-known path.
      const result = await invoke<DriftReport>("scan_spec_drift", {
        projectRoot: "D:/qontinui-root/qontinui-runner",
      });
      setReport(result);
      setOpen(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setOpen(true);
    } finally {
      setScanning(false);
    }
  }, []);

  const totalIssues = report ? report.missing_from_spec.length + report.orphans_in_spec.length : 0;

  return (
    <>
      <button
        ref={btnRef}
        onClick={runScan}
        disabled={scanning}
        title={
          "Scan the runner's source tree for useUIElement(...) registrations and " +
          "compare them against every .spec.uibridge.json file. Reports two kinds of drift: " +
          "elements registered in code but not covered by any assertion, and assertions " +
          "referencing element IDs that no longer appear in source. Read-only — does not " +
          "modify spec files."
        }
        className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold rounded-md
        bg-amber-600 text-white shadow-xs shadow-amber-600/25
        hover:bg-amber-700 disabled:opacity-50 transition-colors shrink-0"
      >
        {scanning ? (
          <Loader2 className="w-3.5 h-3.5 animate-spin" />
        ) : (
          <Search className="w-3.5 h-3.5" />
        )}
        {scanning ? "Scanning..." : "Check Drift"}
      </button>

      {open && (report || error) && (
        <DriftReportModal
          report={report}
          error={error}
          onClose={() => setOpen(false)}
          totalIssues={totalIssues}
        />
      )}
    </>
  );
}

function DriftReportModal({
  report,
  error,
  onClose,
  totalIssues,
}: {
  report: DriftReport | null;
  error: string | null;
  onClose: () => void;
  totalIssues: number;
}) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4">
      <div className="bg-card border border-border rounded-xl shadow-2xl max-w-4xl w-full max-h-[85vh] flex flex-col">
        <div className="flex items-center justify-between px-5 py-3 border-b border-border">
          <div className="flex items-center gap-2">
            {error ? (
              <AlertTriangle className="w-5 h-5 text-destructive" />
            ) : totalIssues === 0 ? (
              <CheckCircle2 className="w-5 h-5 text-green-500" />
            ) : (
              <AlertTriangle className="w-5 h-5 text-amber-500" />
            )}
            <h2 className="text-base font-semibold">Spec Drift Report</h2>
          </div>
          <button onClick={onClose} className="p-1 hover:bg-muted rounded-md" aria-label="Close">
            <X className="w-4 h-4" />
          </button>
        </div>
        <div className="flex-1 overflow-y-auto p-5 space-y-4 text-sm">
          {error && (
            <div className="rounded-md bg-destructive/10 border border-destructive/30 p-3 text-destructive">
              {error}
            </div>
          )}
          {report && (
            <>
              <div className="grid grid-cols-2 gap-3 text-xs text-muted-foreground">
                <div>
                  <div>Source files scanned: {report.source_files_scanned}</div>
                  <div>Spec files scanned: {report.spec_files_scanned}</div>
                </div>
                <div>
                  <div>Registered elements: {report.total_registered_elements}</div>
                  <div>Asserted element IDs: {report.total_asserted_ids}</div>
                </div>
              </div>

              {totalIssues === 0 && (
                <div className="rounded-md bg-green-500/10 border border-green-500/30 p-3 text-green-500">
                  No drift detected. Every registered element has a matching assertion, and every
                  asserted element ID appears in source.
                </div>
              )}

              {report.missing_from_spec.length > 0 && (
                <section>
                  <h3 className="font-semibold mb-2">
                    Missing from spec ({report.missing_from_spec.length})
                  </h3>
                  <p className="text-xs text-muted-foreground mb-2">
                    Elements registered via useUIElement in code but not referenced by any spec
                    assertion. Click &quot;Copy&quot; to copy a suggested assertion JSON to paste
                    into the relevant spec.
                  </p>
                  <div className="space-y-2">
                    {report.missing_from_spec.map((e) => (
                      <div
                        key={`${e.file}:${e.id}`}
                        className="rounded-md border border-border bg-muted/20 p-2"
                      >
                        <div className="flex items-center justify-between gap-3">
                          <div className="min-w-0 flex-1">
                            <div className="font-mono text-xs text-foreground truncate">{e.id}</div>
                            <div className="text-xs text-muted-foreground truncate">
                              {e.label ?? "(no label)"}
                              {" · "}
                              {e.file}:{e.line}
                            </div>
                          </div>
                          <button
                            onClick={() => {
                              void navigator.clipboard.writeText(e.suggested_assertion);
                            }}
                            className="shrink-0 inline-flex items-center gap-1 px-2 py-1 text-xs rounded-md
                              bg-primary/15 text-primary hover:bg-primary/25"
                          >
                            <Copy className="w-3 h-3" />
                            Copy assertion
                          </button>
                        </div>
                      </div>
                    ))}
                  </div>
                </section>
              )}

              {report.orphans_in_spec.length > 0 && (
                <section>
                  <h3 className="font-semibold mb-2">
                    Orphan assertions ({report.orphans_in_spec.length})
                  </h3>
                  <p className="text-xs text-muted-foreground mb-2">
                    Assertions whose target elementId no longer exists in source. Consider removing
                    them, or the element may have been renamed.
                  </p>
                  <div className="space-y-1">
                    {report.orphans_in_spec.map((o) => (
                      <div
                        key={`${o.spec_id}:${o.assertion_id}`}
                        className="rounded-md border border-border bg-muted/10 p-2 text-xs"
                      >
                        <span className="font-mono">{o.element_id}</span>
                        <span className="text-muted-foreground">
                          {" "}
                          — in <span className="font-mono">{o.spec_id}</span>.spec
                          {o.assertion_id ? ` (${o.assertion_id})` : ""}
                          {o.label ? ` · "${o.label}"` : ""}
                        </span>
                      </div>
                    ))}
                  </div>
                </section>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
