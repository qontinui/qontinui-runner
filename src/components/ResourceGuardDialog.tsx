/**
 * ResourceGuardDialog — the blocking "Start anyway?" surface for a CRITICAL
 * spawn-time resource refusal (plan
 * `2026-08-07-runner-resource-guard-and-session-protection.md` §Part D step 3).
 *
 * Mounted once at the App root and prop-less: it subscribes to
 * `src/lib/resourceGuard.ts`'s queue itself, because the spawn that got refused
 * can originate anywhere (a terminal tab, a worker launch button, the Instances
 * panel) while the dialog must exist exactly once.
 *
 * Built on `components/ui/ConfirmDialog` — the runner's real blocking dialog:
 * `fixed inset-0 z-50` with a backdrop, already used by `LibraryBuilderLayout`
 * and `WorkflowBuilderTab`. Deliberately NOT `components/ConflictModal.tsx`,
 * which despite its name is a self-subscribing toast banner (`fixed bottom-4
 * right-4`, `role="alert"`, no backdrop) that kept its name only for import
 * compatibility — it cannot block anything.
 *
 * The override exists because a false positive here blocks the operator's actual
 * work, which is a worse failure than an occasional missed warning. There is no
 * silent-refusal path: either this dialog is on screen, or the refusal reached a
 * caller that reports it (coord `report_spawn_failed`, an HTTP error body).
 */

import { ConfirmDialog } from "./ui";
import { resolvePendingResourceBlock, usePendingResourceBlock } from "@/lib/resourceGuard";

export function ResourceGuardDialog() {
  const pending = usePendingResourceBlock();
  if (!pending) return null;

  return (
    <ConfirmDialog
      open
      variant="danger"
      title="Low memory"
      message={pending.message}
      description={
        "Starting anyway may cause Windows to kill a session that is already running — " +
        "that is the failure this guard exists to prevent. Closing a build or an idle " +
        "session first is usually enough. The floors are configurable under " +
        "Settings > Resource Guard."
      }
      confirmText="Start anyway"
      cancelText="Don't start"
      onConfirm={() => resolvePendingResourceBlock(true)}
      onClose={() => resolvePendingResourceBlock(false)}
    />
  );
}
