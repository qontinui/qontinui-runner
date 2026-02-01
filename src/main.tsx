import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { setDevelopmentMode } from "qontinui-navigation";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import "./index.css";

// Set development mode for navigation (shows hidden items with badge)
if (import.meta.env.DEV) {
  setDevelopmentMode(true);
}

// Add debugging
console.log("Main.tsx loaded");

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
  console.log("Root element found, rendering app...");
  ReactDOM.createRoot(rootElement).render(
    <React.StrictMode>
      <QueryClientProvider client={queryClient}>
        <ErrorBoundary>
          <App />
        </ErrorBoundary>
      </QueryClientProvider>
    </React.StrictMode>,
  );
}
