# ActionDetailModal Component

A comprehensive modal component for displaying detailed action information from the execution tree. This modal shows complete action details including configuration, runtime results, state context, timing, and screenshots.

## Features

- **Action Overview**: Displays action type, status, and duration with visual indicators
- **Configuration Display**: Shows formatted JSON of action configuration
- **Runtime Results**: Comprehensive display of execution results
  - FIND actions: Shows all matches with confidence scores and locations
  - TYPE actions: Displays typed text and character count
  - CLICK actions: Shows click coordinates and button details
  - GO_TO_STATE actions: Shows state transitions and paths
  - IF actions: Displays condition results and branch taken
- **State Context**: Shows states before/after, activated, and deactivated states
- **Timing Information**: Displays start time, end time, and precise duration
- **Screenshot Support**: Shows action screenshots with full-size viewer
- **Error Display**: Detailed error messages with stack traces when available

## Props

```typescript
interface ActionDetailModalProps {
  action: DisplayNode | null;  // The action node to display (null closes modal)
  isOpen: boolean;              // Controls modal visibility
  onClose: () => void;          // Callback when modal is closed
}
```

## Usage Example

### Basic Usage

```typescript
import { useState } from "react";
import ActionDetailModal from "./components/ActionDetailModal";
import { DisplayNode } from "./types/treeEvents";

function MyComponent() {
  const [selectedAction, setSelectedAction] = useState<DisplayNode | null>(null);
  const [isModalOpen, setIsModalOpen] = useState(false);

  const handleActionClick = (action: DisplayNode) => {
    setSelectedAction(action);
    setIsModalOpen(true);
  };

  const handleCloseModal = () => {
    setIsModalOpen(false);
    setSelectedAction(null);
  };

  return (
    <>
      {/* Your action list/tree */}
      <div onClick={() => handleActionClick(someAction)}>
        Click to view details
      </div>

      {/* Action Detail Modal */}
      <ActionDetailModal
        action={selectedAction}
        isOpen={isModalOpen}
        onClose={handleCloseModal}
      />
    </>
  );
}
```

### Integration with TreeNode Component

To add click-to-view-details functionality to the TreeNode component:

```typescript
// TreeNode.tsx
interface TreeNodeProps {
  node: DisplayNode;
  isExpanded: boolean;
  onToggle: (id: string) => void;
  expandedNodes: Set<string>;
  level?: number;
  onNodeClick?: (node: DisplayNode) => void;  // Add this prop
}

export const TreeNode: React.FC<TreeNodeProps> = ({
  node,
  isExpanded,
  onToggle,
  expandedNodes,
  level,
  onNodeClick,  // Add this
}) => {
  // ... existing code ...

  return (
    <div className="space-y-1">
      <div
        className="flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-accent/5 transition-colors cursor-pointer group"
        onClick={(e) => {
          // Only expand/collapse if clicking the toggle, otherwise show details
          if (isExpandable && e.target === e.currentTarget) {
            onToggle(node.id);
          } else if (onNodeClick) {
            onNodeClick(node);  // Show detail modal
          }
        }}
      >
        {/* ... rest of the component ... */}
      </div>
    </div>
  );
};
```

### Full Example with HierarchicalActionLog

```typescript
// HierarchicalActionLog.tsx
import { useState } from "react";
import { TreeNode as TreeNodeComponent } from "./TreeNode";
import ActionDetailModal from "./ActionDetailModal";
import { DisplayNode } from "../types/treeEvents";

export default function HierarchicalActionLog({ treeRoots, autoScroll }: Props) {
  const [selectedAction, setSelectedAction] = useState<DisplayNode | null>(null);
  const [isDetailModalOpen, setIsDetailModalOpen] = useState(false);

  const handleNodeClick = (node: DisplayNode) => {
    // Only show details for action nodes, not workflow nodes
    if (node.type === "action") {
      setSelectedAction(node);
      setIsDetailModalOpen(true);
    }
  };

  const handleCloseModal = () => {
    setIsDetailModalOpen(false);
    // Keep selectedAction for a moment to allow modal close animation
    setTimeout(() => setSelectedAction(null), 200);
  };

  return (
    <>
      <div className="space-y-2">
        {treeRoots.map((node) => (
          <TreeNodeComponent
            key={node.id}
            node={node}
            isExpanded={expandedNodes.has(node.id)}
            onToggle={handleToggleNode}
            expandedNodes={expandedNodes}
            onNodeClick={handleNodeClick}  // Pass the click handler
          />
        ))}
      </div>

      {/* Detail Modal */}
      <ActionDetailModal
        action={selectedAction}
        isOpen={isDetailModalOpen}
        onClose={handleCloseModal}
      />
    </>
  );
}
```

## Runtime Data Structure

The modal displays runtime data from `action.metadata.runtime`. Here's what different action types provide:

### FIND Actions with Multiple Matches

```json
{
  "runtime": {
    "top_matches": [
      {
        "confidence": 0.987,
        "location": { "x": 20, "y": 20 },
        "dimensions": { "w": 100, "h": 50 }
      },
      {
        "confidence": 0.952,
        "location": { "x": 25, "y": 30 },
        "dimensions": { "w": 100, "h": 50 }
      }
    ]
  }
}
```

### TYPE Actions

```json
{
  "runtime": {
    "typed_text": "Hello World",
    "character_count": 11
  }
}
```

### CLICK Actions

```json
{
  "runtime": {
    "clicked_at": { "x": 150, "y": 200 },
    "button": "left",
    "target_type": "lastFindResult"
  }
}
```

### GO_TO_STATE Actions

```json
{
  "runtime": {
    "source_states": ["login_screen"],
    "target_states": ["dashboard"],
    "targets_reached": ["dashboard"],
    "transitions_executed": ["login_to_dashboard"],
    "already_at_target": false
  }
}
```

## State Context

Shows state changes during action execution:

```json
{
  "state_context": {
    "active_before": ["login_screen"],
    "active_after": ["dashboard", "user_logged_in"],
    "changed": true,
    "activated": ["dashboard", "user_logged_in"],
    "deactivated": ["login_screen"]
  }
}
```

## Screenshot Support

The modal displays screenshots if available:

```typescript
{
  "metadata": {
    "screenshot_reference": "/path/to/screenshot.png",
    "visual_debug_reference": "/path/to/debug.png"
  }
}
```

Screenshots are displayed as thumbnails with the ability to:
- Click to view full size
- Hover to show expand button
- See full file path below the image

## Styling

The component uses:
- **Radix UI Dialog**: For accessible modal behavior
- **Tailwind CSS**: For styling with design system colors
- **Lucide Icons**: For consistent iconography
- **CSS Variables**: For theme support (light/dark mode)

### Theme Variables Used

```css
--foreground
--background
--border
--muted
--muted-foreground
--accent
--primary
--primary-foreground
```

## Accessibility

The modal includes:
- Proper ARIA labels for all interactive elements
- Screen reader descriptions
- Keyboard navigation support (Escape to close)
- Focus management
- Semantic HTML structure

## Dependencies

Required packages:
```json
{
  "@radix-ui/react-dialog": "^2.x",
  "lucide-react": "^0.x",
  "tailwindcss": "^3.x",
  "class-variance-authority": "^0.x",
  "clsx": "^2.x",
  "tailwind-merge": "^3.x"
}
```

## Future Enhancements

Potential improvements:
- Export action details as JSON
- Copy to clipboard functionality
- Link to view screenshots in Images tab
- Filter/search within runtime results
- Diff view for state changes
- Timeline visualization for timing
