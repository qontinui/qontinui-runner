/**
 * EventManagerContext
 *
 * Sets up Tauri event listeners and routes events through the EventRouter.
 * Provides a centralized event management system for the application.
 */

import { createContext, useContext, useEffect, useState, ReactNode } from "react";
import { eventRouter } from "../managers";

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
    let unlisten: (() => void) | null = null;
    let isMounted = true;

    const setupListeners = async () => {
      try {
        const { listen } = await import("@tauri-apps/api/event");

        console.log("[EVENT_MGR] Setting up Tauri event listener");

        const unlistenFn = await listen("executor-event", (event: any) => {
          // Prevent processing events if component is unmounted
          if (!isMounted) {
            console.log("[EVENT_MGR] Component unmounted, ignoring event");
            return;
          }

          const data = event.payload;

          // Route event through EventRouter
          eventRouter.route(data);
        });

        unlisten = unlistenFn;
        setIsConnected(true);
        console.log("[EVENT_MGR] Event listener set up successfully");
      } catch (error) {
        console.error("[EVENT_MGR] Failed to set up event listener:", error);
        setIsConnected(false);
      }
    };

    setupListeners();

    // Cleanup
    return () => {
      console.log("[EVENT_MGR] Cleaning up event listener");
      isMounted = false;
      if (unlisten) {
        unlisten();
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
