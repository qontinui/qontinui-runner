/**
 * FailureSection Component
 *
 * Displays detailed failure information when a task run fails.
 */

import { XCircle } from "lucide-react";
import type { FailureInfo } from "@/types/recap";

interface FailureSectionProps {
  failure: FailureInfo;
}

export function FailureSection({ failure }: FailureSectionProps) {
  return (
    <div
      data-ui-id="recap-failure-section"
      className="bg-red-500/10 border border-red-500/30 rounded-lg p-4 space-y-3"
    >
      <div className="flex items-start gap-3">
        <XCircle className="w-5 h-5 text-red-500 flex-shrink-0 mt-0.5" />
        <div className="flex-1 min-w-0">
          <h3 className="font-medium text-red-500">Run Failed</h3>
          <p data-ui-id="recap-failure-reason" className="text-sm text-red-400 mt-1">
            {failure.reason}
          </p>
        </div>
      </div>

      {failure.failed_step && (
        <div className="ml-8 text-sm">
          <span data-content-role="label" className="text-muted-foreground">
            Failed at:{" "}
          </span>
          <span
            data-ui-id="recap-failed-step"
            data-content-role="label"
            data-content-label="failed step"
            className="text-red-400 font-medium"
          >
            {failure.failed_step}
          </span>
        </div>
      )}

      {failure.error_type && (
        <div className="ml-8 text-sm">
          <span data-content-role="label" className="text-muted-foreground">
            Error type:{" "}
          </span>
          <span
            data-ui-id="recap-error-type"
            data-content-role="label"
            data-content-label="error type"
            className="text-red-400"
          >
            {failure.error_type}
          </span>
        </div>
      )}

      {failure.error_details && failure.error_details !== failure.reason && (
        <div className="ml-8 mt-2">
          <details data-ui-id="recap-error-details" className="text-sm">
            <summary className="cursor-pointer text-muted-foreground hover:text-foreground">
              Show error details
            </summary>
            <pre className="mt-2 p-3 bg-background/50 rounded text-xs overflow-x-auto whitespace-pre-wrap text-red-300">
              {failure.error_details}
            </pre>
          </details>
        </div>
      )}
    </div>
  );
}

export default FailureSection;
