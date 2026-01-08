/**
 * Test Builder Context
 *
 * Shared state management for the test builder components.
 */

import { createContext, useContext, useReducer, useCallback, useEffect, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  VerificationTest,
  TestExecutionResult,
  CreateTestInput,
  TestType,
  CommandResponse,
} from "./types";

// State shape
interface TestBuilderState {
  tests: VerificationTest[];
  selectedTestId: string | null;
  isLoading: boolean;
  isExecuting: boolean;
  isSaving: boolean;
  error: string | null;
  lastResult: TestExecutionResult | null;
  isDirty: boolean;
  searchQuery: string;
  filterType: TestType | "all";
}

// Actions
type TestBuilderAction =
  | { type: "SET_TESTS"; tests: VerificationTest[] }
  | { type: "SET_SELECTED_TEST"; id: string | null }
  | { type: "SET_LOADING"; loading: boolean }
  | { type: "SET_EXECUTING"; executing: boolean }
  | { type: "SET_SAVING"; saving: boolean }
  | { type: "SET_ERROR"; error: string | null }
  | { type: "SET_LAST_RESULT"; result: TestExecutionResult | null }
  | { type: "SET_DIRTY"; dirty: boolean }
  | { type: "SET_SEARCH_QUERY"; query: string }
  | { type: "SET_FILTER_TYPE"; filterType: TestType | "all" }
  | { type: "ADD_TEST"; test: VerificationTest }
  | { type: "UPDATE_TEST"; test: VerificationTest }
  | { type: "DELETE_TEST"; id: string };

// Initial state
const initialState: TestBuilderState = {
  tests: [],
  selectedTestId: null,
  isLoading: false,
  isExecuting: false,
  isSaving: false,
  error: null,
  lastResult: null,
  isDirty: false,
  searchQuery: "",
  filterType: "all",
};

// Reducer
function testBuilderReducer(state: TestBuilderState, action: TestBuilderAction): TestBuilderState {
  switch (action.type) {
    case "SET_TESTS":
      return { ...state, tests: action.tests };
    case "SET_SELECTED_TEST":
      return { ...state, selectedTestId: action.id, isDirty: false };
    case "SET_LOADING":
      return { ...state, isLoading: action.loading };
    case "SET_EXECUTING":
      return { ...state, isExecuting: action.executing };
    case "SET_SAVING":
      return { ...state, isSaving: action.saving };
    case "SET_ERROR":
      return { ...state, error: action.error };
    case "SET_LAST_RESULT":
      return { ...state, lastResult: action.result };
    case "SET_DIRTY":
      return { ...state, isDirty: action.dirty };
    case "SET_SEARCH_QUERY":
      return { ...state, searchQuery: action.query };
    case "SET_FILTER_TYPE":
      return { ...state, filterType: action.filterType };
    case "ADD_TEST":
      return { ...state, tests: [...state.tests, action.test] };
    case "UPDATE_TEST":
      return {
        ...state,
        tests: state.tests.map((t) => (t.id === action.test.id ? action.test : t)),
      };
    case "DELETE_TEST":
      return {
        ...state,
        tests: state.tests.filter((t) => t.id !== action.id),
        selectedTestId: state.selectedTestId === action.id ? null : state.selectedTestId,
      };
    default:
      return state;
  }
}

// Context value type
interface TestBuilderContextValue {
  state: TestBuilderState;
  selectedTest: VerificationTest | null;
  filteredTests: VerificationTest[];
  loadTests: () => Promise<void>;
  selectTest: (id: string | null) => void;
  createTest: (input: CreateTestInput) => Promise<VerificationTest | null>;
  updateTest: (id: string, input: CreateTestInput) => Promise<VerificationTest | null>;
  deleteTest: (id: string) => Promise<boolean>;
  executeTest: (id: string) => Promise<TestExecutionResult | null>;
  setSearchQuery: (query: string) => void;
  setFilterType: (filterType: TestType | "all") => void;
  setDirty: (dirty: boolean) => void;
  clearError: () => void;
}

// Create context
const TestBuilderContext = createContext<TestBuilderContextValue | null>(null);

// Provider component
export function TestBuilderProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(testBuilderReducer, initialState);

  // Get selected test
  const selectedTest = state.tests.find((t) => t.id === state.selectedTestId) || null;

  // Filter tests based on search and type
  const filteredTests = state.tests.filter((test) => {
    const matchesSearch =
      state.searchQuery === "" ||
      test.name.toLowerCase().includes(state.searchQuery.toLowerCase()) ||
      test.description?.toLowerCase().includes(state.searchQuery.toLowerCase());
    const matchesType = state.filterType === "all" || test.test_type === state.filterType;
    return matchesSearch && matchesType;
  });

  // Load tests from database
  const loadTests = useCallback(async () => {
    dispatch({ type: "SET_LOADING", loading: true });
    dispatch({ type: "SET_ERROR", error: null });

    try {
      const response = await invoke<CommandResponse<VerificationTest[]>>(
        "list_verification_tests",
        {
          enabledOnly: false,
        },
      );

      if (response.success && response.data) {
        dispatch({ type: "SET_TESTS", tests: response.data });
      } else {
        dispatch({ type: "SET_ERROR", error: response.message || "Failed to load tests" });
      }
    } catch (err) {
      dispatch({ type: "SET_ERROR", error: String(err) });
    } finally {
      dispatch({ type: "SET_LOADING", loading: false });
    }
  }, []);

  // Select a test
  const selectTest = useCallback((id: string | null) => {
    dispatch({ type: "SET_SELECTED_TEST", id });
  }, []);

  // Create a new test
  const createTest = useCallback(
    async (input: CreateTestInput): Promise<VerificationTest | null> => {
      dispatch({ type: "SET_SAVING", saving: true });
      dispatch({ type: "SET_ERROR", error: null });

      try {
        const response = await invoke<CommandResponse<VerificationTest>>(
          "create_verification_test",
          {
            input: {
              name: input.name,
              description: input.description,
              test_type: input.test_type,
              category: input.category,
              playwright_code: input.playwright_code,
              vision_config: input.vision_config,
              python_code: input.python_code,
              repo_test_config: input.repo_test_config,
              success_criteria: input.success_criteria,
              config: input.config || {},
              timeout_seconds: input.timeout_seconds || 60,
              is_critical: input.is_critical ?? true,
              enabled: input.enabled ?? true,
              tags: input.tags || [],
            },
          },
        );

        if (response.success && response.data) {
          dispatch({ type: "ADD_TEST", test: response.data });
          dispatch({ type: "SET_SELECTED_TEST", id: response.data.id });
          return response.data;
        } else {
          dispatch({ type: "SET_ERROR", error: response.message || "Failed to create test" });
          return null;
        }
      } catch (err) {
        dispatch({ type: "SET_ERROR", error: String(err) });
        return null;
      } finally {
        dispatch({ type: "SET_SAVING", saving: false });
      }
    },
    [],
  );

  // Update a test
  const updateTest = useCallback(
    async (id: string, input: CreateTestInput): Promise<VerificationTest | null> => {
      dispatch({ type: "SET_SAVING", saving: true });
      dispatch({ type: "SET_ERROR", error: null });

      try {
        const response = await invoke<CommandResponse<VerificationTest>>(
          "update_verification_test",
          {
            id,
            input: {
              name: input.name,
              description: input.description,
              test_type: input.test_type,
              category: input.category,
              playwright_code: input.playwright_code,
              vision_config: input.vision_config,
              python_code: input.python_code,
              repo_test_config: input.repo_test_config,
              success_criteria: input.success_criteria,
              config: input.config || {},
              timeout_seconds: input.timeout_seconds || 60,
              is_critical: input.is_critical ?? true,
              enabled: input.enabled ?? true,
              tags: input.tags || [],
            },
          },
        );

        if (response.success && response.data) {
          dispatch({ type: "UPDATE_TEST", test: response.data });
          dispatch({ type: "SET_DIRTY", dirty: false });
          return response.data;
        } else {
          dispatch({ type: "SET_ERROR", error: response.message || "Failed to update test" });
          return null;
        }
      } catch (err) {
        dispatch({ type: "SET_ERROR", error: String(err) });
        return null;
      } finally {
        dispatch({ type: "SET_SAVING", saving: false });
      }
    },
    [],
  );

  // Delete a test
  const deleteTest = useCallback(async (id: string): Promise<boolean> => {
    dispatch({ type: "SET_ERROR", error: null });

    try {
      const response = await invoke<CommandResponse>("delete_verification_test", { id });

      if (response.success) {
        dispatch({ type: "DELETE_TEST", id });
        return true;
      } else {
        dispatch({ type: "SET_ERROR", error: response.message || "Failed to delete test" });
        return false;
      }
    } catch (err) {
      dispatch({ type: "SET_ERROR", error: String(err) });
      return false;
    }
  }, []);

  // Execute a test
  const executeTest = useCallback(async (id: string): Promise<TestExecutionResult | null> => {
    dispatch({ type: "SET_EXECUTING", executing: true });
    dispatch({ type: "SET_ERROR", error: null });
    dispatch({ type: "SET_LAST_RESULT", result: null });

    try {
      const response = await invoke<CommandResponse<{ execution_result: TestExecutionResult }>>(
        "execute_test_by_id",
        { testId: id },
      );

      if (response.success && response.data) {
        dispatch({ type: "SET_LAST_RESULT", result: response.data.execution_result });
        return response.data.execution_result;
      } else {
        dispatch({ type: "SET_ERROR", error: response.message || "Test execution failed" });
        return null;
      }
    } catch (err) {
      dispatch({ type: "SET_ERROR", error: String(err) });
      return null;
    } finally {
      dispatch({ type: "SET_EXECUTING", executing: false });
    }
  }, []);

  // Set search query
  const setSearchQuery = useCallback((query: string) => {
    dispatch({ type: "SET_SEARCH_QUERY", query });
  }, []);

  // Set filter type
  const setFilterType = useCallback((filterType: TestType | "all") => {
    dispatch({ type: "SET_FILTER_TYPE", filterType });
  }, []);

  // Set dirty state
  const setDirty = useCallback((dirty: boolean) => {
    dispatch({ type: "SET_DIRTY", dirty });
  }, []);

  // Clear error
  const clearError = useCallback(() => {
    dispatch({ type: "SET_ERROR", error: null });
  }, []);

  // Load tests on mount
  useEffect(() => {
    loadTests();
  }, [loadTests]);

  const value: TestBuilderContextValue = {
    state,
    selectedTest,
    filteredTests,
    loadTests,
    selectTest,
    createTest,
    updateTest,
    deleteTest,
    executeTest,
    setSearchQuery,
    setFilterType,
    setDirty,
    clearError,
  };

  return <TestBuilderContext.Provider value={value}>{children}</TestBuilderContext.Provider>;
}

// Hook to use the context
export function useTestBuilder() {
  const context = useContext(TestBuilderContext);
  if (!context) {
    throw new Error("useTestBuilder must be used within a TestBuilderProvider");
  }
  return context;
}
