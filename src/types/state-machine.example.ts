/**
 * State Machine API Usage Examples
 *
 * This file demonstrates how to use the state machine API in a React/TypeScript
 * application. These examples show common patterns and best practices.
 *
 * @module state-machine.example
 */

import {
  executeTransition,
  navigateToState,
  navigateToMultipleStates,
  getActiveStates,
  getAvailableTransitions,
  isStateActive,
  findTransitionById,
  getTransitionsToState,
  isAnyStateActive,
  areAllStatesActive,
} from "./state-machine";

// ============================================================================
// Example 1: Basic Transition Execution
// ============================================================================

async function example1_executeTransition() {
  console.log("=== Example 1: Execute Transition ===");

  const result = await executeTransition("login_to_dashboard");

  if (result.success) {
    console.log("Transition executed successfully!");
    console.log("Transition ID:", result.transition_id);
    console.log("Active states:", result.active_states);
  } else {
    console.error("Transition failed:", result.error);
  }
}

// ============================================================================
// Example 2: State Navigation
// ============================================================================

async function example2_navigateToState() {
  console.log("=== Example 2: Navigate to State ===");

  const result = await navigateToState("user_dashboard");

  if (result.success) {
    console.log("Navigation successful!");
    console.log("Path taken:", result.path.join(" -> "));
    console.log("Currently at states:", result.active_states);
  } else {
    console.error("Navigation failed:", result.error);
  }
}

// ============================================================================
// Example 3: Multi-State Navigation
// ============================================================================

async function example3_navigateToMultipleStates() {
  console.log("=== Example 3: Navigate to Multiple States ===");

  const targetStates = ["email_client_open", "calendar_open", "chat_open"];
  const result = await navigateToMultipleStates(targetStates);

  if (result.success) {
    console.log("Multi-state navigation successful!");
    console.log("All active states:", result.active_states);

    // Check individual results
    result.results?.forEach((stateResult, index) => {
      const stateName = targetStates[index];
      if (stateResult.success) {
        console.log(`✓ ${stateName}: Reached via ${stateResult.path.join(" -> ")}`);
      } else {
        console.log(`✗ ${stateName}: Failed - ${stateResult.error}`);
      }
    });
  } else {
    console.error("Multi-state navigation failed:", result.error);
  }
}

// ============================================================================
// Example 4: Querying Active States
// ============================================================================

async function example4_getActiveStates() {
  console.log("=== Example 4: Query Active States ===");

  const result = await getActiveStates();

  if (result.success) {
    console.log("Active states:", result.active_states);
    console.log("Current state:", result.current_state || "N/A");

    if (result.state_history && result.state_history.length > 0) {
      console.log("Recent history:");
      result.state_history.slice(0, 5).forEach((state, i) => {
        console.log(`  ${i + 1}. ${state}`);
      });
    }
  } else {
    console.error("Failed to get active states:", result.error);
  }
}

// ============================================================================
// Example 5: Querying Available Transitions
// ============================================================================

async function example5_getAvailableTransitions() {
  console.log("=== Example 5: Query Available Transitions ===");

  const result = await getAvailableTransitions();

  if (result.success) {
    console.log(`Current state: ${result.current_state || "None"}`);
    console.log(`Available transitions: ${result.transitions.length}`);

    result.transitions.forEach((transition) => {
      console.log(`\nTransition: ${transition.id}`);
      console.log(`  From: ${transition.from_state}`);
      console.log(`  To: ${transition.to_state || "Dynamic"}`);
      console.log(`  Workflows: ${transition.workflows.join(", ")}`);
    });
  } else {
    console.error("Failed to get transitions:", result.error);
  }
}

// ============================================================================
// Example 6: Using Utility Functions
// ============================================================================

async function example6_utilityFunctions() {
  console.log("=== Example 6: Utility Functions ===");

  const activeStates = await getActiveStates();

  // Check if a specific state is active
  if (isStateActive("dashboard", activeStates)) {
    console.log("Dashboard is currently active");
  }

  // Check if any of several states is active
  if (isAnyStateActive(["logged_in", "authenticated"], activeStates)) {
    console.log("User is authenticated");
  }

  // Check if all states are active
  if (areAllStatesActive(["email_open", "calendar_synced"], activeStates)) {
    console.log("Email is open and calendar is synced");
  }

  // Find transitions
  const transitions = await getAvailableTransitions();
  const loginTransition = findTransitionById("login", transitions);

  if (loginTransition) {
    console.log("Login transition found:", loginTransition.to_state);
  }

  // Get all transitions to a specific state
  const pathsToDashboard = getTransitionsToState("dashboard", transitions);
  console.log(`Found ${pathsToDashboard.length} ways to reach dashboard`);
}

// ============================================================================
// Example 7: React Component Integration
// ============================================================================

/**
 * Example React component that uses the state machine API.
 *
 * This shows how to integrate state machine commands into a React component
 * with proper error handling and loading states.
 */
/*
import { useState, useEffect } from "react";

function StateNavigator() {
  const [activeStates, setActiveStates] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load active states on mount
  useEffect(() => {
    loadActiveStates();
  }, []);

  const loadActiveStates = async () => {
    const result = await getActiveStates();
    if (result.success) {
      setActiveStates(result.active_states);
    } else {
      setError(result.error || "Failed to load states");
    }
  };

  const handleNavigate = async (stateId: string) => {
    setLoading(true);
    setError(null);

    const result = await navigateToState(stateId);

    if (result.success) {
      setActiveStates(result.active_states);
      console.log("Navigated via:", result.path.join(" -> "));
    } else {
      setError(result.error || "Navigation failed");
    }

    setLoading(false);
  };

  const handleExecuteTransition = async (transitionId: string) => {
    setLoading(true);
    setError(null);

    const result = await executeTransition(transitionId);

    if (result.success) {
      setActiveStates(result.active_states);
    } else {
      setError(result.error || "Transition failed");
    }

    setLoading(false);
  };

  return (
    <div>
      <h2>State Navigator</h2>

      <div>
        <h3>Active States</h3>
        <ul>
          {activeStates.map((state) => (
            <li key={state}>{state}</li>
          ))}
        </ul>
      </div>

      {error && <div className="error">{error}</div>}

      <div>
        <button
          onClick={() => handleNavigate("dashboard")}
          disabled={loading}
        >
          Go to Dashboard
        </button>
        <button
          onClick={() => handleExecuteTransition("logout")}
          disabled={loading}
        >
          Execute Logout
        </button>
      </div>

      {loading && <div>Processing...</div>}
    </div>
  );
}
*/

// ============================================================================
// Example 8: Error Handling Patterns
// ============================================================================

async function example8_errorHandling() {
  console.log("=== Example 8: Error Handling ===");

  // Pattern 1: Basic error check
  const result1 = await navigateToState("nonexistent_state");
  if (!result1.success) {
    console.error("Navigation failed:", result1.error);
    // Handle error (show notification, rollback, etc.)
  }

  // Pattern 2: Try-catch with fallback
  try {
    const result2 = await executeTransition("critical_transition");
    if (!result2.success) {
      throw new Error(result2.error || "Unknown error");
    }
    // Success path
    console.log("Transition successful");
  } catch (error) {
    console.error("Error executing transition:", error);
    // Fallback logic
    await navigateToState("safe_state");
  }

  // Pattern 3: Validation before execution
  const transitions = await getAvailableTransitions();
  const targetTransition = findTransitionById("desired_transition", transitions);

  if (targetTransition) {
    await executeTransition(targetTransition.id);
  } else {
    console.warn("Desired transition not available from current state");
    // Show user message or disable UI
  }
}

// ============================================================================
// Example 9: Complex Workflow
// ============================================================================

async function example9_complexWorkflow() {
  console.log("=== Example 9: Complex Workflow ===");

  // Step 1: Check current state
  const currentState = await getActiveStates();
  console.log("Starting from states:", currentState.active_states);

  // Step 2: Navigate to initial state if needed
  if (!isStateActive("logged_in", currentState)) {
    console.log("Not logged in, navigating to login...");
    const loginResult = await navigateToState("login_page");

    if (!loginResult.success) {
      console.error("Failed to reach login page");
      return;
    }
  }

  // Step 3: Execute login transition
  console.log("Executing login transition...");
  const loginTransition = await executeTransition("perform_login");

  if (!loginTransition.success) {
    console.error("Login failed:", loginTransition.error);
    return;
  }

  // Step 4: Navigate to multiple states in parallel
  console.log("Opening multiple applications...");
  const multiNav = await navigateToMultipleStates(["email_client", "calendar", "tasks"]);

  if (multiNav.success) {
    console.log("All applications opened successfully!");
    console.log("Final state:", multiNav.active_states);
  } else {
    console.error("Some applications failed to open:", multiNav.error);

    // Check which ones succeeded
    multiNav.results?.forEach((result, i) => {
      const appNames = ["email_client", "calendar", "tasks"];
      console.log(`${appNames[i]}: ${result.success ? "OK" : "FAILED"}`);
    });
  }

  // Step 5: Verify final state
  const finalState = await getActiveStates();
  const requiredStates = ["logged_in", "email_client", "calendar"];

  if (areAllStatesActive(requiredStates, finalState)) {
    console.log("Workflow complete! All required states are active.");
  } else {
    console.warn("Workflow incomplete. Missing some required states.");
  }
}

// ============================================================================
// Example 10: State Monitoring Hook (React)
// ============================================================================

/**
 * Custom React hook for monitoring state machine state.
 *
 * This hook provides a convenient way to track state machine status
 * and receive updates when states change.
 */
/*
import { useState, useEffect, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";

function useStateMachine() {
  const [activeStates, setActiveStates] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load initial state
  useEffect(() => {
    loadStates();
  }, []);

  // Listen for state change events
  useEffect(() => {
    let unlisten: (() => void) | undefined;

    const setupListener = async () => {
      unlisten = await listen("state_changed", (event: any) => {
        console.log("State changed:", event.payload);
        loadStates();
      });
    };

    setupListener();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const loadStates = async () => {
    const result = await getActiveStates();
    if (result.success) {
      setActiveStates(result.active_states);
      setError(null);
    } else {
      setError(result.error || "Failed to load states");
    }
  };

  const navigate = useCallback(async (stateId: string) => {
    setLoading(true);
    setError(null);

    const result = await navigateToState(stateId);

    if (result.success) {
      setActiveStates(result.active_states);
    } else {
      setError(result.error || "Navigation failed");
    }

    setLoading(false);
    return result;
  }, []);

  const executeTransitionCommand = useCallback(async (transitionId: string) => {
    setLoading(true);
    setError(null);

    const result = await executeTransition(transitionId);

    if (result.success) {
      setActiveStates(result.active_states);
    } else {
      setError(result.error || "Transition failed");
    }

    setLoading(false);
    return result;
  }, []);

  return {
    activeStates,
    loading,
    error,
    navigate,
    executeTransition: executeTransitionCommand,
    refresh: loadStates,
    isActive: (stateId: string) => activeStates.includes(stateId),
  };
}

// Usage in a component:
function MyComponent() {
  const { activeStates, loading, error, navigate, isActive } = useStateMachine();

  return (
    <div>
      <h2>Current States</h2>
      <ul>
        {activeStates.map((state) => (
          <li key={state}>{state}</li>
        ))}
      </ul>

      {isActive("dashboard") && <div>Dashboard is active!</div>}

      <button onClick={() => navigate("settings")} disabled={loading}>
        Go to Settings
      </button>

      {error && <div className="error">{error}</div>}
    </div>
  );
}
*/

// ============================================================================
// Run Examples
// ============================================================================

export async function runAllExamples() {
  console.log("\n\n========================================");
  console.log("STATE MACHINE API EXAMPLES");
  console.log("========================================\n");

  await example1_executeTransition();
  console.log("\n");

  await example2_navigateToState();
  console.log("\n");

  await example3_navigateToMultipleStates();
  console.log("\n");

  await example4_getActiveStates();
  console.log("\n");

  await example5_getAvailableTransitions();
  console.log("\n");

  await example6_utilityFunctions();
  console.log("\n");

  await example8_errorHandling();
  console.log("\n");

  await example9_complexWorkflow();
  console.log("\n");

  console.log("========================================");
  console.log("ALL EXAMPLES COMPLETE");
  console.log("========================================\n");
}

// Uncomment to run examples:
// runAllExamples().catch(console.error);
