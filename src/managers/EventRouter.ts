/**
 * EventRouter
 *
 * Routes executor events to registered handlers based on event type.
 * Provides a centralized, type-safe way to handle events from the backend.
 */

import { createLogger } from "@/lib/logger";
import type { EventPayload } from "../types/eventPayloads";

const logger = createLogger("EventRouter");

type EventHandler = (payload: EventPayload) => void;

class EventRouter {
  private handlers = new Map<string, Set<EventHandler>>();

  /**
   * Subscribe to a specific event type
   * @param eventType - The event type to listen for (e.g., "ready", "tree_event")
   * @param handler - The function to call when this event type is received
   * @returns Unsubscribe function
   */
  subscribe(eventType: string, handler: EventHandler): () => void {
    logger.debug(`Subscribing to event type: ${eventType}`);

    if (!this.handlers.has(eventType)) {
      this.handlers.set(eventType, new Set());
    }

    this.handlers.get(eventType)!.add(handler);

    // Return unsubscribe function
    return () => {
      logger.debug(`Unsubscribing from event type: ${eventType}`);
      const handlers = this.handlers.get(eventType);
      if (handlers) {
        handlers.delete(handler);
        if (handlers.size === 0) {
          this.handlers.delete(eventType);
        }
      }
    };
  }

  /**
   * Route an event to all registered handlers for that event type
   * @param payload - The event payload from the backend
   */
  route(payload: EventPayload): void {
    // Determine event type
    const eventType = payload.event || payload.type;

    if (!eventType) {
      console.warn("[EVENT_ROUTER] Received event with no type:", payload);
      return;
    }

    logger.debug(
      `Routing event type: ${eventType}, handler count: ${this.handlers.get(eventType)?.size || 0}`,
    );

    // Get handlers for this event type
    const handlers = this.handlers.get(eventType);

    if (!handlers || handlers.size === 0) {
      logger.debug(`No handlers registered for event type: ${eventType}`);
      return;
    }

    // Call all handlers
    handlers.forEach((handler) => {
      try {
        handler(payload);
      } catch (error) {
        console.error(`[EVENT_ROUTER] Error in handler for ${eventType}:`, error);
      }
    });
  }

  /**
   * Get the number of handlers registered for an event type
   */
  getHandlerCount(eventType: string): number {
    return this.handlers.get(eventType)?.size || 0;
  }

  /**
   * Get all registered event types
   */
  getRegisteredEventTypes(): string[] {
    return Array.from(this.handlers.keys());
  }

  /**
   * Clear all handlers (useful for testing/cleanup)
   */
  clearAll(): void {
    logger.debug("Clearing all handlers");
    this.handlers.clear();
  }
}

// Export singleton instance
export const eventRouter = new EventRouter();

// Export class type for type annotations
export type { EventRouter };
