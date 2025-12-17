/**
 * EventManagerContext
 *
 * Sets up Tauri event listeners and routes events through the EventRouter.
 * Provides a centralized event management system for the application.
 */

import { createContext, useContext, useEffect, useState, ReactNode } from "react";
import { eventRouter, logManager } from "../managers";

interface EventManagerContextValue {
  isConnected: boolean;
}

const EventManagerContext = createContext<EventManagerContextValue | null>(null);

interface EventManagerProviderProps {
  children: ReactNode;
}

export function EventManagerProvider({ children }: EventManagerProviderProps) {
  const [isConnected, setIsConnected] = useState(false);

  useEffect(() => {
    let unlistenExecutor: (() => void) | null = null;
    let unlistenAiOutput: (() => void) | null = null;
    let isMounted = true;

    const setupListeners = async () => {
      try {
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
