/**
 * Race a promise against a timeout. If the wrapped promise doesn't settle
 * within `ms`, the returned promise rejects with an `Error` tagged with
 * `label` so the caller's existing catch path runs the same cleanup it
 * would on any other failure.
 *
 * Used by `AuthProvider` to defend against the initial-mount Tauri IPC
 * hang that left the runner stuck on LoginScreen — see plan
 * `qontinui-dev-notes/plans/2026-05-17-runner-auto-login-initial-mount.md`.
 */
export function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeoutPromise = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      reject(new Error(`${label} timed out after ${ms}ms`));
    }, ms);
  });
  return Promise.race([promise, timeoutPromise]).finally(() => {
    if (timer !== undefined) clearTimeout(timer);
  });
}
