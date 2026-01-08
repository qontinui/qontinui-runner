/**
 * Test Builder Tab
 *
 * Main component for the verification test builder.
 * Provides a 3-panel layout for creating and editing tests:
 * - Left: Test library panel (list of tests)
 * - Center: Code editor (Monaco)
 * - Right: Properties panel (metadata and configuration)
 * - Bottom: Execution panel (run tests and view results)
 */

import { useState, useEffect, useCallback } from "react";
import { TestBuilderProvider, useTestBuilder } from "./TestBuilderContext";
import { TestLibraryPanel } from "./TestLibraryPanel";
import { TestEditorPanel, getCodeFromTest } from "./TestEditorPanel";
import { TestPropertiesPanel } from "./TestPropertiesPanel";
import { TestExecutionPanel } from "./TestExecutionPanel";

interface TestBuilderTabProps {
  onLog?: (level: string, message: string) => void;
}

function TestBuilderContent({ onLog }: TestBuilderTabProps) {
  const { selectedTest, state: _state } = useTestBuilder();

  // Track the code in local state for the editor
  const [code, setCode] = useState("");

  // Sync code with selected test
  useEffect(() => {
    if (selectedTest) {
      setCode(getCodeFromTest(selectedTest));
    } else {
      setCode("");
    }
  }, [selectedTest?.id, selectedTest?.test_type]);

  // Handle code changes from editor
  const handleCodeChange = useCallback((newCode: string) => {
    setCode(newCode);
  }, []);

  // Handle save completed
  const handleSave = useCallback(() => {
    if (onLog) {
      onLog("info", `Test saved: ${selectedTest?.name}`);
    }
  }, [selectedTest?.name, onLog]);

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Main content area */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* Left: Test Library */}
        <TestLibraryPanel />

        {/* Center: Code Editor */}
        <div className="flex-1 min-w-0 flex flex-col overflow-hidden">
          <div className="flex-1 min-h-0">
            <TestEditorPanel code={code} onCodeChange={handleCodeChange} />
          </div>
        </div>

        {/* Right: Properties */}
        <TestPropertiesPanel code={code} onSave={handleSave} />
      </div>

      {/* Bottom: Execution Panel */}
      <TestExecutionPanel />
    </div>
  );
}

export function TestBuilderTab({ onLog }: TestBuilderTabProps) {
  return (
    <TestBuilderProvider>
      <TestBuilderContent onLog={onLog} />
    </TestBuilderProvider>
  );
}
