# React Hooks

This directory contains reusable React hooks for the Qontinui Runner frontend.

## Available Hooks

### `useActionLogView`

Hook for fetching and managing Action Log view data from the display profile system.

**Features:**
- Automatic state management (loading, error, data)
- Optional auto-refresh with configurable interval
- Manual refresh function
- Proper cleanup to prevent memory leaks
- TypeScript type safety

**Basic Usage:**
```tsx
import { useActionLogView } from './hooks';

function ActionsTab() {
  const { viewData, loading, error, refresh } = useActionLogView();

  if (loading) return <div>Loading...</div>;
  if (error) return <div>Error: {error}</div>;
  if (!viewData) return <div>No data</div>;

  return (
    <div>
      <p>Showing {viewData.visible_count} of {viewData.total_count} actions</p>
      {viewData.actions.map(action => (
        <ActionRow key={action.id} action={action} />
      ))}
    </div>
  );
}
```

**With Auto-Refresh:**
```tsx
const { viewData, loading, error } = useActionLogView({
  autoRefreshInterval: 1000, // Refresh every second
});
```

**Delayed Loading:**
```tsx
const { viewData, loading, error, refresh } = useActionLogView({
  fetchOnMount: false, // Don't load immediately
});

// Later, trigger manually
<button onClick={refresh}>Load Data</button>
```

## See Also

- `useActionLogView.example.tsx` - Comprehensive usage examples
- `../types/displayProfile.ts` - TypeScript type definitions
