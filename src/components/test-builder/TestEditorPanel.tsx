/**
 * Test Editor Panel
 *
 * Center panel with Monaco code editor for editing test code.
 */

import { useRef, useState } from "react";
import Editor, { Monaco } from "@monaco-editor/react";
import { FileCode, FlaskConical, Copy, Check } from "lucide-react";
import { useTestBuilder } from "./TestBuilderContext";
import type { TestType } from "./types";
import { getStatusColors } from "@/design-system";

// Language mapping for Monaco
const testTypeLanguages: Record<TestType, string> = {
  playwright_cdp: "typescript",
  qontinui_vision: "json",
  python_script: "python",
  repository_test: "json",
};

// Default code templates for each test type
const defaultTemplates: Record<TestType, string> = {
  playwright_cdp: `// Playwright CDP Test
// The browser context and page are automatically available

import { expect } from '@playwright/test';

// Example: Check if an element is visible
await expect(page.locator('.welcome-message')).toBeVisible();

// Example: Check text content
await expect(page.locator('h1')).toHaveText('Welcome');

// Example: Check element count
await expect(page.locator('.list-item')).toHaveCount(5);

// Example: Check input value
await expect(page.locator('#username')).toHaveValue('test@example.com');
`,
  qontinui_vision: `{
  "assertions": [
    {
      "type": "element_within_bounds",
      "config": {
        "element_template": "button_template.png",
        "bounds": { "x": 0, "y": 0, "width": 1920, "height": 800 }
      }
    },
    {
      "type": "text_present",
      "config": {
        "text": "Success",
        "region": { "x": 100, "y": 100, "width": 500, "height": 200 }
      }
    }
  ],
  "threshold": 0.8
}
`,
  python_script: `"""
Python Verification Script

Use the helper functions:
- assertion(name, passed, message): Record a test assertion
- log(message): Log a message
- metric(name, value): Record a metric
"""

import json
from pathlib import Path

def verify():
    results = {
        "status": "passed",
        "assertions": [],
        "output": "",
        "metrics": {}
    }

    # Example: Check a log file
    log_file = Path(".dev-logs/app.log")
    if log_file.exists():
        content = log_file.read_text()

        # Check for success message
        success_found = "Operation completed" in content
        results["assertions"].append({
            "name": "success_message",
            "passed": success_found,
            "message": "Success message found in logs" if success_found else "Success message not found"
        })

        # Check for errors
        error_found = "ERROR" in content
        results["assertions"].append({
            "name": "no_errors",
            "passed": not error_found,
            "message": "No errors in logs" if not error_found else "Errors found in logs"
        })

        # Update overall status
        if not success_found or error_found:
            results["status"] = "failed"

        results["metrics"]["log_size"] = len(content)
    else:
        results["status"] = "error"
        results["output"] = "Log file not found"

    return results

if __name__ == "__main__":
    print(json.dumps(verify()))
`,
  repository_test: `{
  "command": "pytest tests/ -v --tb=short",
  "working_directory": "\${PROJECT_ROOT}",
  "parse_format": "pytest_json",
  "env_vars": {
    "PYTHONPATH": "\${PROJECT_ROOT}/src"
  }
}
`,
};

// Get the code content from a test based on its type
function getCodeFromTest(test: {
  test_type: TestType;
  playwright_code?: string;
  vision_config?: unknown;
  python_code?: string;
  repo_test_config?: unknown;
}): string {
  switch (test.test_type) {
    case "playwright_cdp":
      return test.playwright_code || defaultTemplates.playwright_cdp;
    case "qontinui_vision":
      return test.vision_config
        ? JSON.stringify(test.vision_config, null, 2)
        : defaultTemplates.qontinui_vision;
    case "python_script":
      return test.python_code || defaultTemplates.python_script;
    case "repository_test":
      return test.repo_test_config
        ? JSON.stringify(test.repo_test_config, null, 2)
        : defaultTemplates.repository_test;
  }
}

interface TestEditorPanelProps {
  onCodeChange: (code: string) => void;
  code: string;
}

export function TestEditorPanel({ onCodeChange, code }: TestEditorPanelProps) {
  const { selectedTest, setDirty } = useTestBuilder();
  const [copied, setCopied] = useState(false);
  const editorRef = useRef<unknown>(null);

  // Get the language for the editor
  const language = selectedTest ? testTypeLanguages[selectedTest.test_type] : "typescript";

  // Handle editor mount
  const handleEditorMount = (editor: unknown, _monaco: Monaco) => {
    editorRef.current = editor;
  };

  // Handle code changes
  const handleChange = (value: string | undefined) => {
    if (value !== undefined) {
      onCodeChange(value);
      setDirty(true);
    }
  };

  // Copy code to clipboard
  const handleCopy = async () => {
    await navigator.clipboard.writeText(code);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  // Show placeholder when no test selected
  if (!selectedTest) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground bg-muted/10">
        <FlaskConical className="w-16 h-16 mb-4 opacity-20" />
        <p className="text-lg font-medium">No Test Selected</p>
        <p className="text-sm mt-2 max-w-md text-center">
          Select a test from the library or create a new one to start editing.
        </p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Editor header */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-border/50 bg-muted/20">
        <div className="flex items-center gap-2">
          <FileCode className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">{selectedTest.name}</span>
          <span className="text-xs text-muted-foreground px-2 py-0.5 bg-muted rounded">
            {language}
          </span>
        </div>
        <button
          className="p-1.5 hover:bg-muted rounded-md transition-colors flex items-center gap-1 text-sm"
          onClick={handleCopy}
          data-ui-id="test-builder-copy-code-btn"
        >
          {copied ? (
            <Check className={`w-4 h-4 ${getStatusColors("success").icon}`} />
          ) : (
            <Copy className="w-4 h-4" />
          )}
          <span className="text-xs">{copied ? "Copied" : "Copy"}</span>
        </button>
      </div>

      {/* Monaco Editor */}
      <div className="flex-1 min-h-0">
        <Editor
          height="100%"
          language={language}
          value={code}
          onChange={handleChange}
          onMount={handleEditorMount}
          theme="vs-dark"
          options={{
            minimap: { enabled: false },
            fontSize: 13,
            lineNumbers: "on",
            scrollBeyondLastLine: false,
            automaticLayout: true,
            tabSize: 2,
            wordWrap: "on",
            formatOnPaste: true,
            formatOnType: true,
            padding: { top: 16, bottom: 16 },
          }}
        />
      </div>
    </div>
  );
}

// Export the helpers
export { getCodeFromTest, defaultTemplates };
