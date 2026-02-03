/**
 * ProgressErrorBoundary Component
 *
 * A specialized error boundary for progress-related components.
 * Provides subtle fallback UI that doesn't disrupt the dashboard layout.
 *
 * Features:
 * - Minimal visual footprint on error (shows "--" or empty state)
 * - Auto-retry with configurable delay
 * - Development mode error logging
 * - Compact variant for inline progress indicators
 *
 * @example
 * // Wrap a progress bar
 * <ProgressErrorBoundary>
 *   <ProgressBar current={data.current} total={data.total} />
 * </ProgressErrorBoundary>
 *
 * // Compact mode for inline indicators
 * <ProgressErrorBoundary compact>
 *   <InlineProgressBar current={step.current} total={step.total} />
 * </ProgressErrorBoundary>
 *
 * // With custom error placeholder
 * <ProgressErrorBoundary placeholder="N/A">
 *   <StepProgressMarker taskRunId={id} checkpointId={cpId} />
 * </ProgressErrorBoundary>
 */

import { Component, type ReactNode, type ErrorInfo } from "react";
import { cn } from "../../lib/utils";

export interface ProgressErrorBoundaryProps {
  /** Child components to render */
  children: ReactNode;
  /** Compact mode for inline progress indicators */
  compact?: boolean;
  /** Custom placeholder text when error occurs */
  placeholder?: string;
  /** Additional CSS classes for the fallback container */
  className?: string;
  /** Enable auto-retry functionality (default: true) */
  enableRetry?: boolean;
  /** Delay in milliseconds before auto-retry (default: 5000) */
  retryDelay?: number;
  /** Maximum number of retry attempts (default: 2) */
  maxRetries?: number;
  /** Callback fired when an error is caught */
  onError?: (error: Error, errorInfo: ErrorInfo) => void;
  /** Component name for error reporting */
  componentName?: string;
}

interface ProgressErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
  retryCount: number;
  isRetrying: boolean;
}

/**
 * Specialized error boundary for progress components.
 *
 * Designed to fail gracefully without drawing attention - errors in progress
 * indicators should not break the entire dashboard.
 */
export class ProgressErrorBoundary extends Component<
  ProgressErrorBoundaryProps,
  ProgressErrorBoundaryState
> {
  private retryTimeoutId: ReturnType<typeof setTimeout> | null = null;

  static defaultProps: Partial<ProgressErrorBoundaryProps> = {
    compact: false,
    placeholder: "--",
    enableRetry: true,
    retryDelay: 5000,
    maxRetries: 2,
  };

  constructor(props: ProgressErrorBoundaryProps) {
    super(props);
    this.state = {
      hasError: false,
      error: null,
      retryCount: 0,
      isRetrying: false,
    };
  }

  static getDerivedStateFromError(error: Error): Partial<ProgressErrorBoundaryState> {
    return {
      hasError: true,
      error,
    };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo): void {
    // Call error callback if provided
    if (this.props.onError) {
      this.props.onError(error, errorInfo);
    }

    // Log error in development mode
    if (import.meta.env.DEV) {
      const componentName = this.props.componentName || "Progress";
      console.warn(`[ProgressErrorBoundary] Error in ${componentName}:`, error.message);
      if (errorInfo.componentStack) {
        console.debug("[ProgressErrorBoundary] Component stack:", errorInfo.componentStack);
      }
    }

    // Start auto-retry if enabled and we haven't exceeded max retries
    if (
      this.props.enableRetry &&
      this.state.retryCount < (this.props.maxRetries ?? 2)
    ) {
      this.scheduleRetry();
    }
  }

  componentWillUnmount(): void {
    if (this.retryTimeoutId) {
      clearTimeout(this.retryTimeoutId);
    }
  }

  private scheduleRetry(): void {
    const retryDelay = this.props.retryDelay ?? 5000;

    this.setState({ isRetrying: true });

    this.retryTimeoutId = setTimeout(() => {
      this.handleRetry();
    }, retryDelay);
  }

  handleRetry = (): void => {
    if (this.retryTimeoutId) {
      clearTimeout(this.retryTimeoutId);
      this.retryTimeoutId = null;
    }

    this.setState((prevState) => ({
      hasError: false,
      error: null,
      retryCount: prevState.retryCount + 1,
      isRetrying: false,
    }));
  };

  render(): ReactNode {
    if (this.state.hasError) {
      const { compact, placeholder, className } = this.props;

      // Compact fallback for inline indicators
      if (compact) {
        return (
          <span
            className={cn(
              "text-xs text-muted-foreground/50 tabular-nums",
              className,
            )}
            title={
              import.meta.env.DEV
                ? `Error: ${this.state.error?.message}`
                : undefined
            }
          >
            {placeholder}
          </span>
        );
      }

      // Standard fallback - minimal visual footprint
      return (
        <div
          className={cn(
            "flex items-center justify-center text-muted-foreground/40",
            className,
          )}
          title={
            import.meta.env.DEV
              ? `Error: ${this.state.error?.message}`
              : undefined
          }
        >
          <span className="text-xs tabular-nums">{placeholder}</span>
        </div>
      );
    }

    return this.props.children;
  }
}

/**
 * Higher-order component that wraps a component with ProgressErrorBoundary.
 *
 * @example
 * const SafeProgressBar = withProgressErrorBoundary(ProgressBar);
 */
export function withProgressErrorBoundary<P extends object>(
  WrappedComponent: React.ComponentType<P>,
  boundaryProps?: Omit<ProgressErrorBoundaryProps, "children">,
): React.FC<P> {
  const displayName = WrappedComponent.displayName || WrappedComponent.name || "Component";

  const WithErrorBoundary: React.FC<P> = (props) => (
    <ProgressErrorBoundary
      {...boundaryProps}
      componentName={boundaryProps?.componentName || displayName}
    >
      <WrappedComponent {...props} />
    </ProgressErrorBoundary>
  );

  WithErrorBoundary.displayName = `withProgressErrorBoundary(${displayName})`;

  return WithErrorBoundary;
}

export default ProgressErrorBoundary;
