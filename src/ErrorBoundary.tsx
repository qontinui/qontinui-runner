import { Component, ErrorInfo, ReactNode } from "react";

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
