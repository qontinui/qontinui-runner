/**
 * useActionLogView Hook - Usage Examples
 *
 * This file demonstrates various ways to use the useActionLogView hook
 * in different scenarios.
 */

import React from "react";
import { useActionLogView } from "./useActionLogView";
import type { ActionLogEntry } from "../types/displayProfile";

/**
 * Example 1: Basic Usage
 * Simple component that displays action log data
 */
export function BasicActionLogExample() {
  const { viewData, loading, error, refresh } = useActionLogView();

  if (loading) {
    return <div>Loading actions...</div>;
  }

  if (error) {
    return (
      <div>
        <p>Error: {error}</p>
        <button onClick={refresh}>Retry</button>
      </div>
    );
  }

  if (!viewData) {
    return <div>No action data available</div>;
  }

  return (
    <div>
      <h2>Action Log</h2>
      <p>
        Showing {viewData.visible_count} of {viewData.total_count} actions
      </p>
      <button onClick={refresh}>Refresh</button>
      <div>
        {viewData.actions.map((action) => (
          <div key={action.id}>
            <strong>{action.action_type}</strong> - {action.status}
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * Example 2: Auto-Refresh During Execution
 * Automatically refreshes every second while execution is running
 */
export function AutoRefreshActionLogExample() {
  const [isExecuting, setIsExecuting] = React.useState(false);

  const { viewData, loading, error } = useActionLogView({
    autoRefreshInterval: isExecuting ? 1000 : 0, // Only refresh during execution
  });

  return (
    <div>
      <h2>Live Action Log</h2>
      <button onClick={() => setIsExecuting(!isExecuting)}>
        {isExecuting ? "Stop Auto-Refresh" : "Start Auto-Refresh"}
      </button>

      {loading && <p>Loading...</p>}
      {error && <p>Error: {error}</p>}

      {viewData && (
        <div>
          <p>Actions: {viewData.visible_count}</p>
          <ActionList actions={viewData.actions} />
        </div>
      )}
    </div>
  );
}

/**
 * Example 3: Manual Refresh with Confirmation
 * Shows confirmation before refreshing
 */
export function ManualRefreshExample() {
  const { viewData, loading, error, refresh } = useActionLogView({
    fetchOnMount: false, // Don't fetch on mount
  });

  const handleRefresh = async () => {
    if (window.confirm("Refresh action log data?")) {
      await refresh();
    }
  };

  return (
    <div>
      <button onClick={handleRefresh} disabled={loading}>
        {loading ? "Refreshing..." : "Refresh Data"}
      </button>

      {error && <p style={{ color: "red" }}>Error: {error}</p>}

      {viewData && (
        <div>
          <h3>Last Updated: {new Date().toLocaleTimeString()}</h3>
          <ActionList actions={viewData.actions} />
        </div>
      )}
    </div>
  );
}

/**
 * Example 4: Action Timeline with Relative Timestamps
 * Displays actions with relative timestamps from workflow start
 */
export function ActionTimelineExample() {
  const { viewData, loading, error } = useActionLogView({
    autoRefreshInterval: 500, // Refresh twice per second
  });

  if (loading && !viewData) {
    return <div>Loading timeline...</div>;
  }

  if (error) {
    return <div>Error loading timeline: {error}</div>;
  }

  if (!viewData) {
    return <div>No timeline data</div>;
  }

  const getRelativeTime = (timestamp: number): string => {
    if (!viewData.workflow_start_time) {
      return new Date(timestamp * 1000).toLocaleTimeString();
    }
    const relative = timestamp - viewData.workflow_start_time;
    return `+${relative.toFixed(2)}s`;
  };

  return (
    <div>
      <h2>Action Timeline</h2>
      {viewData.workflow_start_time && (
        <p>
          Started:{" "}
          {new Date(viewData.workflow_start_time * 1000).toLocaleTimeString()}
        </p>
      )}

      <div>
        {viewData.actions.map((action) => (
          <div key={action.id} style={{ marginBottom: "10px" }}>
            <div>
              <strong>{action.action_type}</strong>
              <span style={{ marginLeft: "10px", color: "#666" }}>
                {getRelativeTime(action.timestamp)}
              </span>
            </div>
            {action.duration && (
              <div style={{ fontSize: "0.9em", color: "#888" }}>
                Duration: {action.duration.toFixed(3)}s
              </div>
            )}
            <div>
              Status:{" "}
              <StatusBadge status={action.status} error={action.error} />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * Example 5: Filterable Action Log
 * Allows filtering by action type and status
 */
export function FilterableActionLogExample() {
  const { viewData, loading, error, refresh } = useActionLogView();
  const [filterType, setFilterType] = React.useState<string>("all");
  const [filterStatus, setFilterStatus] = React.useState<string>("all");

  const filteredActions = React.useMemo(() => {
    if (!viewData) return [];

    return viewData.actions.filter((action) => {
      const typeMatch = filterType === "all" || action.action_type === filterType;
      const statusMatch =
        filterStatus === "all" || action.status === filterStatus;
      return typeMatch && statusMatch;
    });
  }, [viewData, filterType, filterStatus]);

  const actionTypes = React.useMemo(() => {
    if (!viewData) return [];
    return Array.from(new Set(viewData.actions.map((a) => a.action_type)));
  }, [viewData]);

  return (
    <div>
      <h2>Filterable Action Log</h2>

      <div style={{ marginBottom: "10px" }}>
        <label>
          Type:
          <select value={filterType} onChange={(e) => setFilterType(e.target.value)}>
            <option value="all">All Types</option>
            {actionTypes.map((type) => (
              <option key={type} value={type}>
                {type}
              </option>
            ))}
          </select>
        </label>

        <label style={{ marginLeft: "10px" }}>
          Status:
          <select
            value={filterStatus}
            onChange={(e) => setFilterStatus(e.target.value)}
          >
            <option value="all">All Statuses</option>
            <option value="pending">Pending</option>
            <option value="running">Running</option>
            <option value="success">Success</option>
            <option value="failed">Failed</option>
          </select>
        </label>

        <button onClick={refresh} style={{ marginLeft: "10px" }}>
          Refresh
        </button>
      </div>

      {loading && <p>Loading...</p>}
      {error && <p>Error: {error}</p>}

      {viewData && (
        <div>
          <p>
            Showing {filteredActions.length} of {viewData.visible_count} actions
          </p>
          <ActionList actions={filteredActions} />
        </div>
      )}
    </div>
  );
}

/**
 * Helper Components
 */

function ActionList({ actions }: { actions: ActionLogEntry[] }) {
  if (actions.length === 0) {
    return <p>No actions to display</p>;
  }

  return (
    <div>
      {actions.map((action) => (
        <div
          key={action.id}
          style={{
            padding: "10px",
            margin: "5px 0",
            border: "1px solid #ddd",
            borderRadius: "4px",
          }}
        >
          <div>
            <strong>{action.action_type}</strong>
            {action.duration && (
              <span style={{ marginLeft: "10px", color: "#666" }}>
                ({action.duration.toFixed(3)}s)
              </span>
            )}
          </div>
          <div>
            <StatusBadge status={action.status} error={action.error} />
          </div>
          {action.error && (
            <div style={{ color: "red", fontSize: "0.9em", marginTop: "5px" }}>
              {action.error}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

function StatusBadge({
  status,
  error,
}: {
  status: string;
  error: string | null;
}) {
  const getStatusColor = () => {
    switch (status) {
      case "success":
        return "#4caf50";
      case "failed":
        return "#f44336";
      case "running":
        return "#2196f3";
      case "pending":
        return "#ff9800";
      default:
        return "#9e9e9e";
    }
  };

  return (
    <span
      style={{
        display: "inline-block",
        padding: "2px 8px",
        borderRadius: "3px",
        backgroundColor: getStatusColor(),
        color: "white",
        fontSize: "0.9em",
      }}
      title={error || undefined}
    >
      {status}
    </span>
  );
}
