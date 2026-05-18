/**
 * Spawn-time claim-conflict modal.
 *
 * Plan reference:
 * `D:/qontinui-root/plans/2026-05-18-agent-spawn-coordination.md` Phase 3.
 *
 * Listens for the runner's `agent-claim-conflict` Tauri event. When fired,
 * renders a modal explaining that another agent is already working on the
 * scoped claim, and offers three actions:
 *
 *   - **Abort** — close the modal without doing anything.
 *   - **Wait** — poll `GET /coord/claims/by-resource` every 30s; on the
 *     claim clearing, auto-retry `claims_acquire` (via the runner-side
 *     Tauri command) and close the modal. Cancellable.
 *   - **Steal** — confirmation sub-dialog (free-text reason); calls
 *     `claims_steal` Tauri command; on success retries `claims_acquire`
 *     and closes the modal.
 *
 * The modal is non-blocking — only one conflict can be active at a time
 * in the current implementation. A second event mid-resolution simply
 * replaces the displayed conflict.
 */

import { useEffect, useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Snake-case ClaimKind discriminator on the wire (matches coord). */
type ClaimKind =
  | "phase"
  | "file_glob"
  | "worktree"
  | "branch_name"
  | "alembic_revision"
  | "ci_wait";

/** Payload of the runner's `agent-claim-conflict` Tauri event. */
interface ConflictEvent {
  kind: ClaimKind | string;
  resourceKey: string;
  currentHolder: string;
  intent?: string | null;
}

/** Coord's `AcquireResult` discriminator — `result` field. */
type AcquireResult =
  | { result: "claimed"; ttl_seconds: number }
  | { result: "renewed"; ttl_seconds: number }
  | { result: "held"; current_holder: string }
  | { result: "topic_conflict" }
  | { result: "topic_unknown" }
  | { result: "invalid_topic" };

/** Coord's `ClaimHolder` shape (GET /coord/claims/by-resource). */
interface ClaimHolder {
  kind: string;
  resource_key: string;
  machine_id: string;
  ttl_seconds: number;
}

/** Per-instance retry source — used by the retry/steal paths to call
 *  the Tauri `claims_acquire` wrapper with the same args the original
 *  spawn used. The event payload doesn't carry the requesting machine
 *  id; we ask the runner's `get_machine_id` endpoint to fetch it on
 *  demand.
 */
async function retryAcquire(
  conflict: ConflictEvent,
): Promise<AcquireResult> {
  const machineId = await getMachineId();
  return invoke<AcquireResult>("claims_acquire", {
    args: {
      kind: conflict.kind,
      resourceKey: conflict.resourceKey,
      machineId,
      metadata: { intent: conflict.intent ?? null },
    },
  });
}

async function getMachineId(): Promise<string> {
  // The runner exposes its machine_id via the device-info command —
  // see `commands::auth::get_device_info`. The conflict event carries
  // `currentHolder` (the OTHER machine's id), not ours, so we fetch
  // ours separately.
  const info = await invoke<{ machineId?: string }>("get_device_info").catch(
    () => ({}) as { machineId?: string },
  );
  if (info?.machineId) return info.machineId;
  // Fallback — without a machine_id the steal/wait paths can't proceed;
  // the modal surfaces an error inline.
  throw new Error("machine_id not available from get_device_info");
}

/** Poll `GET /coord/claims/by-resource` until the claim is no longer
 *  held, OR until the AbortController fires.
 *
 *  Returns when:
 *  - The claim is no longer held (resolves with `null`).
 *  - The caller aborts (rejects with `DOMException(AbortError)`).
 *
 *  Polling cadence: 30s, per plan.
 */
async function waitForClaimCleared(
  kind: string,
  resourceKey: string,
  signal: AbortSignal,
): Promise<ClaimHolder | null> {
  while (!signal.aborted) {
    const url = new URL("/coord/claims/by-resource", coordHttpBase());
    url.searchParams.set("kind", kind);
    url.searchParams.set("key", resourceKey);
    try {
      const resp = await fetch(url.toString(), { signal });
      if (resp.ok) {
        const body = (await resp.json()) as ClaimHolder | null;
        if (body == null) return null;
      }
    } catch (e) {
      if (signal.aborted) throw e;
      // transient — keep polling
    }
    await sleep(30_000, signal);
  }
  throw new DOMException("aborted", "AbortError");
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const t = setTimeout(resolve, ms);
    if (signal) {
      const onAbort = () => {
        clearTimeout(t);
        reject(new DOMException("aborted", "AbortError"));
      };
      if (signal.aborted) {
        onAbort();
      } else {
        signal.addEventListener("abort", onAbort, { once: true });
      }
    }
  });
}

/**
 * Resolve coord's HTTP base URL by asking the runner via a dedicated
 * Tauri command if available; falls back to `http://localhost:9870` for
 * the dev profile.
 *
 * For Phase 3 we use the fallback directly — by-resource polling is a
 * read-only GET that's fine to call against the dev default; a future
 * enhancement adds a `get_coord_http_base` Tauri command if the production
 * deployment needs it.
 */
function coordHttpBase(): string {
  return "http://localhost:9870";
}

export function ConflictModal() {
  const [conflict, setConflict] = useState<ConflictEvent | null>(null);
  const [waitingState, setWaitingState] = useState<"idle" | "waiting" | "retrying">("idle");
  const [stealOpen, setStealOpen] = useState(false);
  const [stealReason, setStealReason] = useState("");
  const [stealBusy, setStealBusy] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const waitAbortRef = useRef<AbortController | null>(null);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    (async () => {
      try {
        unlisten = await listen<ConflictEvent>("agent-claim-conflict", (event) => {
          setConflict(event.payload);
          setWaitingState("idle");
          setStealOpen(false);
          setStealReason("");
          setErrorMessage(null);
        });
      } catch (e) {
        // listen() failure is unrecoverable for this surface; logging
        // is the best we can do.
        console.error("ConflictModal: listen failed", e);
      }
    })();
    return () => {
      if (unlisten) unlisten();
      waitAbortRef.current?.abort();
    };
  }, []);

  const closeModal = useCallback(() => {
    waitAbortRef.current?.abort();
    setConflict(null);
    setWaitingState("idle");
    setStealOpen(false);
    setStealReason("");
    setErrorMessage(null);
  }, []);

  const handleWait = useCallback(async () => {
    if (!conflict) return;
    setErrorMessage(null);
    waitAbortRef.current?.abort();
    const ac = new AbortController();
    waitAbortRef.current = ac;
    setWaitingState("waiting");
    try {
      await waitForClaimCleared(conflict.kind, conflict.resourceKey, ac.signal);
      setWaitingState("retrying");
      const result = await retryAcquire(conflict);
      if (result.result === "claimed" || result.result === "renewed") {
        closeModal();
      } else if (result.result === "held") {
        // Race — someone else grabbed it between poll-clear and retry.
        // Restart the wait loop.
        setConflict({
          ...conflict,
          currentHolder: result.current_holder,
        });
        setWaitingState("idle");
      } else {
        setErrorMessage(`unexpected acquire result: ${result.result}`);
        setWaitingState("idle");
      }
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") {
        // Caller cancelled — already in `idle` after closeModal.
        return;
      }
      setErrorMessage(String(e));
      setWaitingState("idle");
    } finally {
      waitAbortRef.current = null;
    }
  }, [conflict, closeModal]);

  const handleStealConfirm = useCallback(async () => {
    if (!conflict) return;
    setStealBusy(true);
    setErrorMessage(null);
    try {
      const machineId = await getMachineId();
      const stealResult = await invoke<{ result: string; displaced_machine_id?: string | null }>(
        "claims_steal",
        {
          args: {
            kind: conflict.kind,
            resourceKey: conflict.resourceKey,
            machineId,
            reason: stealReason.trim() || "user-initiated steal from runner UI",
          },
        },
      );
      if (stealResult.result !== "stolen" && stealResult.result !== "not_held" && stealResult.result !== "no_originator") {
        setErrorMessage(`unexpected steal result: ${stealResult.result}`);
        setStealBusy(false);
        return;
      }
      // Steal succeeded — retry the acquire so the user's session can proceed.
      const retry = await retryAcquire(conflict);
      if (retry.result === "claimed" || retry.result === "renewed") {
        closeModal();
      } else if (retry.result === "held") {
        // Another agent grabbed it in the race window — surface fresh holder.
        setConflict({ ...conflict, currentHolder: retry.current_holder });
        setStealOpen(false);
        setStealReason("");
      } else {
        setErrorMessage(`acquire after steal returned: ${retry.result}`);
      }
    } catch (e) {
      setErrorMessage(String(e));
    } finally {
      setStealBusy(false);
    }
  }, [conflict, stealReason, closeModal]);

  if (!conflict) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="claim-conflict-title"
      style={modalBackdropStyle}
    >
      <div style={modalCardStyle}>
        <h2 id="claim-conflict-title" style={titleStyle}>
          Claim conflict
        </h2>
        <p style={messageStyle}>
          Another agent (machine{" "}
          <code style={codeStyle}>
            {conflict.currentHolder ? conflict.currentHolder.slice(0, 8) : "unknown"}
          </code>
          ) is already working on{" "}
          <code style={codeStyle}>
            {conflict.kind}:{conflict.resourceKey}
          </code>
          .
        </p>
        {conflict.intent && (
          <p style={intentStyle}>
            <strong>Intent:</strong> {conflict.intent}
          </p>
        )}

        {errorMessage && <p style={errorStyle}>{errorMessage}</p>}

        {!stealOpen && (
          <div style={buttonRowStyle}>
            <button
              type="button"
              onClick={closeModal}
              disabled={waitingState !== "idle"}
              style={btnSecondary}
            >
              Abort
            </button>
            {waitingState === "idle" && (
              <button type="button" onClick={handleWait} style={btnSecondary}>
                Wait
              </button>
            )}
            {waitingState === "waiting" && (
              <button
                type="button"
                onClick={() => {
                  waitAbortRef.current?.abort();
                  setWaitingState("idle");
                }}
                style={btnSecondary}
              >
                Cancel wait
              </button>
            )}
            {waitingState === "retrying" && (
              <button type="button" disabled style={btnSecondary}>
                Retrying...
              </button>
            )}
            <button
              type="button"
              onClick={() => setStealOpen(true)}
              disabled={waitingState !== "idle"}
              style={btnDanger}
            >
              Steal
            </button>
          </div>
        )}

        {stealOpen && (
          <div style={{ marginTop: 16 }}>
            <p style={messageStyle}>
              <strong>Confirm steal.</strong> This will revoke the other
              agent&apos;s claim. The other agent&apos;s next heartbeat will
              return <code style={codeStyle}>Stolen</code> and its session
              will be notified.
            </p>
            <label style={{ display: "block", marginTop: 8 }}>
              <span style={labelStyle}>Reason (free text)</span>
              <textarea
                value={stealReason}
                onChange={(e) => setStealReason(e.target.value)}
                placeholder="why are you stealing this claim?"
                rows={3}
                style={textareaStyle}
              />
            </label>
            <div style={buttonRowStyle}>
              <button
                type="button"
                onClick={() => {
                  setStealOpen(false);
                  setStealReason("");
                }}
                disabled={stealBusy}
                style={btnSecondary}
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={handleStealConfirm}
                disabled={stealBusy}
                style={btnDanger}
              >
                {stealBusy ? "Stealing..." : "Confirm steal"}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────
// Inline styles — match the existing BuildRefreshBanner / canary-alert
// look-and-feel rather than depending on Tailwind's component class library.
// ─────────────────────────────────────────────────────────────────────────

const modalBackdropStyle: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0, 0, 0, 0.6)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 10000,
};

const modalCardStyle: React.CSSProperties = {
  background: "var(--bg-tertiary, #242837)",
  color: "var(--text-primary, #e4e4e7)",
  border: "1px solid var(--accent, #6366f1)",
  borderRadius: 12,
  padding: 24,
  maxWidth: 560,
  width: "90%",
  boxShadow: "0 12px 40px rgba(0, 0, 0, 0.5)",
};

const titleStyle: React.CSSProperties = {
  margin: 0,
  marginBottom: 12,
  fontSize: "1.25rem",
  color: "var(--text-primary, #e4e4e7)",
};

const messageStyle: React.CSSProperties = {
  margin: 0,
  marginBottom: 12,
  fontSize: "0.9375rem",
  color: "var(--text-secondary, #a1a1aa)",
  lineHeight: 1.5,
};

const intentStyle: React.CSSProperties = {
  ...messageStyle,
  background: "rgba(99, 102, 241, 0.08)",
  borderLeft: "3px solid var(--accent, #6366f1)",
  padding: "8px 12px",
  borderRadius: 4,
};

const codeStyle: React.CSSProperties = {
  background: "rgba(255, 255, 255, 0.08)",
  padding: "2px 6px",
  borderRadius: 4,
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: "0.875rem",
};

const labelStyle: React.CSSProperties = {
  display: "block",
  fontSize: "0.8125rem",
  marginBottom: 4,
  color: "var(--text-secondary, #a1a1aa)",
};

const textareaStyle: React.CSSProperties = {
  width: "100%",
  fontFamily: "inherit",
  fontSize: "0.875rem",
  background: "var(--bg-secondary, #1a1d2a)",
  color: "var(--text-primary, #e4e4e7)",
  border: "1px solid var(--border, #3f3f46)",
  borderRadius: 4,
  padding: 8,
  resize: "vertical",
};

const buttonRowStyle: React.CSSProperties = {
  display: "flex",
  justifyContent: "flex-end",
  gap: 8,
  marginTop: 16,
};

const btnSecondary: React.CSSProperties = {
  background: "transparent",
  color: "var(--text-primary, #e4e4e7)",
  border: "1px solid var(--border, #3f3f46)",
  borderRadius: 4,
  padding: "6px 14px",
  fontSize: "0.875rem",
  cursor: "pointer",
};

const btnDanger: React.CSSProperties = {
  background: "var(--destructive, #ef4444)",
  color: "#fff",
  border: "none",
  borderRadius: 4,
  padding: "6px 14px",
  fontSize: "0.875rem",
  fontWeight: 600,
  cursor: "pointer",
};

const errorStyle: React.CSSProperties = {
  margin: "8px 0",
  padding: "8px 12px",
  background: "rgba(239, 68, 68, 0.1)",
  border: "1px solid rgba(239, 68, 68, 0.3)",
  borderRadius: 4,
  color: "var(--destructive, #ef4444)",
  fontSize: "0.875rem",
};
