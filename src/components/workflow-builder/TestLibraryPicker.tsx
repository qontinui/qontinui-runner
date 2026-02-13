/**
 * Test Library Picker
 *
 * Modal dialog for selecting a saved test from the Test Builder library
 * to add as a workflow step.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Search,
  X,
  Check as CheckIcon,
  TestTube2,
  Eye,
  Code,
  FileCode,
  Terminal,
} from "lucide-react";
import type { WorkflowPhase } from "../../types/unified-workflow";

// Test type from Test Builder
interface SavedTest {
  id: string;
  name: string;
  description?: string;
  test_type: "playwright_cdp" | "qontinui_vision" | "python_script" | "repository_test";
  category?: string;
  timeout_seconds: number;
  is_critical: boolean;
  enabled: boolean;
  tags: string[];
  created_at: string;
  updated_at: string;
}

interface TestLibraryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (test: SavedTest, phase: WorkflowPhase) => void;
  phase: WorkflowPhase;
}

const TEST_TYPE_ICONS: Record<string, typeof TestTube2> = {
  playwright_cdp: FileCode,
  qontinui_vision: Eye,
  python_script: Code,
  repository_test: Terminal,
};

const TEST_TYPE_COLORS: Record<string, string> = {
  playwright_cdp: "text-green-400",
  qontinui_vision: "text-purple-400",
  python_script: "text-blue-400",
  repository_test: "text-orange-400",
};

const TEST_TYPE_LABELS: Record<string, string> = {
  playwright_cdp: "Playwright",
  qontinui_vision: "Vision",
  python_script: "Python",
  repository_test: "Repository",
};

export function TestLibraryPicker({ isOpen, onClose, onSelect, phase }: TestLibraryPickerProps) {
  const [tests, setTests] = useState<SavedTest[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [filterType, setFilterType] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);

  // Fetch tests from database
  useEffect(() => {
    if (!isOpen) return;

    const fetchTests = async () => {
      setIsLoading(true);
      try {
        const response = await invoke<{
          success: boolean;
          data?: SavedTest[];
          message?: string;
        }>("list_verification_tests", { enabledOnly: false });

        if (response.success && response.data) {
          setTests(response.data);
        }
      } catch (err) {
        console.error("Failed to fetch tests:", err);
      } finally {
        setIsLoading(false);
      }
    };

    fetchTests();
  }, [isOpen]);

  // Reset selection when modal opens
  useEffect(() => {
    if (isOpen) {
      setSelectedId(null);
      setSearchQuery("");
      setFilterType(null);
    }
  }, [isOpen]);

  // Filter tests
  const filteredTests = tests.filter((test) => {
    const matchesSearch =
      !searchQuery ||
      test.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      test.description?.toLowerCase().includes(searchQuery.toLowerCase());

    const matchesType = !filterType || test.test_type === filterType;

    return matchesSearch && matchesType;
  });

  // Get unique test types for filter
  const testTypes = [...new Set(tests.map((t) => t.test_type))];

  const handleSelect = useCallback(() => {
    const selected = tests.find((t) => t.id === selectedId);
    if (selected) {
      onSelect(selected, phase);
      onClose();
    }
  }, [tests, selectedId, onSelect, phase, onClose]);

  const handleDoubleClick = useCallback(
    (test: SavedTest) => {
      onSelect(test, phase);
      onClose();
    },
    [onSelect, phase, onClose],
  );

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div
        data-ui-id="dialog-test-library-picker"
        className="bg-neutral-900 border border-neutral-700 rounded-lg shadow-2xl w-[700px] max-w-[90vw] max-h-[80vh] flex flex-col"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-neutral-700">
          <div>
            <h2 className="text-lg font-semibold">Select Test from Library</h2>
            <p className="text-sm text-muted-foreground">
              Choose a saved test to add as a {phase} step
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 hover:bg-neutral-700 rounded-md transition-colors"
            data-ui-id="workflow-builder-test-picker-close-btn"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search and Filter */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-neutral-800">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search tests..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-green-500/50"
              autoFocus
              data-ui-id="workflow-builder-test-picker-search-input"
            />
          </div>
          <select
            value={filterType || ""}
            onChange={(e) => setFilterType(e.target.value || null)}
            className="px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-green-500/50"
            data-ui-id="workflow-builder-test-picker-type-select"
          >
            <option value="">All Types</option>
            {testTypes.map((type) => (
              <option key={type} value={type}>
                {TEST_TYPE_LABELS[type] || type}
              </option>
            ))}
          </select>
        </div>

        {/* Test List */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading ? (
            <div className="text-center py-8 text-muted-foreground">Loading tests...</div>
          ) : filteredTests.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              {tests.length === 0
                ? "No tests saved yet. Create tests in the Test Builder first."
                : "No tests match your search."}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredTests.map((test) => {
                const Icon = TEST_TYPE_ICONS[test.test_type] || TestTube2;
                const colorClass = TEST_TYPE_COLORS[test.test_type] || "text-green-400";
                const isSelected = selectedId === test.id;

                return (
                  <button
                    key={test.id}
                    onClick={() => setSelectedId(test.id)}
                    onDoubleClick={() => handleDoubleClick(test)}
                    className={`
                      relative flex items-start gap-3 p-3 rounded-lg text-left transition-all
                      ${
                        isSelected
                          ? "bg-green-500/15 border-2 border-green-500"
                          : "bg-neutral-800/50 border-2 border-transparent hover:bg-neutral-800 hover:border-neutral-600"
                      }
                    `}
                    data-ui-id={`workflow-builder-test-picker-item-${test.id}`}
                  >
                    {isSelected && (
                      <div className="absolute top-2 right-2">
                        <CheckIcon className="w-4 h-4 text-green-400" />
                      </div>
                    )}
                    <div className={`p-2 rounded-md bg-neutral-900 ${colorClass}`}>
                      <Icon className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{test.name}</span>
                        {!test.enabled && (
                          <span className="text-xs px-1.5 py-0.5 bg-amber-500/20 text-amber-400 rounded">
                            disabled
                          </span>
                        )}
                        {test.is_critical && (
                          <span className="text-xs px-1.5 py-0.5 bg-red-500/20 text-red-400 rounded">
                            critical
                          </span>
                        )}
                      </div>
                      {test.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-1">
                          {test.description}
                        </p>
                      )}
                      <div className="flex items-center gap-3 mt-1.5 text-xs text-muted-foreground">
                        <span>{TEST_TYPE_LABELS[test.test_type] || test.test_type}</span>
                        {test.category && (
                          <>
                            <span className="text-neutral-600">|</span>
                            <span className="capitalize">{test.category}</span>
                          </>
                        )}
                        <span className="text-neutral-600">|</span>
                        <span>{test.timeout_seconds}s timeout</span>
                      </div>
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-neutral-700 bg-neutral-800/50">
          <span
            data-content-role="metric"
            data-content-label="available test count"
            className="text-sm text-muted-foreground"
          >
            {filteredTests.length} test{filteredTests.length !== 1 ? "s" : ""} available
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm rounded-md hover:bg-neutral-700 transition-colors"
              data-ui-id="workflow-builder-test-picker-cancel-btn"
            >
              Cancel
            </button>
            <button
              onClick={handleSelect}
              disabled={!selectedId}
              className="px-4 py-2 text-sm bg-green-600 hover:bg-green-700 text-white rounded-md
                       transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-id="workflow-builder-test-picker-add-btn"
            >
              Add to Workflow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
