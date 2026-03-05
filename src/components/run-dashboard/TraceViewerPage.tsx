import React from "react";
import { TraceViewerWidget } from "../active-dashboard/widgets/trace-viewer";
import { useRunSelection } from "../../contexts/RunSelectionContext";

export const TraceViewerPage: React.FC = () => {
  const { selectedRunId } = useRunSelection();

  return (
    <div className="flex flex-col h-full p-4 gap-4">
      <div className="flex-1 min-h-0">
        <TraceViewerWidget executionId={selectedRunId} height={window.innerHeight - 140} />
      </div>
    </div>
  );
};
