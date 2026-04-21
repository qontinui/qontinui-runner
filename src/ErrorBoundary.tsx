import { Component, ErrorInfo, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
  errorInfo: ErrorInfo | null;
}

class ErrorBoundary extends Component<Props, State> {
  public state: State = {
    hasError: false,
    error: null,
    errorInfo: null,
  };

  public static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error, errorInfo: null };
  }

  public componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    // Log full error details to console
    console.error("=== ErrorBoundary caught error ===");
    console.error("Error name:", error.name);
    console.error("Error message:", error.message);
    console.error("Error stack:", error.stack);
    console.error("Component stack:", errorInfo.componentStack);
    console.error("=================================");
    this.setState({
      error,
      errorInfo,
    });

    // Phase 3J.1 — notify the Rust backend so /health, heartbeats, and
    // supervisor fleet views can flag this runner as errored. Best-effort
    // only: if the IPC fails (non-Tauri context, runtime not ready, etc.)
    // we swallow the error so we never mask the original render failure.
    // React 18+ production builds attach `error.digest` for minified
    // errors — capture it when present for cross-runner aggregation.
    try {
      const digest =
        typeof (error as Error & { digest?: unknown }).digest === "string"
          ? (error as Error & { digest?: string }).digest
          : null;
      invoke("report_ui_error", {
        message: error.message,
        stack: error.stack ?? null,
        componentStack: errorInfo.componentStack ?? null,
        digest: digest ?? null,
      }).catch(() => {
        /* best-effort: swallow IPC failures */
      });
    } catch {
      /* best-effort: non-Tauri context, etc. */
    }
  }

  public componentDidUpdate(_prevProps: Props, prevState: State) {
    // Phase 3J.1 — when the boundary recovers (previously errored, now
    // rendering cleanly), clear the Rust-side UI-error state so health
    // flips back to "healthy". Best-effort only.
    if (prevState.hasError && !this.state.hasError) {
      try {
        invoke("clear_ui_error").catch(() => {
          /* best-effort: swallow IPC failures */
        });
      } catch {
        /* best-effort: non-Tauri context, etc. */
      }
    }
  }

  public render() {
    if (this.state.hasError) {
      const errorMessage = this.state.error?.message || "Unknown error";
      const errorStack = this.state.error?.stack || "";
      const componentStack = this.state.errorInfo?.componentStack || "";

      return (
        <div
          style={{
            padding: "20px",
            backgroundColor: "#1e1e1e",
            color: "#ff6b6b",
            minHeight: "100vh",
            fontFamily: "monospace",
          }}
        >
          <h2>Something went wrong.</h2>
          <div
            style={{
              backgroundColor: "#2d2d2d",
              padding: "15px",
              borderRadius: "8px",
              marginTop: "15px",
              border: "1px solid #ff6b6b",
            }}
          >
            <strong style={{ color: "#ff9999" }}>Error:</strong>
            <pre
              style={{
                color: "#ffffff",
                marginTop: "10px",
                whiteSpace: "pre-wrap",
                wordBreak: "break-word",
              }}
            >
              {errorMessage}
            </pre>
          </div>
          <details open style={{ whiteSpace: "pre-wrap", marginTop: "20px" }}>
            <summary style={{ cursor: "pointer", color: "#ffaa00" }}>Full Stack Trace</summary>
            <pre
              style={{
                color: "#aaaaaa",
                fontSize: "12px",
                marginTop: "10px",
                backgroundColor: "#2d2d2d",
                padding: "10px",
                borderRadius: "4px",
                overflow: "auto",
                maxHeight: "300px",
              }}
            >
              {errorStack}
            </pre>
          </details>
          <details style={{ whiteSpace: "pre-wrap", marginTop: "15px" }}>
            <summary style={{ cursor: "pointer", color: "#ffaa00" }}>Component Stack</summary>
            <pre
              style={{
                color: "#aaaaaa",
                fontSize: "12px",
                marginTop: "10px",
                backgroundColor: "#2d2d2d",
                padding: "10px",
                borderRadius: "4px",
                overflow: "auto",
                maxHeight: "300px",
              }}
            >
              {componentStack}
            </pre>
          </details>
        </div>
      );
    }

    return this.props.children;
  }
}

export default ErrorBoundary;
