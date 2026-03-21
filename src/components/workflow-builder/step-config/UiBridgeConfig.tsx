import type { UnifiedStep } from "../../../types/unified-workflow";

export function UiBridgeConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "ui_bridge" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-3">
      <div>
        <label
          htmlFor="uibridge-action-select"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Action
        </label>
        <select
          id="uibridge-action-select"
          value={step.action || "snapshot"}
          onChange={(e) =>
            onUpdate({
              action: e.target.value as
                | "navigate"
                | "execute"
                | "assert"
                | "snapshot"
                | "snapshot_assert",
            })
          }
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
        >
          <option value="navigate">Navigate</option>
          <option value="execute">Execute Instruction</option>
          <option value="assert">Assert Condition</option>
          <option value="snapshot">Take Snapshot</option>
          <option value="snapshot_assert">Snapshot Assert (Batch)</option>
        </select>
      </div>

      {step.action === "navigate" && (
        <div>
          <label
            htmlFor="uibridge-url-input"
            className="block text-sm font-medium text-zinc-400 mb-1"
          >
            URL
          </label>
          <input
            id="uibridge-url-input"
            type="url"
            value={step.url || ""}
            onChange={(e) => onUpdate({ url: e.target.value })}
            placeholder="https://example.com"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
          />
          <p className="text-xs text-zinc-500 mt-1">The URL to navigate to</p>
        </div>
      )}

      {step.action === "execute" && (
        <div>
          <label
            htmlFor="uibridge-instruction-textarea"
            className="block text-sm font-medium text-zinc-400 mb-1"
          >
            Instruction
          </label>
          <textarea
            id="uibridge-instruction-textarea"
            value={step.instruction || ""}
            onChange={(e) => onUpdate({ instruction: e.target.value })}
            placeholder="Click the submit button, fill in the form..."
            rows={4}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50 resize-y"
          />
          <p className="text-xs text-zinc-500 mt-1">
            Natural language instruction for the UI Bridge to execute
          </p>
        </div>
      )}

      {step.action === "assert" && (
        <>
          <div>
            <label
              htmlFor="uibridge-target-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Target Element
            </label>
            <input
              id="uibridge-target-input"
              type="text"
              value={step.target || ""}
              onChange={(e) => onUpdate({ target: e.target.value })}
              placeholder='[data-testid="submit-btn"], .header-title, etc.'
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50 font-mono text-sm"
            />
            <p className="text-xs text-zinc-500 mt-1">CSS selector of the target element</p>
          </div>

          <div>
            <label
              htmlFor="uibridge-assert-type-select"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Assert Type
            </label>
            <select
              id="uibridge-assert-type-select"
              value={step.assert_type || "exists"}
              onChange={(e) =>
                onUpdate({
                  assert_type: e.target.value as
                    | "exists"
                    | "text_equals"
                    | "contains"
                    | "visible"
                    | "enabled",
                })
              }
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
            >
              <option value="exists">Exists</option>
              <option value="text_equals">Text Equals</option>
              <option value="contains">Contains</option>
              <option value="visible">Visible</option>
              <option value="enabled">Enabled</option>
            </select>
          </div>

          <div>
            <label
              htmlFor="uibridge-expected-input"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Expected Value
            </label>
            <input
              id="uibridge-expected-input"
              type="text"
              value={step.expected || ""}
              onChange={(e) => onUpdate({ expected: e.target.value })}
              placeholder="Expected text or value"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
            />
            <p className="text-xs text-zinc-500 mt-1">
              Expected value for text_equals and contains assertions
            </p>
          </div>
        </>
      )}

      {step.action === "snapshot_assert" && (
        <>
          <div>
            <label
              htmlFor="uibridge-assertions-textarea"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Assertions (JSON)
            </label>
            <textarea
              id="uibridge-assertions-textarea"
              value={step.target || "[]"}
              onChange={(e) => onUpdate({ target: e.target.value })}
              placeholder={`[\n  {\n    "id": "check-1",\n    "description": "Header exists",\n    "severity": "critical",\n    "assertionType": "exists",\n    "criteria": { "textContent": "Header" }\n  }\n]`}
              rows={8}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50 resize-y font-mono text-xs"
            />
            <p className="text-xs text-zinc-500 mt-1">
              JSON array of assertions. Each needs: id, description, severity, assertionType
              (exists/contains/visible/not_exists), criteria (search fields like textContent,
              testId, type).
            </p>
          </div>

          <div>
            <label
              htmlFor="uibridge-snapshot-target-select"
              className="block text-sm font-medium text-zinc-400 mb-1"
            >
              Snapshot Target
            </label>
            <select
              id="uibridge-snapshot-target-select"
              value={step.ui_bridge_snapshot_target || "control"}
              onChange={(e) => onUpdate({ ui_bridge_snapshot_target: e.target.value })}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
            >
              <option value="control">Control (Runner UI)</option>
              <option value="sdk">SDK (Connected App)</option>
            </select>
          </div>
        </>
      )}

      <div>
        <label
          htmlFor="uibridge-timeout-input"
          className="block text-sm font-medium text-zinc-400 mb-1"
        >
          Timeout (ms)
        </label>
        <input
          id="uibridge-timeout-input"
          type="number"
          value={step.timeout_ms ?? 5000}
          onChange={(e) => onUpdate({ timeout_ms: parseInt(e.target.value) || 5000 })}
          min={1000}
          max={60000}
          step={1000}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
        />
      </div>
    </div>
  );
}
