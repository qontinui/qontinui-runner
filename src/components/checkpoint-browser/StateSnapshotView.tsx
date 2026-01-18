/**
 * StateSnapshotView.tsx
 *
 * Displays detailed state snapshot information from a checkpoint.
 */

import { useState } from "react";
import {
  Hash,
  Clock,
  Database,
  Lightbulb,
  Shield,
  Bug,
  FileCode,
  ChevronDown,
  ChevronRight,
  CheckCircle,
  XCircle,
} from "lucide-react";
import type { Checkpoint, StateSnapshot } from "../../types/checkpoint";
import { formatDateTime, TRIGGER_TYPE_LABELS } from "../../types/checkpoint";

interface StateSnapshotViewProps {
  checkpoint: Checkpoint;
}

function CollapsibleSection({
  title,
  icon: Icon,
  count,
  children,
  defaultOpen = false,
}: {
  title: string;
  icon: React.ElementType;
  count: number;
  children: React.ReactNode;
  defaultOpen?: boolean;
}) {
  const [isOpen, setIsOpen] = useState(defaultOpen);

  return (
    <div className="border border-gray-700 rounded-lg overflow-hidden">
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="w-full px-4 py-3 flex items-center justify-between bg-gray-800 hover:bg-gray-750 transition-colors"
      >
        <div className="flex items-center gap-2">
          <Icon className="w-4 h-4 text-gray-400" />
          <span className="font-medium text-white">{title}</span>
          <span className="text-xs bg-gray-700 text-gray-300 px-2 py-0.5 rounded-full">
            {count}
          </span>
        </div>
        {isOpen ? (
          <ChevronDown className="w-4 h-4 text-gray-400" />
        ) : (
          <ChevronRight className="w-4 h-4 text-gray-400" />
        )}
      </button>
      {isOpen && <div className="p-4 bg-gray-900">{children}</div>}
    </div>
  );
}

export function StateSnapshotView({ checkpoint }: StateSnapshotViewProps) {
  const { state } = checkpoint;
  const triggerLabel = TRIGGER_TYPE_LABELS[checkpoint.trigger.type] || checkpoint.trigger.type;

  return (
    <div className="space-y-4">
      {/* Header Info */}
      <div className="bg-gray-800 rounded-lg p-4 space-y-3">
        {checkpoint.name && <h3 className="text-lg font-semibold text-white">{checkpoint.name}</h3>}
        {checkpoint.description && (
          <p className="text-sm text-gray-400">{checkpoint.description}</p>
        )}

        <div className="grid grid-cols-2 gap-4 text-sm">
          <div>
            <span className="text-gray-500">State</span>
            <p className="text-white font-medium">{state.state}</p>
          </div>
          <div>
            <span className="text-gray-500">Iteration</span>
            <p className="text-white font-medium">{state.iteration}</p>
          </div>
          <div>
            <span className="text-gray-500">Trigger</span>
            <p className="text-white font-medium">{triggerLabel}</p>
          </div>
          <div>
            <span className="text-gray-500">Created</span>
            <p className="text-white font-medium">{formatDateTime(checkpoint.created_at)}</p>
          </div>
        </div>

        {checkpoint.tags.length > 0 && (
          <div className="flex flex-wrap gap-1 pt-2">
            {checkpoint.tags.map((tag, idx) => (
              <span key={idx} className="text-xs bg-gray-700 text-gray-300 px-2 py-0.5 rounded">
                {tag}
              </span>
            ))}
          </div>
        )}
      </div>

      {/* Channels */}
      <CollapsibleSection
        title="Channels"
        icon={Database}
        count={Object.keys(state.channels).length}
        defaultOpen={true}
      >
        {Object.keys(state.channels).length === 0 ? (
          <p className="text-gray-500 text-sm">No channel data</p>
        ) : (
          <div className="space-y-2">
            {Object.entries(state.channels).map(([name, channel]) => (
              <div key={name} className="bg-gray-800 rounded p-3">
                <div className="flex items-center justify-between">
                  <span className="font-mono text-sm text-white">{name}</span>
                  <span className="text-xs text-gray-400">v{channel.version}</span>
                </div>
                <pre className="mt-2 text-xs text-gray-400 overflow-auto max-h-32">
                  {JSON.stringify(channel.value, null, 2)}
                </pre>
              </div>
            ))}
          </div>
        )}
      </CollapsibleSection>

      {/* Knowledge */}
      <CollapsibleSection title="Knowledge" icon={Lightbulb} count={state.knowledge.length}>
        {state.knowledge.length === 0 ? (
          <p className="text-gray-500 text-sm">No knowledge entries</p>
        ) : (
          <div className="space-y-2">
            {state.knowledge.map((entry) => (
              <div key={entry.id} className="bg-gray-800 rounded p-3">
                <div className="flex items-center gap-2">
                  <span className="text-xs bg-gray-700 text-gray-300 px-2 py-0.5 rounded">
                    {entry.category}
                  </span>
                  <span className="text-xs text-gray-400">Iter {entry.iteration}</span>
                </div>
                <p className="mt-2 text-sm text-gray-300">{entry.content}</p>
              </div>
            ))}
          </div>
        )}
      </CollapsibleSection>

      {/* Verification */}
      <CollapsibleSection
        title="Verification"
        icon={Shield}
        count={Object.keys(state.verification.criteria_results).length}
      >
        <div className="space-y-2">
          <div className="flex items-center gap-2 mb-3">
            {state.verification.overall_passed ? (
              <>
                <CheckCircle className="w-5 h-5 text-green-500" />
                <span className="text-green-400">All criteria passed</span>
              </>
            ) : (
              <>
                <XCircle className="w-5 h-5 text-red-500" />
                <span className="text-red-400">Verification pending/failed</span>
              </>
            )}
          </div>
          {Object.keys(state.verification.criteria_results).length === 0 ? (
            <p className="text-gray-500 text-sm">No verification results</p>
          ) : (
            Object.entries(state.verification.criteria_results).map(([id, result]) => (
              <div key={id} className="bg-gray-800 rounded p-3 flex items-start gap-3">
                {result.passed ? (
                  <CheckCircle className="w-4 h-4 text-green-500 flex-shrink-0 mt-0.5" />
                ) : (
                  <XCircle className="w-4 h-4 text-red-500 flex-shrink-0 mt-0.5" />
                )}
                <div>
                  <p className="text-sm text-white">{result.criterion_id}</p>
                  {result.reason && <p className="text-xs text-gray-400 mt-1">{result.reason}</p>}
                </div>
              </div>
            ))
          )}
        </div>
      </CollapsibleSection>

      {/* Findings */}
      <CollapsibleSection title="Findings" icon={Bug} count={state.findings.length}>
        {state.findings.length === 0 ? (
          <p className="text-gray-500 text-sm">No findings</p>
        ) : (
          <div className="space-y-2">
            {state.findings.map((finding) => (
              <div key={finding.id} className="bg-gray-800 rounded p-3">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    <span className="text-xs bg-gray-700 text-gray-300 px-2 py-0.5 rounded">
                      {finding.category}
                    </span>
                    <span
                      className={`text-xs px-2 py-0.5 rounded ${
                        finding.severity === "high" || finding.severity === "critical"
                          ? "bg-red-500/20 text-red-400"
                          : finding.severity === "medium"
                            ? "bg-yellow-500/20 text-yellow-400"
                            : "bg-gray-700 text-gray-300"
                      }`}
                    >
                      {finding.severity}
                    </span>
                  </div>
                  {finding.resolved && <span className="text-xs text-green-400">Resolved</span>}
                </div>
                <p className="mt-2 text-sm text-gray-300">{finding.description}</p>
              </div>
            ))}
          </div>
        )}
      </CollapsibleSection>

      {/* Files Modified */}
      <CollapsibleSection
        title="Files Modified"
        icon={FileCode}
        count={state.files_modified.length}
      >
        {state.files_modified.length === 0 ? (
          <p className="text-gray-500 text-sm">No files modified</p>
        ) : (
          <ul className="space-y-1">
            {state.files_modified.map((file, idx) => (
              <li key={idx} className="font-mono text-sm text-gray-300">
                {file}
              </li>
            ))}
          </ul>
        )}
      </CollapsibleSection>
    </div>
  );
}
