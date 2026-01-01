/**
 * SessionManager Service
 *
 * Singleton service that provides a single source of truth for AI session management.
 * Tracks the current session context including id, actionId, name, status, and timing.
 * Supports event subscription for session state changes.
 */

/**
 * Session status types
 */
export type SessionStatus = "active" | "completed" | "failed" | "cancelled";

/**
 * Session context representing the current or a previous session
 */
export interface SessionContext {
  id: string;
  actionId: string;
  name: string;
  startedAt: number;
  status: SessionStatus;
}

/**
 * Listener callback for session changes
 */
export type SessionChangeListener = (
  session: SessionContext | null,
  previousSession: SessionContext | null,
) => void;

export class SessionManager {
  private static instance: SessionManager | null = null;

  private currentSession: SessionContext | null = null;
  private listeners: Set<SessionChangeListener> = new Set();

  private constructor() {
    // Private constructor for singleton
  }

  /** Get the singleton instance */
  static getInstance(): SessionManager {
    if (!SessionManager.instance) {
      SessionManager.instance = new SessionManager();
    }
    return SessionManager.instance;
  }

  /** Reset the singleton (useful for testing) */
  static resetInstance(): void {
    SessionManager.instance = null;
  }

  /**
   * Start a new session
   *
   * If a session is already active, it will be auto-ended with "cancelled" status
   * before starting the new session.
   *
   * @param actionId - The action ID associated with this session
   * @param name - Optional name for the session (defaults to "AI Session")
   * @returns The newly created SessionContext
   */
  startSession(actionId: string, name?: string): SessionContext {
    // Auto-end previous session if one exists
    if (this.currentSession) {
      console.log(
        `[SessionManager] Auto-ending previous session: ${this.currentSession.id} (was ${this.currentSession.status})`,
      );
      this.endSession("cancelled");
    }

    const session: SessionContext = {
      id: crypto.randomUUID(),
      actionId,
      name: name ?? "AI Session",
      startedAt: Date.now(),
      status: "active",
    };

    const previousSession = this.currentSession;
    this.currentSession = session;

    this.notifyListeners(session, previousSession);

    console.log(
      `[SessionManager] Session started: ${session.name} (${session.id}) for action ${actionId}`,
    );

    return session;
  }

  /**
   * End the current session with a status
   *
   * @param status - The final status of the session
   * @returns The ended session context, or null if no session was active
   */
  endSession(status: SessionStatus): SessionContext | null {
    if (!this.currentSession) {
      console.warn("[SessionManager] No active session to end");
      return null;
    }

    const endedSession: SessionContext = {
      ...this.currentSession,
      status,
    };

    const previousSession = this.currentSession;
    this.currentSession = null;

    this.notifyListeners(null, previousSession);

    console.log(
      `[SessionManager] Session ended: ${endedSession.name} (${endedSession.id}) with status: ${status}`,
    );

    return endedSession;
  }

  /**
   * Get the current session context
   *
   * @returns The current session, or null if no session is active
   */
  getCurrentSession(): SessionContext | null {
    return this.currentSession;
  }

  /**
   * Subscribe to session changes
   *
   * @param callback - Function to call when session state changes
   * @returns Unsubscribe function
   */
  subscribe(callback: SessionChangeListener): () => void {
    this.listeners.add(callback);
    return () => this.listeners.delete(callback);
  }

  /**
   * Notify all listeners of a session change
   */
  private notifyListeners(
    session: SessionContext | null,
    previousSession: SessionContext | null,
  ): void {
    this.listeners.forEach((listener) => {
      try {
        listener(session, previousSession);
      } catch (error) {
        console.error("[SessionManager] Listener error:", error);
      }
    });
  }
}

// Export singleton instance
export const sessionManager = SessionManager.getInstance();
