import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { setDevelopmentMode } from "qontinui-navigation";
import { ProductModeProvider } from "./contexts/ProductModeContext";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import { setScriptEmitter, TauriScriptEmitter } from "./lib/step-output-handlers/script-emitter";
import "./index.css";

// Set development mode for navigation (shows hidden items with badge)
if (import.meta.env.DEV) {
  setDevelopmentMode(true);
}

// Register the production ScriptEmitter so scripted-output handlers
// (see `src/lib/step-output-handlers/scripted-base.ts`) route through
// the Rust `emit_extraction_script` Tauri command instead of the noop
// fallback.  Guard on the presence of Tauri's injected globals so this
// is a no-op in non-Tauri contexts (unit tests, web builds) where the
// `invoke` import would throw on first use.  The env-var gate
// (`QONTINUI_SCRIPTED_OUTPUT=1`) and the `scripted_output.enabled`
// settings flag are enforced further down the stack — the TS side only
// installs the adapter.
//
// Runs at module-load time (before `ReactDOM.createRoot`) so the emitter
// is available the instant any async handler fires.
if (
  typeof window !== "undefined" &&
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  ((window as any).__TAURI_INTERNALS__ || (window as any).__TAURI__)
) {
  setScriptEmitter(new TauriScriptEmitter());
}

// Create a client for react-query
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60, // 1 minute
      retry: 1,
    },
  },
});

const rootElement = document.getElementById("root");
if (!rootElement) {
  console.error("Root element not found!");
  document.body.innerHTML = '<div style="color: red; padding: 20px;">Root element not found!</div>';
} else {
  ReactDOM.createRoot(rootElement).render(
    <React.StrictMode>
      <QueryClientProvider client={queryClient}>
        <ErrorBoundary>
          <ProductModeProvider>
            <App />
          </ProductModeProvider>
        </ErrorBoundary>
      </QueryClientProvider>
    </React.StrictMode>,
  );
}
