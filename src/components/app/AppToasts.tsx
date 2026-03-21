interface AppToastsProps {
  runLastWorkflowError: string | null;
  onDismissRunError: () => void;
  staleTaskMessage: string | null;
  onDismissStaleTask: () => void;
}

export function AppToasts({
  runLastWorkflowError,
  onDismissRunError,
  staleTaskMessage,
  onDismissStaleTask,
}: AppToastsProps) {
  return (
    <>
      {runLastWorkflowError && (
        <div className="fixed bottom-4 right-4 p-4 rounded-lg shadow-lg border max-w-md z-toast bg-card border-destructive/50">
          <div className="flex items-start gap-3">
            <div className="flex-1 min-w-0">
              <h4 className="font-medium text-sm text-destructive">Workflow Not Found</h4>
              <p className="text-sm text-muted-foreground mt-1">{runLastWorkflowError}</p>
            </div>
            <button
              onClick={onDismissRunError}
              className="text-muted-foreground hover:text-foreground shrink-0"
            >
              &times;
            </button>
          </div>
        </div>
      )}

      {staleTaskMessage && (
        <div className="fixed bottom-4 right-4 p-4 rounded-lg shadow-lg border max-w-md z-toast bg-card border-yellow-500/50">
          <div className="flex items-start gap-3">
            <div className="flex-1 min-w-0">
              <h4 className="font-medium text-sm text-yellow-600 dark:text-yellow-400">
                Possibly Stale Task
              </h4>
              <p className="text-sm text-muted-foreground mt-1">{staleTaskMessage}</p>
            </div>
            <button
              onClick={onDismissStaleTask}
              className="text-muted-foreground hover:text-foreground shrink-0"
            >
              &times;
            </button>
          </div>
        </div>
      )}
    </>
  );
}
