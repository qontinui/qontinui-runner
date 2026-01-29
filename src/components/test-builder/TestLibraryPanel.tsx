/**
 * Test Library Panel
 *
 * Left panel showing the list of verification tests with search and filtering.
 */

import { useState, useCallback } from "react";
import {
  Search,
  Plus,
  FlaskConical,
  Globe,
  Eye,
  FileCode,
  FolderGit2,
  MoreVertical,
  Trash2,
  Copy,
  Upload,
  Download,
  Sparkles,
  Check,
  X,
} from "lucide-react";
import { useTestBuilder } from "./TestBuilderContext";
import { ImportExportDialog } from "./ImportExportDialog";
import { BatchDeleteDialog } from "../ui/BatchDeleteDialog";
import type { TestType, VerificationTest } from "./types";

// Test type icons
const testTypeIcons: Record<TestType, React.ElementType> = {
  playwright_cdp: Globe,
  qontinui_vision: Eye,
  python_script: FileCode,
  repository_test: FolderGit2,
};

// Test type labels
const testTypeLabels: Record<TestType, string> = {
  playwright_cdp: "Playwright CDP",
  qontinui_vision: "Vision",
  python_script: "Python Script",
  repository_test: "Repository Test",
};

interface TestListItemProps {
  test: VerificationTest;
  isSelected: boolean;
  isSelectionMode: boolean;
  isChecked: boolean;
  onSelect: () => void;
  onDelete: () => void;
  onDuplicate: () => void;
  onToggleSelection: () => void;
}

function TestListItem({ test, isSelected, isSelectionMode, isChecked, onSelect, onDelete, onDuplicate, onToggleSelection }: TestListItemProps) {
  const [showMenu, setShowMenu] = useState(false);
  const Icon = testTypeIcons[test.test_type];

  return (
    <div
      className={`
        group flex items-center gap-2 px-3 py-2 rounded-md cursor-pointer
        transition-colors
        ${
          isSelected
            ? "bg-primary/15 text-primary border-l-2 border-primary ml-[-1px]"
            : "hover:bg-muted/30"
        }
        ${isSelectionMode && isChecked ? "bg-red-500/10" : ""}
      `}
      onClick={isSelectionMode ? onToggleSelection : onSelect}
    >
      {isSelectionMode && (
        <div
          className={`w-4 h-4 rounded border flex-shrink-0 flex items-center justify-center ${
            isChecked
              ? "bg-red-500 border-red-500"
              : "border-neutral-500 hover:border-red-400"
          }`}
        >
          {isChecked && <Check className="w-3 h-3 text-white" />}
        </div>
      )}
      <Icon className="w-4 h-4 flex-shrink-0 text-muted-foreground" />
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium truncate">{test.name}</div>
        <div className="text-xs text-muted-foreground truncate">
          {testTypeLabels[test.test_type]}
        </div>
      </div>
      {!isSelectionMode && (
        <div className="relative">
          <button
            className="p-1 opacity-0 group-hover:opacity-100 hover:bg-muted rounded transition-opacity"
            onClick={(e) => {
              e.stopPropagation();
              setShowMenu(!showMenu);
            }}
          >
            <MoreVertical className="w-4 h-4" />
          </button>
          {showMenu && (
            <>
              <div className="fixed inset-0 z-10" onClick={() => setShowMenu(false)} />
              <div className="absolute right-0 top-full mt-1 z-20 bg-popover border border-border rounded-md shadow-lg py-1 min-w-[120px]">
                <button
                  className="w-full px-3 py-1.5 text-sm text-left hover:bg-muted flex items-center gap-2"
                  onClick={(e) => {
                    e.stopPropagation();
                    onDuplicate();
                    setShowMenu(false);
                  }}
                >
                  <Copy className="w-4 h-4" />
                  Duplicate
                </button>
                <button
                  className="w-full px-3 py-1.5 text-sm text-left hover:bg-muted flex items-center gap-2 text-destructive"
                  onClick={(e) => {
                    e.stopPropagation();
                    onDelete();
                    setShowMenu(false);
                  }}
                >
                  <Trash2 className="w-4 h-4" />
                  Delete
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}

interface TestLibraryPanelProps {
  /** Callback to switch to AI tab (optional - if not provided, AI button is disabled) */
  onOpenAiTab?: () => void;
}

export function TestLibraryPanel({ onOpenAiTab }: TestLibraryPanelProps) {
  const {
    state,
    filteredTests,
    selectTest,
    deleteTest,
    duplicateTest,
    setSearchQuery,
    setFilterType,
    loadTests,
    startNewTest,
    isDraftSelected,
    discardDraft,
  } = useTestBuilder();
  const [showNewTestMenu, setShowNewTestMenu] = useState(false);
  const [importExportMode, setImportExportMode] = useState<"import" | "export" | null>(null);

  // Bulk deletion state
  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  // Toggle selection for bulk delete
  const toggleSelection = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  // Exit selection mode
  const exitSelectionMode = useCallback(() => {
    setIsSelectionMode(false);
    setSelectedIds(new Set());
  }, []);

  // Delete selected tests
  const deleteSelectedTests = useCallback(async () => {
    setIsDeleting(true);
    try {
      for (const id of selectedIds) {
        await deleteTest(id);
      }
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
    } catch (error) {
      console.error("Failed to delete tests:", error);
    } finally {
      setIsDeleting(false);
    }
  }, [selectedIds, deleteTest, exitSelectionMode]);

  // Get names of selected tests for dialog
  const getSelectedNames = useCallback(() => {
    return state.tests
      .filter((t) => selectedIds.has(t.id))
      .map((t) => t.name);
  }, [state.tests, selectedIds]);

  const handleCreateTest = (testType: TestType) => {
    setShowNewTestMenu(false);
    // Start creating a new test as a draft (does not save to database)
    startNewTest(testType);
  };

  const handleDeleteTest = async (id: string) => {
    if (window.confirm("Are you sure you want to delete this test?")) {
      await deleteTest(id);
    }
  };

  const handleDuplicateTest = async (id: string) => {
    await duplicateTest(id);
  };

  // Group tests by type
  const groupedTests = filteredTests.reduce(
    (acc, test) => {
      if (!acc[test.test_type]) {
        acc[test.test_type] = [];
      }
      acc[test.test_type].push(test);
      return acc;
    },
    {} as Record<TestType, VerificationTest[]>,
  );

  return (
    <div className="h-full flex flex-col bg-card border-r border-border/50 w-64 flex-shrink-0">
      {/* Header */}
      <div className="p-4 border-b border-border/50">
        <div className="flex items-center justify-between mb-2">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <FlaskConical className="w-4 h-4" />
            Tests
          </h2>
        </div>

        {/* Action buttons */}
        <div className="flex items-center gap-1 mb-3">
          {onOpenAiTab ? (
            <button
              onClick={onOpenAiTab}
              className="flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg hover:bg-muted transition-colors text-blue-400 hover:text-blue-300 text-xs"
              title="Generate with AI"
              data-ui-id="test-builder-ai-btn"
            >
              <Sparkles className="w-3.5 h-3.5" />
              <span>AI</span>
            </button>
          ) : (
            <button
              disabled
              className="flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg text-xs text-muted-foreground cursor-not-allowed"
              title="AI generation coming soon"
              data-ui-id="test-builder-ai-btn"
            >
              <Sparkles className="w-3.5 h-3.5" />
              <span>AI</span>
            </button>
          )}
          <button
            onClick={() => setIsSelectionMode(!isSelectionMode)}
            className={`flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg transition-colors text-xs ${
              isSelectionMode
                ? "bg-red-500/20 text-red-400"
                : "hover:bg-muted text-muted-foreground hover:text-foreground"
            }`}
            title={isSelectionMode ? "Exit selection mode" : "Select tests to delete"}
            data-ui-id="test-builder-delete-mode-btn"
          >
            <Trash2 className="w-3.5 h-3.5" />
            <span>Delete</span>
          </button>
          <div className="relative flex-1">
            <button
              className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg hover:bg-muted transition-colors text-xs"
              onClick={() => setShowNewTestMenu(!showNewTestMenu)}
              title="Create new test"
              data-ui-id="test-builder-new-test-btn"
            >
              <Plus className="w-3.5 h-3.5" />
              <span>New</span>
            </button>
            {showNewTestMenu && (
              <>
                <div className="fixed inset-0 z-10" onClick={() => setShowNewTestMenu(false)} />
                <div className="absolute right-0 top-full mt-1 z-20 bg-popover border border-border rounded-md shadow-lg py-1 min-w-[180px]">
                  {(Object.keys(testTypeLabels) as TestType[]).map((type) => {
                    const Icon = testTypeIcons[type];
                    return (
                      <button
                        key={type}
                        className="w-full px-3 py-2 text-sm text-left hover:bg-muted flex items-center gap-2"
                        onClick={() => handleCreateTest(type)}
                      >
                        <Icon className="w-4 h-4" />
                        {testTypeLabels[type]}
                      </button>
                    );
                  })}
                </div>
              </>
            )}
          </div>
        </div>

        {/* Search */}
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search tests..."
            value={state.searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-8 pr-3 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-primary/50"
            data-ui-id="test-builder-search-input"
          />
        </div>

        {/* Filter */}
        <div className="mt-2">
          <select
            value={state.filterType}
            onChange={(e) => setFilterType(e.target.value as TestType | "all")}
            className="w-full px-2 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-primary/50"
            data-ui-id="test-builder-filter-type-select"
          >
            <option value="all">All Types</option>
            {(Object.keys(testTypeLabels) as TestType[]).map((type) => (
              <option key={type} value={type}>
                {testTypeLabels[type]}
              </option>
            ))}
          </select>
        </div>
      </div>

      {/* Selection Mode Header */}
      {isSelectionMode && (
        <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
          <span className="text-sm text-red-400">
            {selectedIds.size} selected
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={() => setSelectedIds(new Set(filteredTests.map(t => t.id)))}
              className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded"
              data-ui-id="test-builder-select-all-btn"
            >
              Select All
            </button>
            <button
              onClick={() => setSelectedIds(new Set())}
              disabled={selectedIds.size === 0}
              className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded disabled:opacity-50"
              data-ui-id="test-builder-clear-selection-btn"
            >
              Clear
            </button>
            <button
              onClick={() => setShowBatchDeleteDialog(true)}
              disabled={selectedIds.size === 0}
              className="px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-id="test-builder-delete-selected-btn"
            >
              Delete Selected
            </button>
            <button
              onClick={exitSelectionMode}
              className="p-1 text-neutral-400 hover:text-neutral-200"
              title="Cancel selection"
              data-ui-id="test-builder-cancel-selection-btn"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>
      )}

      {/* Draft Test Indicator */}
      {isDraftSelected && (
        <div className="mx-2 mb-2 p-2 bg-amber-900/30 border border-amber-700/50 rounded-md">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <div className="w-2 h-2 bg-amber-500 rounded-full animate-pulse" />
              <span className="text-xs text-amber-400 font-medium">Unsaved Draft</span>
            </div>
            <button
              onClick={discardDraft}
              className="text-xs text-amber-500 hover:text-amber-400 transition-colors"
              title="Discard draft"
            >
              Discard
            </button>
          </div>
        </div>
      )}

      {/* Test List */}
      <div className="flex-1 overflow-y-auto p-2 space-y-4">
        {state.isLoading ? (
          <div className="text-center py-8 text-muted-foreground text-sm">Loading tests...</div>
        ) : filteredTests.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground text-sm">
            {state.searchQuery || state.filterType !== "all"
              ? "No tests match your search"
              : "No tests yet. Click + to create one."}
          </div>
        ) : state.filterType === "all" ? (
          // Show grouped by type
          Object.entries(groupedTests).map(([type, tests]) => {
            const Icon = testTypeIcons[type as TestType];
            return (
              <div key={type}>
                <div className="flex items-center gap-2 px-2 py-1 text-xs font-semibold text-muted-foreground/70 uppercase">
                  <Icon className="w-3 h-3" />
                  {testTypeLabels[type as TestType]}
                  <span className="text-muted-foreground/50">({tests.length})</span>
                </div>
                <div className="space-y-0.5">
                  {tests.map((test) => (
                    <TestListItem
                      key={test.id}
                      test={test}
                      isSelected={state.selectedTestId === test.id}
                      isSelectionMode={isSelectionMode}
                      isChecked={selectedIds.has(test.id)}
                      onSelect={() => selectTest(test.id)}
                      onDelete={() => handleDeleteTest(test.id)}
                      onDuplicate={() => handleDuplicateTest(test.id)}
                      onToggleSelection={() => toggleSelection(test.id)}
                    />
                  ))}
                </div>
              </div>
            );
          })
        ) : (
          // Show flat list
          <div className="space-y-0.5">
            {filteredTests.map((test) => (
              <TestListItem
                key={test.id}
                test={test}
                isSelected={state.selectedTestId === test.id}
                isSelectionMode={isSelectionMode}
                isChecked={selectedIds.has(test.id)}
                onSelect={() => selectTest(test.id)}
                onDelete={() => handleDeleteTest(test.id)}
                onDuplicate={() => handleDuplicateTest(test.id)}
                onToggleSelection={() => toggleSelection(test.id)}
              />
            ))}
          </div>
        )}
      </div>

      {/* Footer with stats and import/export */}
      <div className="p-3 border-t border-border/50">
        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">
            {state.tests.length} test{state.tests.length !== 1 ? "s" : ""} total
          </span>
          <div className="flex items-center gap-1">
            <button
              onClick={() => setImportExportMode("import")}
              className="p-1.5 hover:bg-muted rounded-md transition-colors text-muted-foreground hover:text-foreground"
              title="Import tests from file"
              data-ui-id="test-builder-import-btn"
            >
              <Upload className="w-4 h-4" />
            </button>
            <button
              onClick={() => setImportExportMode("export")}
              className="p-1.5 hover:bg-muted rounded-md transition-colors text-muted-foreground hover:text-foreground"
              title="Export tests to file"
              disabled={state.tests.length === 0}
              data-ui-id="test-builder-export-btn"
            >
              <Download className="w-4 h-4" />
            </button>
          </div>
        </div>
      </div>

      {/* Import/Export Dialog */}
      {importExportMode && (
        <ImportExportDialog
          mode={importExportMode}
          tests={state.tests}
          selectedTestIds={state.selectedTestId ? [state.selectedTestId] : []}
          onClose={() => setImportExportMode(null)}
          onComplete={() => loadTests()}
        />
      )}

      {/* Batch Delete Dialog */}
      <BatchDeleteDialog
        open={showBatchDeleteDialog}
        title="Delete Tests"
        itemType="test"
        itemNames={getSelectedNames()}
        isDeleting={isDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelectedTests}
      />
    </div>
  );
}
