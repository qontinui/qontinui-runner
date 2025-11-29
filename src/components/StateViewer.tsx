/**
 * StateViewer Component
 *
 * A React component for viewing and managing detected states from state discovery.
 * Displays a list of detected states with detailed information about their constituent
 * elements (StateImages, StateRegions, StateLocations).
 *
 * Features:
 * - State list with selection and highlighting
 * - Detailed state information view
 * - Statistics display (images, regions, locations count)
 * - Edit and export action buttons
 * - Responsive layout with accessible markup
 */

import React, { useState } from "react";

// ============================================================================
// Type Definitions
// ============================================================================

/**
 * Represents a StateImage - a visual element within a state
 */
export interface StateImage {
  id: string;
  name: string;
  x: number;
  y: number;
  x2: number;
  y2: number;
  width: number;
  height: number;
  pixelHash: string;
  stabilityScore: number;
  screenshots: string[];
  createdAt: string;
  tags?: string[];
  darkPixelPercentage?: number;
  lightPixelPercentage?: number;
  avgBrightness?: number;
  maskDensity?: number;
  hasMask?: boolean;
}

/**
 * Represents a StateRegion - a defined rectangular area in a state
 */
export interface StateRegion {
  id: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  isSearchRegion?: boolean;
}

/**
 * Represents a StateLocation - a specific point in a state
 */
export interface StateLocation {
  id: string;
  name: string;
  x: number;
  y: number;
  fixed: boolean;
  anchor: boolean;
}

/**
 * Represents a detected state from state discovery
 */
export interface DetectedState {
  id: string;
  name: string;
  stateImages: StateImage[];
  stateRegions: StateRegion[];
  stateLocations: StateLocation[];
  screenshotIds: string[];
  confidence: number;
  metadata?: Record<string, any>;
}

/**
 * Props for the StateViewer component
 */
interface StateViewerProps {
  /** List of detected states to display */
  states: DetectedState[];

  /** Callback fired when user clicks edit button for a state */
  onStateEdit?: (state: DetectedState) => void;

  /** Callback fired when user clicks export button */
  onExport?: (states: DetectedState[]) => void;
}

// ============================================================================
// Component
// ============================================================================

/**
 * StateViewer Component
 *
 * Displays a list of detected states with selection and detailed view capabilities.
 */
export const StateViewer: React.FC<StateViewerProps> = ({ states, onStateEdit, onExport }) => {
  const [selectedStateId, setSelectedStateId] = useState<string | null>(null);

  // Get the currently selected state
  const selectedState = states.find((state) => state.id === selectedStateId);

  // Calculate statistics for a state
  const getStateStats = (state: DetectedState) => ({
    images: state.stateImages.length,
    regions: state.stateRegions.length,
    locations: state.stateLocations.length,
    screenshots: state.screenshotIds.length,
  });

  // Handle state selection
  const handleStateClick = (stateId: string) => {
    setSelectedStateId(stateId === selectedStateId ? null : stateId);
  };

  // Handle edit action
  const handleEdit = () => {
    if (selectedState && onStateEdit) {
      onStateEdit(selectedState);
    }
  };

  // Handle export action
  const handleExport = () => {
    if (onExport) {
      onExport(states);
    }
  };

  return (
    <div className="state-viewer">
      <div className="state-viewer-header">
        <h2 className="state-viewer-title">Detected States</h2>
        <div className="state-viewer-actions">
          <button
            className="btn-export"
            onClick={handleExport}
            disabled={states.length === 0}
            aria-label="Export states"
          >
            Export All
          </button>
        </div>
      </div>

      <div className="state-viewer-content">
        {/* State List */}
        <div className="state-list">
          <div className="state-list-header">
            <h3>States ({states.length})</h3>
          </div>

          <div className="state-list-items">
            {states.length === 0 ? (
              <div className="state-list-empty">
                <p>No states detected yet.</p>
              </div>
            ) : (
              states.map((state) => {
                const stats = getStateStats(state);
                const isSelected = state.id === selectedStateId;

                return (
                  <div
                    key={state.id}
                    className={`state-list-item ${isSelected ? "selected" : ""}`}
                    onClick={() => handleStateClick(state.id)}
                    role="button"
                    tabIndex={0}
                    aria-pressed={isSelected}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        handleStateClick(state.id);
                      }
                    }}
                  >
                    <div className="state-list-item-header">
                      <span className="state-name">{state.name}</span>
                      <span className="state-confidence">
                        {(state.confidence * 100).toFixed(1)}%
                      </span>
                    </div>
                    <div className="state-list-item-stats">
                      <span className="stat-badge" title="StateImages">
                        Images: {stats.images}
                      </span>
                      <span className="stat-badge" title="StateRegions">
                        Regions: {stats.regions}
                      </span>
                      <span className="stat-badge" title="StateLocations">
                        Locations: {stats.locations}
                      </span>
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </div>

        {/* State Details */}
        <div className="state-details">
          {selectedState ? (
            <>
              <div className="state-details-header">
                <h3>{selectedState.name}</h3>
                <div className="state-details-actions">
                  <button
                    className="btn-edit"
                    onClick={handleEdit}
                    disabled={!onStateEdit}
                    aria-label={`Edit ${selectedState.name}`}
                  >
                    Edit
                  </button>
                </div>
              </div>

              <div className="state-details-content">
                {/* State Info */}
                <div className="state-info-section">
                  <h4>State Information</h4>
                  <dl className="state-info-grid">
                    <dt>ID:</dt>
                    <dd>{selectedState.id}</dd>
                    <dt>Confidence:</dt>
                    <dd>{(selectedState.confidence * 100).toFixed(1)}%</dd>
                    <dt>Screenshots:</dt>
                    <dd>{selectedState.screenshotIds.length}</dd>
                  </dl>
                </div>

                {/* StateImages */}
                <div className="state-section">
                  <h4>StateImages ({selectedState.stateImages.length})</h4>
                  {selectedState.stateImages.length === 0 ? (
                    <p className="empty-message">No StateImages</p>
                  ) : (
                    <ul className="element-list">
                      {selectedState.stateImages.map((image) => (
                        <li key={image.id} className="element-item">
                          <div className="element-name">{image.name}</div>
                          <div className="element-details">
                            <span>
                              Position: ({image.x}, {image.y})
                            </span>
                            <span>
                              Size: {image.width}x{image.height}
                            </span>
                            <span>Stability: {(image.stabilityScore * 100).toFixed(1)}%</span>
                          </div>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>

                {/* StateRegions */}
                <div className="state-section">
                  <h4>StateRegions ({selectedState.stateRegions.length})</h4>
                  {selectedState.stateRegions.length === 0 ? (
                    <p className="empty-message">No StateRegions</p>
                  ) : (
                    <ul className="element-list">
                      {selectedState.stateRegions.map((region) => (
                        <li key={region.id} className="element-item">
                          <div className="element-name">{region.name}</div>
                          <div className="element-details">
                            <span>
                              Position: ({region.x}, {region.y})
                            </span>
                            <span>
                              Size: {region.width}x{region.height}
                            </span>
                            {region.isSearchRegion && (
                              <span className="badge-search">Search Region</span>
                            )}
                          </div>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>

                {/* StateLocations */}
                <div className="state-section">
                  <h4>StateLocations ({selectedState.stateLocations.length})</h4>
                  {selectedState.stateLocations.length === 0 ? (
                    <p className="empty-message">No StateLocations</p>
                  ) : (
                    <ul className="element-list">
                      {selectedState.stateLocations.map((location) => (
                        <li key={location.id} className="element-item">
                          <div className="element-name">{location.name}</div>
                          <div className="element-details">
                            <span>
                              Position: ({location.x}, {location.y})
                            </span>
                            {location.fixed && <span className="badge-fixed">Fixed</span>}
                            {location.anchor && <span className="badge-anchor">Anchor</span>}
                          </div>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>

                {/* Metadata */}
                {selectedState.metadata && Object.keys(selectedState.metadata).length > 0 && (
                  <div className="state-section">
                    <h4>Metadata</h4>
                    <pre className="metadata-content">
                      {JSON.stringify(selectedState.metadata, null, 2)}
                    </pre>
                  </div>
                )}
              </div>
            </>
          ) : (
            <div className="state-details-empty">
              <p>Select a state to view details</p>
            </div>
          )}
        </div>
      </div>

      <style>{`
        .state-viewer {
          display: flex;
          flex-direction: column;
          height: 100%;
          background: var(--background);
          color: var(--foreground);
        }

        .state-viewer-header {
          display: flex;
          justify-content: space-between;
          align-items: center;
          padding: 1rem 1.5rem;
          border-bottom: 1px solid rgba(255, 255, 255, 0.1);
        }

        .state-viewer-title {
          font-size: 1.5rem;
          font-weight: 600;
          margin: 0;
        }

        .state-viewer-actions {
          display: flex;
          gap: 0.5rem;
        }

        .state-viewer-content {
          display: grid;
          grid-template-columns: 350px 1fr;
          gap: 1px;
          flex: 1;
          overflow: hidden;
          background: rgba(255, 255, 255, 0.05);
        }

        /* State List Styles */
        .state-list {
          display: flex;
          flex-direction: column;
          background: hsl(var(--card));
          overflow: hidden;
        }

        .state-list-header {
          padding: 1rem;
          border-bottom: 1px solid rgba(255, 255, 255, 0.1);
        }

        .state-list-header h3 {
          margin: 0;
          font-size: 1rem;
          font-weight: 600;
          color: hsl(var(--primary));
        }

        .state-list-items {
          flex: 1;
          overflow-y: auto;
          padding: 0.5rem;
        }

        .state-list-empty {
          padding: 2rem;
          text-align: center;
          color: hsl(var(--muted-foreground));
        }

        .state-list-item {
          padding: 0.75rem;
          margin-bottom: 0.5rem;
          border-radius: 0.375rem;
          background: rgba(255, 255, 255, 0.02);
          border: 1px solid rgba(255, 255, 255, 0.05);
          cursor: pointer;
          transition: all 0.2s;
        }

        .state-list-item:hover {
          background: rgba(255, 255, 255, 0.05);
          border-color: rgba(255, 255, 255, 0.1);
        }

        .state-list-item.selected {
          background: hsl(var(--primary) / 0.15);
          border-color: hsl(var(--primary) / 0.5);
        }

        .state-list-item-header {
          display: flex;
          justify-content: space-between;
          align-items: center;
          margin-bottom: 0.5rem;
        }

        .state-name {
          font-weight: 500;
          font-size: 0.95rem;
        }

        .state-confidence {
          font-size: 0.85rem;
          color: hsl(var(--accent));
          font-weight: 600;
        }

        .state-list-item-stats {
          display: flex;
          gap: 0.5rem;
          flex-wrap: wrap;
        }

        .stat-badge {
          font-size: 0.75rem;
          padding: 0.125rem 0.5rem;
          border-radius: 0.25rem;
          background: rgba(255, 255, 255, 0.1);
          color: hsl(var(--muted-foreground));
        }

        /* State Details Styles */
        .state-details {
          display: flex;
          flex-direction: column;
          background: hsl(var(--card));
          overflow: hidden;
        }

        .state-details-header {
          display: flex;
          justify-content: space-between;
          align-items: center;
          padding: 1rem 1.5rem;
          border-bottom: 1px solid rgba(255, 255, 255, 0.1);
        }

        .state-details-header h3 {
          margin: 0;
          font-size: 1.25rem;
          font-weight: 600;
        }

        .state-details-actions {
          display: flex;
          gap: 0.5rem;
        }

        .state-details-content {
          flex: 1;
          overflow-y: auto;
          padding: 1.5rem;
        }

        .state-details-empty {
          display: flex;
          align-items: center;
          justify-content: center;
          height: 100%;
          color: hsl(var(--muted-foreground));
        }

        /* Section Styles */
        .state-info-section,
        .state-section {
          margin-bottom: 2rem;
        }

        .state-info-section h4,
        .state-section h4 {
          margin: 0 0 1rem 0;
          font-size: 1rem;
          font-weight: 600;
          color: hsl(var(--primary));
        }

        .state-info-grid {
          display: grid;
          grid-template-columns: auto 1fr;
          gap: 0.5rem 1rem;
          margin: 0;
        }

        .state-info-grid dt {
          font-weight: 500;
          color: hsl(var(--muted-foreground));
        }

        .state-info-grid dd {
          margin: 0;
        }

        .empty-message {
          color: hsl(var(--muted-foreground));
          font-size: 0.9rem;
          margin: 0;
        }

        /* Element List Styles */
        .element-list {
          list-style: none;
          padding: 0;
          margin: 0;
        }

        .element-item {
          padding: 0.75rem;
          margin-bottom: 0.5rem;
          border-radius: 0.375rem;
          background: rgba(255, 255, 255, 0.02);
          border: 1px solid rgba(255, 255, 255, 0.05);
        }

        .element-name {
          font-weight: 500;
          margin-bottom: 0.5rem;
        }

        .element-details {
          display: flex;
          gap: 1rem;
          flex-wrap: wrap;
          font-size: 0.85rem;
          color: hsl(var(--muted-foreground));
        }

        .badge-search,
        .badge-fixed,
        .badge-anchor {
          font-size: 0.75rem;
          padding: 0.125rem 0.5rem;
          border-radius: 0.25rem;
          font-weight: 500;
        }

        .badge-search {
          background: hsl(var(--secondary) / 0.2);
          color: hsl(var(--secondary));
        }

        .badge-fixed {
          background: hsl(var(--accent) / 0.2);
          color: hsl(var(--accent));
        }

        .badge-anchor {
          background: hsl(var(--primary) / 0.2);
          color: hsl(var(--primary));
        }

        .metadata-content {
          background: rgba(0, 0, 0, 0.2);
          padding: 1rem;
          border-radius: 0.375rem;
          font-size: 0.85rem;
          overflow-x: auto;
          margin: 0;
        }

        /* Button Styles */
        .btn-export,
        .btn-edit {
          padding: 0.5rem 1rem;
          border-radius: 0.375rem;
          font-weight: 500;
          border: none;
          cursor: pointer;
          transition: all 0.2s;
          font-size: 0.9rem;
        }

        .btn-export {
          background: hsl(var(--primary));
          color: hsl(var(--primary-foreground));
        }

        .btn-export:hover:not(:disabled) {
          opacity: 0.9;
          box-shadow: 0 0 10px hsl(var(--primary) / 0.5);
        }

        .btn-edit {
          background: hsl(var(--accent));
          color: hsl(var(--accent-foreground));
        }

        .btn-edit:hover:not(:disabled) {
          opacity: 0.9;
          box-shadow: 0 0 10px hsl(var(--accent) / 0.5);
        }

        .btn-export:disabled,
        .btn-edit:disabled {
          opacity: 0.5;
          cursor: not-allowed;
        }

        /* Scrollbar Styles */
        .state-list-items::-webkit-scrollbar,
        .state-details-content::-webkit-scrollbar {
          width: 8px;
        }

        .state-list-items::-webkit-scrollbar-track,
        .state-details-content::-webkit-scrollbar-track {
          background: rgba(255, 255, 255, 0.05);
        }

        .state-list-items::-webkit-scrollbar-thumb,
        .state-details-content::-webkit-scrollbar-thumb {
          background: rgba(255, 255, 255, 0.2);
          border-radius: 4px;
        }

        .state-list-items::-webkit-scrollbar-thumb:hover,
        .state-details-content::-webkit-scrollbar-thumb:hover {
          background: rgba(255, 255, 255, 0.3);
        }

        /* Responsive Design */
        @media (max-width: 768px) {
          .state-viewer-content {
            grid-template-columns: 1fr;
          }

          .state-list {
            max-height: 300px;
          }
        }
      `}</style>
    </div>
  );
};

export default StateViewer;
