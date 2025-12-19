/**
 * EventManagerContext
 *
 * Sets up Tauri event listeners and routes events through the EventRouter.
 * Provides a centralized event management system for the application.
 */

import { createContext, useContext, useEffect, useState, ReactNode } from "react";
import { eventRouter, logManager } from "../managers";
import { verificationService } from "../services";

interface EventManagerContextValue {
  isConnected: boolean;
}

// Store context in window to survive HMR reloads
declare global {
  interface Window {
    __EVENT_MANAGER_CONTEXT__?: React.Context<EventManagerContextValue | null>;
  }
}

// Create context once and store in window to survive HMR reloads
const EventManagerContext: React.Context<EventManagerContextValue | null> =
  window.__EVENT_MANAGER_CONTEXT__ ||
  (window.__EVENT_MANAGER_CONTEXT__ = createContext<EventManagerContextValue | null>(null));

interface EventManagerProviderProps {
  children: ReactNode;
}

export function EventManagerProvider({ children }: EventManagerProviderProps) {
  const [isConnected, setIsConnected] = useState(false);

  useEffect(() => {
    let unlistenExecutor: (() => void) | null = null;
    let unlistenAiOutput: (() => void) | null = null;
    let isMounted = true;

    // Check for and auto-trigger pending verification
    const checkPendingVerification = async () => {
      try {
        const pending = await verificationService.loadPendingVerification();
        if (pending && pending.status === "pending") {
          console.log("[EVENT_MGR] Found pending verification, auto-triggering...");

          // Small delay to ensure everything is ready
          await new Promise((resolve) => setTimeout(resolve, 2000));

          // Get the verification prompt
          const verificationPrompt = await verificationService.triggerVerification();
          if (!verificationPrompt) {
            console.warn("[EVENT_MGR] Failed to get verification prompt");
            return;
          }

          // Build full prompt with conversation history for context
          const aiOutputLogs = logManager.getAiOutputLogs();
          let fullPrompt = "";

          // Include previous conversation as context (not displayed again since it's loaded)
          if (aiOutputLogs.length > 0) {
            fullPrompt += "## Restored Conversation Context\n\n";
            fullPrompt += "(Previous conversation has been restored from before the restart)\n\n";
            fullPrompt += "---\n\n";
          }

          fullPrompt += verificationPrompt;

          console.log("[EVENT_MGR] Sending verification prompt to AI...");

          // Send to AI analysis endpoint
          const response = await fetch("http://localhost:9876/trigger-ai-analysis", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              prompt: fullPrompt,
              display_prompt: "Verification: Checking if the fix worked...",
              timeout_seconds: 600,
            }),
          });

          const result = await response.json();
          if (result.success) {
            console.log("[EVENT_MGR] Verification prompt sent successfully");
          } else {
            console.error("[EVENT_MGR] Verification prompt failed:", result.error);
          }
        } else if (pending) {
          console.log("[EVENT_MGR] Pending verification exists but status is:", pending.status);
        }
      } catch (error) {
        console.error("[EVENT_MGR] Error checking pending verification:", error);
      }
    };

    const setupListeners = async () => {
      try {
        // Initialize LogManager first to load conversation history before new events arrive
        await logManager.initialize();

        const { listen } = await import("@tauri-apps/api/event");

        console.log("[EVENT_MGR] Setting up Tauri event listeners");

        // Listen for executor events
        const unlistenExecutorFn = await listen("executor-event", (event: any) => {
          // Prevent processing events if component is unmounted
          if (!isMounted) {
            console.log("[EVENT_MGR] Component unmounted, ignoring event");
            return;
          }

          const data = event.payload;

          // Route event through EventRouter
          eventRouter.route(data);
        });

        // Listen for AI output events
        const unlistenAiOutputFn = await listen("ai-output", (event: any) => {
          if (!isMounted) {
            return;
          }

          const data = event.payload;
          console.log("[EVENT_MGR] AI output event:", data.source, data.line?.substring(0, 50));

          // Route AI output to LogManager
          if (data.line !== undefined && data.source !== undefined) {
            logManager.addAiOutputLog(data.line, data.source, data.actionId);
          }
        });

        unlistenExecutor = unlistenExecutorFn;
        unlistenAiOutput = unlistenAiOutputFn;
        setIsConnected(true);
        console.log("[EVENT_MGR] Event listeners set up successfully");

        // Check for pending verification after initialization
        checkPendingVerification();
      } catch (error) {
        console.error("[EVENT_MGR] Failed to set up event listeners:", error);
        setIsConnected(false);
      }
    };

    setupListeners();

    // Cleanup
    return () => {
      console.log("[EVENT_MGR] Cleaning up event listeners");
      isMounted = false;
      if (unlistenExecutor) {
        unlistenExecutor();
      }
      if (unlistenAiOutput) {
        unlistenAiOutput();
      }
      setIsConnected(false);
    };
  }, []);

  const value: EventManagerContextValue = {
    isConnected,
  };

  return <EventManagerContext.Provider value={value}>{children}</EventManagerContext.Provider>;
}

/**
 * Hook to access event manager context
 */
export function useEventManager() {
  const context = useContext(EventManagerContext);
  if (!context) {
    throw new Error("useEventManager must be used within EventManagerProvider");
  }
  return context;
}
