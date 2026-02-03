/**
 * Error Boundary Test Components
 *
 * These components are used to verify error boundary functionality.
 * They can be used in development to test error recovery behavior.
 *
 * @example
 * // In development, render a test error thrower
 * <ErrorBoundaryTestSuite />
 */

import React, { useState } from "react";
import { ErrorBoundary } from "../ErrorBoundary";
import { ProgressErrorBoundary } from "../ProgressErrorBoundary";
import { ProgressBar, InlineProgressBar } from "../ProgressBar";

/**
 * Component that throws an error on render.
 */
function ErrorThrower({ message = "Test error" }: { message?: string }): React.JSX.Element {
  throw new Error(message);
}

/**
 * Component that throws after a delay (simulates async error in render).
 */
function DelayedErrorThrower({
  delay = 100,
  message = "Delayed test error",
}: {
  delay?: number;
  message?: string;
}) {
  const [shouldThrow, setShouldThrow] = useState(false);

  if (shouldThrow) {
    throw new Error(message);
  }

  // Trigger error after delay
  setTimeout(() => setShouldThrow(true), delay);

  return <span>Waiting to throw...</span>;
}

// Export for test usage (prefixed export to avoid unused warning)
export { DelayedErrorThrower as _DelayedErrorThrower };

/**
 * Component that receives malformed progress data.
 */
function MalformedProgressData() {
  // These would normally cause issues but defensive parsing handles them
  const badData = {
    current: "not a number" as unknown as number,
    total: undefined as unknown as number | null,
  };

  return (
    <div className="space-y-4">
      <h3 className="text-sm font-medium">Malformed Progress Data Test</h3>
      <ProgressBar current={badData.current} total={badData.total} showLabel />
      <InlineProgressBar current={badData.current} total={badData.total} />
    </div>
  );
}

/**
 * Progress component that throws during render.
 */
function BrokenProgressComponent() {
  const data: { value: number } | null = null;
  // This will throw: Cannot read property 'value' of null
  return <ProgressBar current={data!.value} total={100} />;
}

/**
 * Test suite component for verifying error boundary behavior.
 * Only render this in development mode.
 */
export function ErrorBoundaryTestSuite() {
  const [showTests, setShowTests] = useState(false);

  if (import.meta.env.MODE !== "development") {
    return null;
  }

  return (
    <div className="p-4 space-y-6 border border-amber-500/30 rounded-lg bg-amber-500/5">
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold text-amber-400">Error Boundary Test Suite</h2>
        <button
          onClick={() => setShowTests(!showTests)}
          className="px-3 py-1 text-sm border border-amber-500/50 rounded hover:bg-amber-500/10"
        >
          {showTests ? "Hide Tests" : "Show Tests"}
        </button>
      </div>

      {showTests && (
        <div className="space-y-6">
          {/* Test 1: Generic Error Boundary */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">1. Generic ErrorBoundary</h3>
            <ErrorBoundary
              onError={(error) => console.log("[Test] Caught error:", error.message)}
            >
              <ErrorThrower message="Generic boundary test" />
            </ErrorBoundary>
          </div>

          {/* Test 2: Error Boundary with Retry */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">2. ErrorBoundary with Retry</h3>
            <ErrorBoundary
              enableRetry
              maxRetries={3}
              retryDelay={3000}
              onError={(error) => console.log("[Test] Caught error (retry enabled):", error.message)}
            >
              <ErrorThrower message="Retry boundary test" />
            </ErrorBoundary>
          </div>

          {/* Test 3: Progress Error Boundary (compact) */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">3. ProgressErrorBoundary (compact)</h3>
            <div className="flex items-center gap-2">
              <span>Progress:</span>
              <ProgressErrorBoundary compact>
                <BrokenProgressComponent />
              </ProgressErrorBoundary>
            </div>
          </div>

          {/* Test 4: Progress Error Boundary (standard) */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">4. ProgressErrorBoundary (standard)</h3>
            <ProgressErrorBoundary>
              <BrokenProgressComponent />
            </ProgressErrorBoundary>
          </div>

          {/* Test 5: Malformed Data Handling */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">5. Malformed Data (Defensive Parsing)</h3>
            <p className="text-xs text-muted-foreground">
              Should render without errors due to defensive parsing
            </p>
            <MalformedProgressData />
          </div>

          {/* Test 6: Valid Progress (control) */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">6. Valid Progress (control)</h3>
            <ProgressErrorBoundary>
              <ProgressBar current={7} total={10} showLabel labelFormat="both" />
            </ProgressErrorBoundary>
          </div>

          {/* Test 7: Custom Fallback */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">7. Custom Fallback</h3>
            <ErrorBoundary
              fallback={
                <span className="text-xs text-muted-foreground italic">
                  Custom fallback rendered
                </span>
              }
            >
              <ErrorThrower message="Custom fallback test" />
            </ErrorBoundary>
          </div>

          {/* Test 8: Nested Error Boundaries */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">8. Nested Error Boundaries</h3>
            <ErrorBoundary
              componentName="Outer"
              onError={(e) => console.log("[Test] Outer caught:", e.message)}
            >
              <div className="p-2 border border-border rounded">
                <span className="text-xs">Outer boundary content</span>
                <ErrorBoundary
                  componentName="Inner"
                  onError={(e) => console.log("[Test] Inner caught:", e.message)}
                >
                  <ErrorThrower message="Inner boundary error" />
                </ErrorBoundary>
              </div>
            </ErrorBoundary>
          </div>
        </div>
      )}
    </div>
  );
}

export default ErrorBoundaryTestSuite;
