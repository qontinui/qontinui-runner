/**
 * Test Builder Context
 *
 * Shared state management for the test builder components.
 * Supports draft tests that are only saved when explicitly requested.
 */

import { createContext, useContext, useReducer, useCallback, useEffect, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  VerificationTest,
  TestExecutionResult,
  CreateTestInput,
  TestType,
  CommandResponse,
  CollectedAnalysisSet,
  CollectedAnalysis,
} from "./types";

// Storage key for persisting draft test state
const DRAFT_TEST_STORAGE_KEY = "qontinui-test-builder-draft";

// Special ID for draft tests (not yet saved to database)
const DRAFT_TEST_ID = "__draft__";

// Draft test data shape (for localStorage persistence)
interface DraftTestData {
  input: CreateTestInput;
  code: string;
  createdAt: number;
}

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
  // Draft test state
  draftTest: DraftTestData | null;
  isCreatingNew: boolean;
  // Collected analyses for auto-populating test config
  collectedAnalyses: CollectedAnalysisSet | null;
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
  | { type: "DELETE_TEST"; id: string }
  | { type: "SET_DRAFT_TEST"; draft: DraftTestData | null }
  | { type: "SET_CREATING_NEW"; isCreating: boolean }
  | { type: "START_NEW_TEST"; draft: DraftTestData }
  | { type: "CLEAR_DRAFT" }
  | { type: "SET_COLLECTED_ANALYSES"; analyses: CollectedAnalysisSet | null };

// Load draft from localStorage
function loadDraftFromStorage(): DraftTestData | null {
  try {
    const saved = localStorage.getItem(DRAFT_TEST_STORAGE_KEY);
    if (saved) {
      return JSON.parse(saved);
    }
  } catch {
    // Ignore parse errors
  }
  return null;
}

// Save draft to localStorage
function saveDraftToStorage(draft: DraftTestData | null): void {
  try {
    if (draft) {
      localStorage.setItem(DRAFT_TEST_STORAGE_KEY, JSON.stringify(draft));
    } else {
      localStorage.removeItem(DRAFT_TEST_STORAGE_KEY);
    }
  } catch {
    // Ignore storage errors
  }
}

// Initial state - restore draft from localStorage if available
const savedDraft = loadDraftFromStorage();
const initialState: TestBuilderState = {
  tests: [],
  selectedTestId: savedDraft ? DRAFT_TEST_ID : null,
  isLoading: false,
  isExecuting: false,
  isSaving: false,
  error: null,
  lastResult: null,
  isDirty: false,
  searchQuery: "",
  filterType: "all",
  draftTest: savedDraft,
  isCreatingNew: savedDraft !== null,
  collectedAnalyses: null,
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
    case "SET_DRAFT_TEST":
      saveDraftToStorage(action.draft);
      return { ...state, draftTest: action.draft };
    case "SET_CREATING_NEW":
      return { ...state, isCreatingNew: action.isCreating };
    case "START_NEW_TEST":
      saveDraftToStorage(action.draft);
      return {
        ...state,
        draftTest: action.draft,
        isCreatingNew: true,
        selectedTestId: DRAFT_TEST_ID,
        isDirty: true,
      };
    case "CLEAR_DRAFT":
      saveDraftToStorage(null);
      return {
        ...state,
        draftTest: null,
        isCreatingNew: false,
        selectedTestId: null,
        isDirty: false,
        collectedAnalyses: null,
      };
    case "SET_COLLECTED_ANALYSES":
      return { ...state, collectedAnalyses: action.analyses };
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
  duplicateTest: (id: string) => Promise<VerificationTest | null>;
  executeTest: (id: string) => Promise<TestExecutionResult | null>;
  setSearchQuery: (query: string) => void;
  setFilterType: (filterType: TestType | "all") => void;
  setDirty: (dirty: boolean) => void;
  clearError: () => void;
  // Draft test functions
  startNewTest: (testType: TestType) => void;
  updateDraft: (input: Partial<CreateTestInput>, code?: string) => void;
  saveDraft: () => Promise<VerificationTest | null>;
  discardDraft: () => void;
  isDraftSelected: boolean;
  // Collected analyses functions
  setCollectedAnalyses: (analyses: CollectedAnalysisSet | null) => void;
}

// Create context
const TestBuilderContext = createContext<TestBuilderContextValue | null>(null);

// Provider component
export function TestBuilderProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(testBuilderReducer, initialState);

  // Check if draft is selected
  const isDraftSelected = state.selectedTestId === DRAFT_TEST_ID && state.draftTest !== null;

  // Get selected test - return virtual test object for draft
  const selectedTest: VerificationTest | null = (() => {
    if (isDraftSelected && state.draftTest) {
      // Create a virtual VerificationTest from draft data
      const draft = state.draftTest;
      return {
        id: DRAFT_TEST_ID,
        name: draft.input.name || "New Test",
        description: draft.input.description,
        test_type: draft.input.test_type,
        category: draft.input.category || "custom",
        playwright_code: draft.input.playwright_code,
        vision_config: draft.input.vision_config,
        python_code: draft.input.python_code,
        repo_test_config: draft.input.repo_test_config,
        success_criteria: draft.input.success_criteria,
        config: draft.input.config || {},
        timeout_seconds: draft.input.timeout_seconds || 60,
        is_critical: draft.input.is_critical ?? true,
        enabled: draft.input.enabled ?? true,
        tags: draft.input.tags || [],
        created_at: new Date(draft.createdAt).toISOString(),
        updated_at: new Date().toISOString(),
        ai_generated: false,
      };
    }
    return state.tests.find((t) => t.id === state.selectedTestId) || null;
  })();

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

  // Duplicate a test
  const duplicateTest = useCallback(
    async (id: string): Promise<VerificationTest | null> => {
      dispatch({ type: "SET_SAVING", saving: true });
      dispatch({ type: "SET_ERROR", error: null });

      try {
        // Find the test to duplicate
        const testToDuplicate = state.tests.find((t) => t.id === id);
        if (!testToDuplicate) {
          dispatch({ type: "SET_ERROR", error: "Test not found" });
          return null;
        }

        // Create a new test with copied data and a new name
        const duplicatedInput: CreateTestInput = {
          name: `${testToDuplicate.name} (Copy)`,
          description: testToDuplicate.description,
          test_type: testToDuplicate.test_type,
          category: testToDuplicate.category,
          playwright_code: testToDuplicate.playwright_code,
          vision_config: testToDuplicate.vision_config,
          python_code: testToDuplicate.python_code,
          repo_test_config: testToDuplicate.repo_test_config,
          success_criteria: testToDuplicate.success_criteria,
          config: testToDuplicate.config,
          timeout_seconds: testToDuplicate.timeout_seconds,
          is_critical: testToDuplicate.is_critical,
          enabled: testToDuplicate.enabled,
          tags: testToDuplicate.tags,
        };

        const response = await invoke<CommandResponse<VerificationTest>>(
          "create_verification_test",
          { input: duplicatedInput },
        );

        if (response.success && response.data) {
          dispatch({ type: "ADD_TEST", test: response.data });
          dispatch({ type: "SET_SELECTED_TEST", id: response.data.id });
          return response.data;
        } else {
          dispatch({ type: "SET_ERROR", error: response.message || "Failed to duplicate test" });
          return null;
        }
      } catch (err) {
        dispatch({ type: "SET_ERROR", error: String(err) });
        return null;
      } finally {
        dispatch({ type: "SET_SAVING", saving: false });
      }
    },
    [state.tests],
  );

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

  // Start creating a new test (creates draft, does not save)
  const startNewTest = useCallback((testType: TestType) => {
    const testTypeLabels: Record<TestType, string> = {
      playwright_cdp: "Playwright CDP",
      qontinui_vision: "Vision",
      python_script: "Python Script",
      repository_test: "Repository Test",
    };

    const draft: DraftTestData = {
      input: {
        name: `New ${testTypeLabels[testType]}`,
        test_type: testType,
        category:
          testType === "playwright_cdp"
            ? "dom"
            : testType === "qontinui_vision"
              ? "visual"
              : "custom",
      },
      code: "",
      createdAt: Date.now(),
    };
    dispatch({ type: "START_NEW_TEST", draft });
  }, []);

  // Update draft test data
  const updateDraft = useCallback(
    (input: Partial<CreateTestInput>, code?: string) => {
      if (!state.draftTest) return;

      const updatedDraft: DraftTestData = {
        ...state.draftTest,
        input: { ...state.draftTest.input, ...input },
        code: code !== undefined ? code : state.draftTest.code,
      };
      dispatch({ type: "SET_DRAFT_TEST", draft: updatedDraft });
      dispatch({ type: "SET_DIRTY", dirty: true });
    },
    [state.draftTest],
  );

  // Save draft test to database
  const saveDraft = useCallback(async (): Promise<VerificationTest | null> => {
    if (!state.draftTest) return null;

    dispatch({ type: "SET_SAVING", saving: true });
    dispatch({ type: "SET_ERROR", error: null });

    try {
      const input = { ...state.draftTest.input };

      // Auto-populate api_request_config from collected API request analysis
      if (state.collectedAnalyses && input.test_type === "python_script") {
        const apiAnalysis = state.collectedAnalyses.analyses.find(
          (a): a is CollectedAnalysis & { type: "api_request" } => a.type === "api_request",
        );
        if (apiAnalysis?.data.source_request_config) {
          // Only set if not already configured
          const existingConfig = input.config as Record<string, unknown> | undefined;
          if (!existingConfig?.api_request_config) {
            input.config = {
              ...existingConfig,
              api_request_config: apiAnalysis.data.source_request_config,
            };
          }
        }
      }

      const response = await invoke<CommandResponse<VerificationTest>>("create_verification_test", {
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
      });

      if (response.success && response.data) {
        dispatch({ type: "ADD_TEST", test: response.data });
        dispatch({ type: "CLEAR_DRAFT" });
        dispatch({ type: "SET_SELECTED_TEST", id: response.data.id });
        return response.data;
      } else {
        dispatch({ type: "SET_ERROR", error: response.message || "Failed to save test" });
        return null;
      }
    } catch (err) {
      dispatch({ type: "SET_ERROR", error: String(err) });
      return null;
    } finally {
      dispatch({ type: "SET_SAVING", saving: false });
    }
  }, [state.draftTest, state.collectedAnalyses]);

  // Discard draft test
  const discardDraft = useCallback(() => {
    dispatch({ type: "CLEAR_DRAFT" });
  }, []);

  // Set collected analyses (from PageAnalyzer)
  const setCollectedAnalyses = useCallback((analyses: CollectedAnalysisSet | null) => {
    dispatch({ type: "SET_COLLECTED_ANALYSES", analyses });
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
    duplicateTest,
    executeTest,
    setSearchQuery,
    setFilterType,
    setDirty,
    clearError,
    // Draft test functions
    startNewTest,
    updateDraft,
    saveDraft,
    discardDraft,
    isDraftSelected,
    // Collected analyses functions
    setCollectedAnalyses,
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
