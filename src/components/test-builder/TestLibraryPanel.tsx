/**
 * Test Library Panel
 *
 * Left panel showing the list of verification tests with search and filtering.
 */

import { useState } from "react";
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
} from "lucide-react";
import { useTestBuilder } from "./TestBuilderContext";
import { ImportExportDialog } from "./ImportExportDialog";
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
  onSelect: () => void;
  onDelete: () => void;
}

function TestListItem({ test, isSelected, onSelect, onDelete }: TestListItemProps) {
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
      `}
      onClick={onSelect}
    >
      <Icon className="w-4 h-4 flex-shrink-0 text-muted-foreground" />
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium truncate">{test.name}</div>
        <div className="text-xs text-muted-foreground truncate">
          {testTypeLabels[test.test_type]}
        </div>
      </div>
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
                  // TODO: Implement duplicate
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
    </div>
  );
}

export function TestLibraryPanel() {
  const {
    state,
    filteredTests,
    selectTest,
    createTest,
    deleteTest,
    setSearchQuery,
    setFilterType,
    loadTests,
  } = useTestBuilder();
  const [showNewTestMenu, setShowNewTestMenu] = useState(false);
  const [importExportMode, setImportExportMode] = useState<"import" | "export" | null>(null);

  const handleCreateTest = async (testType: TestType) => {
    setShowNewTestMenu(false);
    await createTest({
      name: `New ${testTypeLabels[testType]}`,
      test_type: testType,
      category:
        testType === "playwright_cdp"
          ? "dom"
          : testType === "qontinui_vision"
            ? "visual"
            : "custom",
    });
  };

  const handleDeleteTest = async (id: string) => {
    if (window.confirm("Are you sure you want to delete this test?")) {
      await deleteTest(id);
    }
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
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <FlaskConical className="w-4 h-4" />
            Tests
          </h2>
          <div className="relative">
            <button
              className="p-1.5 hover:bg-muted rounded-md transition-colors"
              onClick={() => setShowNewTestMenu(!showNewTestMenu)}
            >
              <Plus className="w-4 h-4" />
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
          />
        </div>

        {/* Filter */}
        <div className="mt-2">
          <select
            value={state.filterType}
            onChange={(e) => setFilterType(e.target.value as TestType | "all")}
            className="w-full px-2 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-primary/50"
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
                      onSelect={() => selectTest(test.id)}
                      onDelete={() => handleDeleteTest(test.id)}
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
                onSelect={() => selectTest(test.id)}
                onDelete={() => handleDeleteTest(test.id)}
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
            >
              <Upload className="w-4 h-4" />
            </button>
            <button
              onClick={() => setImportExportMode("export")}
              className="p-1.5 hover:bg-muted rounded-md transition-colors text-muted-foreground hover:text-foreground"
              title="Export tests to file"
              disabled={state.tests.length === 0}
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
    </div>
  );
}
