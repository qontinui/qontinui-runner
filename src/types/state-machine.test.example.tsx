/**
 * State Machine Integration Test Examples
 *
 * This file contains example test cases and integration patterns for the
 * state machine API. These examples show how to test state machine interactions
 * and integrate them into React components.
 *
 * Note: These are example patterns, not actual runnable tests.
 * For real tests, use a testing framework like Jest or Vitest.
 */

import { useState, useEffect } from "react";
import {
  executeTransition,
  navigateToState,
  navigateToMultipleStates,
  getActiveStates,
  getAvailableTransitions,
  isStateActive,
  areAllStatesActive,
} from "./state-machine";

// ============================================================================
// Example: State Machine Button Component
// ============================================================================

interface StateTransitionButtonProps {
  transitionId: string;
  label: string;
  onSuccess?: () => void;
  onError?: (error: string) => void;
}

/**
 * Button component that executes a state transition when clicked.
 */
export function StateTransitionButton({
  transitionId,
  label,
  onSuccess,
  onError,
}: StateTransitionButtonProps) {
  const [loading, setLoading] = useState(false);

  const handleClick = async () => {
    setLoading(true);

    const result = await executeTransition(transitionId);

    if (result.success) {
      onSuccess?.();
    } else {
      onError?.(result.error || "Transition failed");
    }

    setLoading(false);
  };

  return (
    <button onClick={handleClick} disabled={loading}>
      {loading ? "Processing..." : label}
    </button>
  );
}

// ============================================================================
// Example: State Navigator Component
// ============================================================================

interface StateNavigatorProps {
  targetState: string;
  label: string;
  className?: string;
}

/**
 * Button component that navigates to a specific state.
 */
export function StateNavigator({ targetState, label, className }: StateNavigatorProps) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleNavigate = async () => {
    setLoading(true);
    setError(null);

    const result = await navigateToState(targetState);

    if (!result.success) {
      setError(result.error || "Navigation failed");
    }

    setLoading(false);
  };

  return (
    <div className={className}>
      <button onClick={handleNavigate} disabled={loading}>
        {loading ? "Navigating..." : label}
      </button>
      {error && <div className="error">{error}</div>}
    </div>
  );
}

// ============================================================================
// Example: Active States Display Component
// ============================================================================

/**
 * Component that displays currently active states with auto-refresh.
 */
export function ActiveStatesDisplay() {
  const [activeStates, setActiveStates] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    loadStates();
    const interval = setInterval(loadStates, 2000); // Refresh every 2 seconds
    return () => clearInterval(interval);
  }, []);

  const loadStates = async () => {
    const result = await getActiveStates();
    if (result.success) {
      setActiveStates(result.active_states);
      setLoading(false);
    }
  };

  if (loading) {
    return <div>Loading states...</div>;
  }

  return (
    <div className="active-states">
      <h3>Active States</h3>
      {activeStates.length === 0 ? (
        <p>No active states</p>
      ) : (
        <ul>
          {activeStates.map((state) => (
            <li key={state}>{state}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ============================================================================
// Example: Available Transitions Display Component
// ============================================================================

/**
 * Component that shows available transitions from current state.
 */
export function AvailableTransitionsPanel() {
  const [transitions, setTransitions] = useState<any[]>([]);
  const [currentState, setCurrentState] = useState<string>("");
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    loadTransitions();
  }, []);

  const loadTransitions = async () => {
    const result = await getAvailableTransitions();
    if (result.success) {
      setTransitions(result.transitions);
      setCurrentState(result.current_state || "");
      setLoading(false);
    }
  };

  const handleExecuteTransition = async (transitionId: string) => {
    const result = await executeTransition(transitionId);
    if (result.success) {
      // Reload transitions after execution
      await loadTransitions();
    }
  };

  if (loading) {
    return <div>Loading transitions...</div>;
  }

  return (
    <div className="transitions-panel">
      <h3>Available Transitions</h3>
      <p>Current State: {currentState || "None"}</p>

      {transitions.length === 0 ? (
        <p>No transitions available</p>
      ) : (
        <div className="transitions-list">
          {transitions.map((transition) => (
            <div key={transition.id} className="transition-item">
              <div>
                <strong>{transition.id}</strong>
                <br />
                From: {transition.from_state} → To: {transition.to_state || "Dynamic"}
              </div>
              <button onClick={() => handleExecuteTransition(transition.id)}>Execute</button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Example: Multi-State Application Launcher
// ============================================================================

interface AppLauncherProps {
  apps: Array<{
    id: string;
    name: string;
    stateId: string;
    icon?: string;
  }>;
}

/**
 * Component for launching multiple applications simultaneously.
 */
export function MultiAppLauncher({ apps }: AppLauncherProps) {
  const [selectedApps, setSelectedApps] = useState<Set<string>>(new Set());
  const [launching, setLaunching] = useState(false);
  const [results, setResults] = useState<Map<string, boolean>>(new Map());

  const toggleApp = (appId: string) => {
    const newSelected = new Set(selectedApps);
    if (newSelected.has(appId)) {
      newSelected.delete(appId);
    } else {
      newSelected.add(appId);
    }
    setSelectedApps(newSelected);
  };

  const launchApps = async () => {
    setLaunching(true);
    setResults(new Map());

    const stateIds = apps.filter((app) => selectedApps.has(app.id)).map((app) => app.stateId);

    const result = await navigateToMultipleStates(stateIds);

    if (result.success && result.results) {
      const newResults = new Map<string, boolean>();
      result.results.forEach((r, index) => {
        const appId = apps[index].id;
        newResults.set(appId, r.success);
      });
      setResults(newResults);
    }

    setLaunching(false);
  };

  return (
    <div className="multi-app-launcher">
      <h3>Launch Applications</h3>

      <div className="app-grid">
        {apps.map((app) => {
          const isSelected = selectedApps.has(app.id);
          const launchResult = results.get(app.id);

          return (
            <div
              key={app.id}
              className={`app-card ${isSelected ? "selected" : ""}`}
              onClick={() => toggleApp(app.id)}
            >
              {app.icon && <span className="icon">{app.icon}</span>}
              <span className="name">{app.name}</span>
              {launchResult !== undefined && (
                <span className={`status ${launchResult ? "success" : "error"}`}>
                  {launchResult ? "✓" : "✗"}
                </span>
              )}
            </div>
          );
        })}
      </div>

      <button onClick={launchApps} disabled={launching || selectedApps.size === 0}>
        {launching ? "Launching..." : `Launch ${selectedApps.size} App(s)`}
      </button>
    </div>
  );
}

// ============================================================================
// Example: State-Aware Component
// ============================================================================

interface StateAwareContentProps {
  requiredStates: string[];
  fallbackContent?: React.ReactNode;
  children: React.ReactNode;
}

/**
 * Component that only renders its children if required states are active.
 */
export function StateAwareContent({
  requiredStates,
  fallbackContent,
  children,
}: StateAwareContentProps) {
  const [isReady, setIsReady] = useState(false);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    checkStates();
    const interval = setInterval(checkStates, 1000);
    return () => clearInterval(interval);
  }, [requiredStates]);

  const checkStates = async () => {
    const result = await getActiveStates();
    if (result.success) {
      const ready = areAllStatesActive(requiredStates, result);
      setIsReady(ready);
      setLoading(false);
    }
  };

  if (loading) {
    return <div>Loading...</div>;
  }

  if (!isReady) {
    return <>{fallbackContent || <div>Required states not active</div>}</>;
  }

  return <>{children}</>;
}

// ============================================================================
// Example: Conditional State Navigator
// ============================================================================

/**
 * Component that navigates to different states based on current state.
 */
export function ConditionalNavigator() {
  const [currentStates, setCurrentStates] = useState<string[]>([]);

  useEffect(() => {
    loadAndNavigate();
  }, []);

  const loadAndNavigate = async () => {
    const states = await getActiveStates();

    if (states.success) {
      setCurrentStates(states.active_states);

      // Navigate based on current state
      if (isStateActive("logged_out", states)) {
        await navigateToState("login_page");
      } else if (isStateActive("logged_in", states)) {
        await navigateToState("dashboard");
      }
    }
  };

  return (
    <div>
      <h3>Current States</h3>
      <ul>
        {currentStates.map((state) => (
          <li key={state}>{state}</li>
        ))}
      </ul>
    </div>
  );
}

// ============================================================================
// Example: State Machine Dashboard
// ============================================================================

/**
 * Comprehensive dashboard showing all state machine information.
 */
export function StateMachineDashboard() {
  const [activeStates, setActiveStates] = useState<string[]>([]);
  const [transitions, setTransitions] = useState<any[]>([]);
  const [currentState, setCurrentState] = useState<string>("");
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    loadDashboard();
    const interval = setInterval(loadDashboard, 3000);
    return () => clearInterval(interval);
  }, []);

  const loadDashboard = async () => {
    const [statesResult, transitionsResult] = await Promise.all([
      getActiveStates(),
      getAvailableTransitions(),
    ]);

    if (statesResult.success) {
      setActiveStates(statesResult.active_states);
      setCurrentState(statesResult.current_state || "");
    }

    if (transitionsResult.success) {
      setTransitions(transitionsResult.transitions);
    }

    setLoading(false);
  };

  const handleTransition = async (transitionId: string) => {
    await executeTransition(transitionId);
    await loadDashboard();
  };

  const handleNavigate = async (stateId: string) => {
    await navigateToState(stateId);
    await loadDashboard();
  };

  if (loading) {
    return <div>Loading dashboard...</div>;
  }

  return (
    <div className="state-machine-dashboard">
      <div className="dashboard-grid">
        <div className="panel active-states-panel">
          <h3>Active States</h3>
          <div className="state-badges">
            {activeStates.map((state) => (
              <span key={state} className="badge">
                {state}
              </span>
            ))}
          </div>
          {currentState && (
            <p>
              <strong>Current:</strong> {currentState}
            </p>
          )}
        </div>

        <div className="panel transitions-panel">
          <h3>Available Transitions</h3>
          <div className="transition-list">
            {transitions.map((t) => (
              <div key={t.id} className="transition-item">
                <div className="transition-info">
                  <strong>{t.id}</strong>
                  <br />
                  <small>
                    {t.from_state} → {t.to_state || "Dynamic"}
                  </small>
                </div>
                <button onClick={() => handleTransition(t.id)}>Execute</button>
              </div>
            ))}
          </div>
        </div>

        <div className="panel quick-nav-panel">
          <h3>Quick Navigation</h3>
          <div className="nav-buttons">
            <button onClick={() => handleNavigate("dashboard")}>Dashboard</button>
            <button onClick={() => handleNavigate("settings")}>Settings</button>
            <button onClick={() => handleNavigate("profile")}>Profile</button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ============================================================================
// Example: Testing Patterns
// ============================================================================

/**
 * Example test patterns (for reference, not actual tests).
 */
export const testPatterns = {
  /**
   * Test pattern: Execute transition
   */
  async testExecuteTransition() {
    const result = await executeTransition("test_transition");

    // Assert success
    console.assert(result.success, "Transition should succeed");

    // Assert transition ID matches
    console.assert(result.transition_id === "test_transition", "Transition ID should match");

    // Assert active states updated
    console.assert(result.active_states.length > 0, "Should have active states");
  },

  /**
   * Test pattern: Navigate to state
   */
  async testNavigateToState() {
    const result = await navigateToState("target_state");

    // Assert success
    console.assert(result.success, "Navigation should succeed");

    // Assert path exists
    console.assert(result.path.length > 0, "Should have navigation path");

    // Assert target state is active
    console.assert(result.active_states.includes("target_state"), "Target state should be active");
  },

  /**
   * Test pattern: Multi-state navigation
   */
  async testMultiStateNavigation() {
    const targetStates = ["state1", "state2", "state3"];
    const result = await navigateToMultipleStates(targetStates);

    // Assert overall success
    console.assert(result.success, "Multi-state navigation should succeed");

    // Assert all states are active
    targetStates.forEach((state) => {
      console.assert(result.active_states.includes(state), `State ${state} should be active`);
    });

    // Assert individual results
    console.assert(
      result.results?.length === targetStates.length,
      "Should have result for each state",
    );
  },

  /**
   * Test pattern: Error handling
   */
  async testErrorHandling() {
    const result = await navigateToState("nonexistent_state");

    // Assert failure
    console.assert(!result.success, "Should fail for nonexistent state");

    // Assert error message exists
    console.assert(result.error !== undefined, "Should have error message");
  },
};
