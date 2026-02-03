/**
 * ErrorBoundary Component
 *
 * A generic error boundary that catches JavaScript errors anywhere in the
 * child component tree, logs them, and displays a fallback UI.
 *
 * Error boundaries only catch errors in:
 * - Rendering
 * - Lifecycle methods
 * - Constructors of the whole tree below them
 *
 * Error boundaries do NOT catch errors in:
 * - Event handlers
 * - Asynchronous code
 * - Server-side rendering
 * - Errors thrown in the error boundary itself
 *
 * @example
 * // Basic usage with default fallback
 * <ErrorBoundary>
 *   <ComponentThatMightFail />
 * </ErrorBoundary>
 *
 * // Custom fallback UI
 * <ErrorBoundary fallback={<div>Something went wrong</div>}>
 *   <ComponentThatMightFail />
 * </ErrorBoundary>
 *
 * // With error callback for logging
 * <ErrorBoundary onError={(error, info) => logToService(error, info)}>
 *   <ComponentThatMightFail />
 * </ErrorBoundary>
 */

import { Component, type ReactNode, type ErrorInfo } from "react";

export interface ErrorBoundaryProps {
  /** Child components to render */
  children: ReactNode;
  /** Fallback UI to render when an error occurs */
  fallback?: ReactNode;
  /** Callback fired when an error is caught */
  onError?: (error: Error, errorInfo: ErrorInfo) => void;
  /** Whether to show error details in development mode */
  showErrorInDev?: boolean;
  /** Enable auto-retry functionality */
  enableRetry?: boolean;
  /** Maximum number of retry attempts */
  maxRetries?: number;
  /** Delay in milliseconds before auto-retry */
  retryDelay?: number;
  /** Component name for error reporting */
  componentName?: string;
}

interface ErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
  errorInfo: ErrorInfo | null;
  retryCount: number;
  isRetrying: boolean;
}

/**
 * Generic error boundary component.
 *
 * Must be a class component as React currently only supports error boundaries
 * via the componentDidCatch lifecycle method.
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  private retryTimeoutId: ReturnType<typeof setTimeout> | null = null;

  static defaultProps: Partial<ErrorBoundaryProps> = {
    showErrorInDev: true,
    enableRetry: false,
    maxRetries: 3,
    retryDelay: 5000,
  };

  constructor(props: ErrorBoundaryProps) {
    super(props);
    this.state = {
      hasError: false,
      error: null,
      errorInfo: null,
      retryCount: 0,
      isRetrying: false,
    };
  }

  /**
   * Update state when an error is thrown.
   * This lifecycle is invoked after an error has been thrown by a descendant component.
   */
  static getDerivedStateFromError(error: Error): Partial<ErrorBoundaryState> {
    return {
      hasError: true,
      error,
    };
  }

  /**
   * Log the error and call the error callback.
   * This lifecycle is invoked after an error has been thrown by a descendant component.
   */
  componentDidCatch(error: Error, errorInfo: ErrorInfo): void {
    this.setState({ errorInfo });

    // Call error callback if provided
    if (this.props.onError) {
      this.props.onError(error, errorInfo);
    }

    // Log error in development mode
    if (import.meta.env.DEV) {
      const componentName = this.props.componentName || "Unknown";
      console.error(`[ErrorBoundary] Error in ${componentName}:`, error);
      console.error("[ErrorBoundary] Component stack:", errorInfo.componentStack);
    }

    // Start auto-retry if enabled and we haven't exceeded max retries
    if (
      this.props.enableRetry &&
      this.state.retryCount < (this.props.maxRetries ?? 3)
    ) {
      this.scheduleRetry();
    }
  }

  componentWillUnmount(): void {
    // Clean up any pending retry timeout
    if (this.retryTimeoutId) {
      clearTimeout(this.retryTimeoutId);
    }
  }

  /**
   * Schedule an automatic retry after the configured delay.
   */
  private scheduleRetry(): void {
    const retryDelay = this.props.retryDelay ?? 5000;

    this.setState({ isRetrying: true });

    this.retryTimeoutId = setTimeout(() => {
      this.handleRetry();
    }, retryDelay);
  }

  /**
   * Manually retry rendering the children.
   */
  handleRetry = (): void => {
    if (this.retryTimeoutId) {
      clearTimeout(this.retryTimeoutId);
      this.retryTimeoutId = null;
    }

    this.setState((prevState) => ({
      hasError: false,
      error: null,
      errorInfo: null,
      retryCount: prevState.retryCount + 1,
      isRetrying: false,
    }));
  };

  /**
   * Reset the error state completely (including retry count).
   */
  handleReset = (): void => {
    if (this.retryTimeoutId) {
      clearTimeout(this.retryTimeoutId);
      this.retryTimeoutId = null;
    }

    this.setState({
      hasError: false,
      error: null,
      errorInfo: null,
      retryCount: 0,
      isRetrying: false,
    });
  };

  render(): ReactNode {
    if (this.state.hasError) {
      // If a custom fallback is provided, render it
      if (this.props.fallback !== undefined) {
        return this.props.fallback;
      }

      // Default fallback UI
      const showDetails =
        this.props.showErrorInDev && import.meta.env.DEV;
      const canRetry =
        this.props.enableRetry &&
        this.state.retryCount < (this.props.maxRetries ?? 3);

      return (
        <div className="p-4 border border-red-500/30 rounded-md bg-red-500/5">
          <div className="flex items-center justify-between gap-2">
            <span className="text-sm text-red-400">Something went wrong</span>
            {canRetry && (
              <button
                onClick={this.handleRetry}
                disabled={this.state.isRetrying}
                className="text-xs text-muted-foreground hover:text-foreground transition-colors"
              >
                {this.state.isRetrying
                  ? "Retrying..."
                  : `Retry (${this.state.retryCount + 1}/${this.props.maxRetries})`}
              </button>
            )}
          </div>
          {showDetails && this.state.error && (
            <pre className="mt-2 text-xs text-red-300/70 overflow-auto max-h-32">
              {this.state.error.message}
            </pre>
          )}
        </div>
      );
    }

    return this.props.children;
  }
}

export default ErrorBoundary;
