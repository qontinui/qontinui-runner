/**
 * WebIntegrationSettings.tsx
 *
 * Settings for web integration, enabling this runner to register with
 * qontinui-web so it can be dispatched to and stream phase events back.
 *
 * Backed by the following Tauri commands (Phase 3G-runner):
 *   - get_web_integration_status
 *   - save_web_integration_settings
 *   - test_web_integration_connection
 *   - start_web_token_flow   (optional; provided by 3G-web-polish — hidden when missing)
 *
 * Also listens to the "web-integration-changed" event so the UI refreshes
 * whenever the backing settings change (including via the web token flow).
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { formatDistanceToNow } from "date-fns";
import {
  Globe,
  CheckCircle2,
  XCircle,
  Loader2,
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  ExternalLink,
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Shape of `get_web_integration_status` response from the runner. */
interface WebIntegrationStatus {
  enabled: boolean;
  backendUrl: string;
  runnerTokenMasked: string;
  runnerId: string | null;
  lastHeartbeatAt: string | null;
  registrationError: string | null;
  /** Whether the unified runner WebSocket is currently connected. */
  wsConnected: boolean;
}

/** Shape of `test_web_integration_connection` response on success. */
interface WebIntegrationTestResult {
  runnerId: string;
}

interface TestConnectionUIState {
  state: "idle" | "testing" | "success" | "error";
  runnerId?: string;
  error?: string;
}

interface WebIntegrationSettingsProps {
  onLog: LogFunction;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** URL validation: must parse and use http/https scheme. */
function validateBackendUrl(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed) return "Backend URL is required";
  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return "Backend URL must use http:// or https://";
    }
  } catch {
    return "Backend URL must be a valid URL";
  }
  return null;
}

/**
 * Format an ISO timestamp as "<relative time> ago".
 * Uses date-fns which is already a project dependency (see WorkflowLibraryCard).
 */
function formatRelativeTime(iso: string | null): string {
  if (!iso) return "never";
  try {
    return formatDistanceToNow(new Date(iso), { addSuffix: true });
  } catch {
    return "unknown";
  }
}

/** Truncate a long error message for single-line display. */
function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  return text.slice(0, Math.max(0, max - 1)) + "…";
}

/**
 * Derive the user-facing web-frontend origin from an API backend URL.
 *
 * Production is a split deployment: the API host carries an `api.` label
 * (`https://api.qontinui.io`, which serves NO login page) while the web
 * frontend drops it (`https://qontinui.io`). When the backend host starts with
 * `api.` we strip that single label (preserving scheme + port); otherwise
 * (localhost dev, a bare IP, or a unified origin) we return the backend
 * unchanged. Mirrors the Rust `api_config::derive_web_base_url` used by the
 * one-click web-login flow so both code paths land on the same origin.
 *
 * Returns null if `backendUrl` is empty/unparseable, so callers can fall back
 * to plain text rather than rendering a broken link.
 */
function deriveWebAppUrl(backendUrl: string): string | null {
  const trimmed = backendUrl.trim();
  if (!trimmed) return null;
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return null;
  }
  const host = parsed.hostname.startsWith("api.")
    ? parsed.hostname.slice("api.".length)
    : parsed.hostname;
  const portPart = parsed.port ? `:${parsed.port}` : "";
  return `${parsed.protocol}//${host}${portPart}`;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

const PLACEHOLDER_BACKEND_URL = "https://api.qontinui.io";

export function WebIntegrationSettings({ onLog }: WebIntegrationSettingsProps) {
  // --- Persisted/server state ------------------------------------------------
  const [status, setStatus] = useState<WebIntegrationStatus | null>(null);
  const [loading, setLoading] = useState(true);

  // --- Form state (user edits, may diverge from `status`) -------------------
  const [formEnabled, setFormEnabled] = useState(false);
  const [formBackendUrl, setFormBackendUrl] = useState("");
  /** Token input value. Empty string means "unchanged, keep persisted token". */
  const [formToken, setFormToken] = useState("");
  const [tokenEdited, setTokenEdited] = useState(false);

  const [saving, setSaving] = useState(false);
  const [urlError, setUrlError] = useState<string | null>(null);
  const [testState, setTestState] = useState<TestConnectionUIState>({ state: "idle" });

  // --- One-click token flow availability (3G-web-polish) ---------------------
  const [tokenFlowAvailable, setTokenFlowAvailable] = useState<boolean>(false);

  // --- Help collapse ---------------------------------------------------------
  const [helpTokenOpen, setHelpTokenOpen] = useState(false);
  const [helpWhatOpen, setHelpWhatOpen] = useState(false);

  // --- Pair-code paste flow (Phase 2a.3) -------------------------------------
  // Single-use 5-char-from-32-alphabet code minted by the operator on
  // qontinui-web's /runners Auth Tokens tab and typed in here.
  const [pairCodeInput, setPairCodeInput] = useState("");
  const [pairCodeState, setPairCodeState] = useState<{
    state: "idle" | "redeeming" | "success" | "error";
    message?: string;
  }>({ state: "idle" });

  // --- Tick to keep "last heartbeat <relative> ago" alive --------------------
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((n) => n + 1), 30_000);
    return () => clearInterval(id);
  }, []);

  // Track whether the form has been seeded from status, so we don't overwrite
  // the user's in-progress edits on every refresh.
  const seededRef = useRef(false);

  const applyStatusToForm = useCallback((next: WebIntegrationStatus) => {
    setFormEnabled(next.enabled);
    setFormBackendUrl(next.backendUrl);
    // Token stays blank — the masked version is shown as placeholder.
    setFormToken("");
    setTokenEdited(false);
    setUrlError(null);
  }, []);

  const refreshStatus = useCallback(async (): Promise<WebIntegrationStatus | null> => {
    try {
      const next = await invoke<WebIntegrationStatus>("get_web_integration_status");
      setStatus(next);
      if (!seededRef.current) {
        applyStatusToForm(next);
        seededRef.current = true;
      }
      return next;
    } catch (err) {
      console.error("Failed to load web integration status:", err);
      onLog("error", `Failed to load web integration status: ${err}`);
      return null;
    }
  }, [applyStatusToForm, onLog]);

  // --- Initial load + event subscription + polling ---------------------------
  useEffect(() => {
    let cancelled = false;
    const init = async () => {
      await refreshStatus();
      if (!cancelled) setLoading(false);
    };
    init();
    return () => {
      cancelled = true;
    };
  }, [refreshStatus]);

  useEffect(() => {
    // React to save events fired from anywhere (including the web token flow).
    const unlisten = listen("web-integration-changed", () => {
      void refreshStatus();
    });
    return () => {
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, [refreshStatus]);

  useEffect(() => {
    // Lightweight poll every 10s to pick up lastHeartbeatAt changes and
    // registration state transitions that don't fire the event.
    const id = setInterval(() => {
      void refreshStatus();
    }, 10_000);
    return () => clearInterval(id);
  }, [refreshStatus]);

  // --- Feature-detect the one-click flow command ----------------------------
  //
  // There is no formal "does this Tauri command exist" API. We piggy-back on
  // invoke's error contract: Tauri surfaces an unregistered command as an
  // error containing a phrase like "not found" / "is not defined". Any OTHER
  // error (validation, browser open failure, network) means the command IS
  // wired — it just didn't like our probe. That's the signal we want.
  //
  // We invoke with an empty backendUrl (undefined) so that IF the command is
  // already wired and IF invoking it successfully opens the browser, we don't
  // inadvertently start a real flow just to test. The 3G-web-polish team's
  // command takes an optional backendUrl; calling it with {} is valid, so on
  // a live binary this probe may actually open the user's browser. To avoid
  // that side effect, we defer the probe until the user hovers/focuses the
  // button via lazy detection below instead. (See `handleStartWebTokenFlow`.)
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        // Use the core "get command list" capability if exposed; otherwise
        // optimistically assume it's available and fall back on actual
        // invocation failure at click time.
        // Tauri exposes no public "list registered commands" — best we can do
        // without side effects is assume it's available when the command name
        // is known to be in the roadmap, and gracefully hide on real invoke
        // failure. See handleStartWebTokenFlow.
        if (!cancelled) setTokenFlowAvailable(true);
      } catch {
        if (!cancelled) setTokenFlowAvailable(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // --- Derived state --------------------------------------------------------
  const isDirty = (() => {
    if (!status) return false;
    if (formEnabled !== status.enabled) return true;
    if (formBackendUrl.trim() !== status.backendUrl.trim()) return true;
    if (tokenEdited && formToken.length > 0) return true;
    return false;
  })();

  const canSave = isDirty && !saving && !urlError;

  // Web-frontend origin for the "log into qontinui-web" help link. Derived from
  // the backend URL (api.qontinui.io → qontinui.io); null when the backend
  // field is empty/invalid, in which case the help text renders without a link.
  const webAppUrl = deriveWebAppUrl(formBackendUrl);

  // --- Handlers -------------------------------------------------------------
  const handleBackendUrlChange = (value: string) => {
    setFormBackendUrl(value);
    setUrlError(validateBackendUrl(value));
  };

  const handleEnabledToggle = () => {
    // When turning OFF an enabled+connected integration, confirm.
    if (formEnabled && status?.enabled) {
      const ok = window.confirm(
        "Disable web integration?\n\n" +
          "This runner will stop heartbeating to qontinui-web and will no " +
          "longer be dispatchable from the web UI. In-flight workflows are " +
          "not interrupted. You can re-enable this at any time.",
      );
      if (!ok) return;
    }
    setFormEnabled((prev) => !prev);
  };

  const handleTestConnection = useCallback(async () => {
    const urlValidation = validateBackendUrl(formBackendUrl);
    if (urlValidation) {
      setUrlError(urlValidation);
      setTestState({ state: "error", error: urlValidation });
      return;
    }
    // Use the edited token if provided, otherwise empty string. The backend
    // treats an empty token as "use persisted" when no new value is supplied.
    const tokenForTest = tokenEdited ? formToken : "";
    if (!tokenForTest && !status?.runnerTokenMasked) {
      setTestState({
        state: "error",
        error: "Enter a runner token before testing.",
      });
      return;
    }
    setTestState({ state: "testing" });
    try {
      const result = await invoke<WebIntegrationTestResult>("test_web_integration_connection", {
        backendUrl: formBackendUrl.trim(),
        runnerToken: tokenForTest,
      });
      setTestState({ state: "success", runnerId: result.runnerId });
    } catch (err) {
      setTestState({ state: "error", error: String(err) });
    }
  }, [formBackendUrl, formToken, tokenEdited, status?.runnerTokenMasked]);

  const handleSave = useCallback(async () => {
    const urlValidation = validateBackendUrl(formBackendUrl);
    if (urlValidation) {
      setUrlError(urlValidation);
      return;
    }
    setSaving(true);
    try {
      await invoke<void>("save_web_integration_settings", {
        enabled: formEnabled,
        backendUrl: formBackendUrl.trim(),
        runnerToken: tokenEdited ? formToken : "",
      });
      onLog("success", "Web integration settings saved");

      // Promote the runner to Tier 2 (qontinui_account) once a usable
      // integration config exists. Persisting a runner token alone leaves
      // the runner in Tier Local, where the cloud WS relay stays idle and
      // never connects (the relay only runs at tier == qontinui_account with
      // a device JWT). Promoting here — and kicking the relay via
      // set_runner_tier's side effect — is what actually brings the runner
      // online after a token Save. Gated on a complete config so toggling
      // the integration off doesn't spuriously promote.
      //
      // We use the post-save status (which reflects the just-persisted
      // token) to decide whether the config is complete.
      const next = await refreshStatus();
      const hasToken =
        (tokenEdited && formToken.trim().length > 0) ||
        Boolean(next?.runnerTokenMasked && next.runnerTokenMasked.length > 0);
      const configComplete =
        formEnabled && formBackendUrl.trim().length > 0 && hasToken;

      if (configComplete) {
        try {
          await invoke<void>("set_runner_tier", { tier: "qontinui_account" });
          // Notify tier consumers (AuthProvider et al.) without waiting for
          // their poll cycle.
          window.dispatchEvent(new CustomEvent("runner-tier-changed"));
          onLog("success", "Runner promoted to Qontinui account tier — connecting…");
          // Re-read so the status banner picks up the (re)connecting relay.
          await refreshStatus();
        } catch (tierErr) {
          onLog("error", `Saved, but tier promotion failed: ${tierErr}`);
        }
      }

      // Re-seed the form so isDirty goes false; also clear the raw token.
      if (next) applyStatusToForm(next);
    } catch (err) {
      onLog("error", `Failed to save web integration settings: ${err}`);
    } finally {
      setSaving(false);
    }
  }, [
    formBackendUrl,
    formEnabled,
    formToken,
    tokenEdited,
    onLog,
    refreshStatus,
    applyStatusToForm,
  ]);

  const handleRetryRegistration = useCallback(async () => {
    // "Retry" runs the test against the persisted-shape values and, if
    // successful, re-saves to kick the registration loop.
    setTestState({ state: "testing" });
    try {
      const result = await invoke<WebIntegrationTestResult>("test_web_integration_connection", {
        backendUrl: formBackendUrl.trim(),
        runnerToken: tokenEdited ? formToken : "",
      });
      setTestState({ state: "success", runnerId: result.runnerId });
      await invoke<void>("save_web_integration_settings", {
        enabled: formEnabled,
        backendUrl: formBackendUrl.trim(),
        runnerToken: tokenEdited ? formToken : "",
      });
      await refreshStatus();
      onLog("success", "Retrying web integration registration…");
    } catch (err) {
      setTestState({ state: "error", error: String(err) });
      onLog("error", `Retry failed: ${err}`);
    }
  }, [formBackendUrl, formEnabled, formToken, tokenEdited, onLog, refreshStatus]);

  /**
   * Redeem a 6-char one-time pair code minted from the dashboard.
   *
   * Phase 2a.3 of plan
   * `D:/qontinui-root/plans/2026-05-22-mtc-iter3-remediation-web-dashboard.md`.
   * Calls the Rust-side `redeem_pair_code` command which POSTs to
   * `/api/v1/devices/pair-codes/{code}/redeem` and persists the
   * resulting device JWT.
   */
  const handleRedeemPairCode = useCallback(async () => {
    const trimmed = pairCodeInput.trim().toUpperCase();
    if (!trimmed) {
      setPairCodeState({ state: "error", message: "Enter a pair code first." });
      return;
    }
    // Validate the backend URL the same way Save does, and pass the form
    // value through so the redeem hits the URL the operator currently sees
    // in the field — not a stale persisted/derived one. The Rust command
    // treats an empty/absent backendUrl as "fall back to the active
    // profile", preserving the prior behavior when the field is blank.
    const formUrl = formBackendUrl.trim();
    if (formUrl) {
      const urlValidation = validateBackendUrl(formUrl);
      if (urlValidation) {
        setPairCodeState({ state: "error", message: urlValidation });
        return;
      }
    }
    setPairCodeState({ state: "redeeming" });
    try {
      const result = await invoke<{
        userId: string;
        tenantId: string;
        deviceId: string;
      }>("redeem_pair_code", {
        code: trimmed,
        backendUrl: formUrl || undefined,
      });
      setPairCodeInput("");
      setPairCodeState({
        state: "success",
        message: `Paired (device ${result.deviceId.slice(0, 8)}…).`,
      });
      onLog("info", `Paired via one-time code (user=${result.userId})`);

      // A successful pair-code redeem persists a device JWT but, like Save,
      // leaves the runner in Tier Local — so the WS relay stays idle. Promote
      // to Tier 2 so the relay comes online. (The Rust redeem command also
      // promotes defensively; doing it here keeps the UI responsive without
      // waiting for the next status poll.)
      try {
        await invoke<void>("set_runner_tier", { tier: "qontinui_account" });
        window.dispatchEvent(new CustomEvent("runner-tier-changed"));
      } catch (tierErr) {
        onLog("error", `Paired, but tier promotion failed: ${tierErr}`);
      }

      // Refresh status so the UI picks up the new auth + ws state.
      void refreshStatus();
    } catch (err) {
      const msg = String(err);
      setPairCodeState({ state: "error", message: msg });
      onLog("error", `Pair-code redeem failed: ${msg}`);
    }
  }, [pairCodeInput, formBackendUrl, onLog, refreshStatus]);

  const handleStartWebTokenFlow = useCallback(async () => {
    try {
      await invoke<void>("start_web_token_flow", {
        backendUrl: formBackendUrl.trim() || undefined,
      });
      onLog("info", "Opened web login in your browser…");
    } catch (err) {
      const msg = String(err);
      const notFound =
        msg.includes("not found") ||
        msg.includes("Command not allowed") ||
        msg.includes("is not defined") ||
        msg.includes("Command start_web_token_flow not");
      if (notFound) {
        // 3G-web-polish hasn't shipped yet. Hide the button going forward.
        setTokenFlowAvailable(false);
        onLog(
          "info",
          "Web login flow is not available on this runner build — paste a token manually.",
        );
      } else {
        onLog("error", `Failed to start web token flow: ${msg}`);
      }
    }
  }, [formBackendUrl, onLog]);

  // --- Render ---------------------------------------------------------------
  if (loading) {
    return (
      <div className="space-y-6">
        <SectionHeader
          title="Web Integration"
          description="Register this runner with qontinui-web so it can be dispatched to and its phase events stream back."
          icon={<Globe className="w-6 h-6" />}
        />
        <div className="flex items-center gap-2 text-muted-foreground text-sm">
          <Loader2 className="w-4 h-4 animate-spin" />
          Loading…
        </div>
      </div>
    );
  }

  // --- Status banner --------------------------------------------------------
  const renderStatusBanner = () => {
    if (!status?.enabled) return null;
    if (status.wsConnected && status.runnerId) {
      return (
        <div className="flex items-start gap-2 p-3 rounded-md bg-green-500/10 border border-green-500/20">
          <CheckCircle2 className="w-4 h-4 text-green-500 shrink-0 mt-0.5" />
          <div className="text-xs">
            <div className="font-medium text-green-500">
              Connected as runner <code className="font-mono">{status.runnerId}</code>
            </div>
            <div className="text-muted-foreground">
              Last heartbeat {formatRelativeTime(status.lastHeartbeatAt)}
            </div>
          </div>
        </div>
      );
    }
    if (status.runnerId && !status.wsConnected) {
      // Runner was registered earlier in the session but the WS has dropped.
      // The relay is reconnecting in the background.
      return (
        <div className="flex items-start gap-2 p-3 rounded-md bg-yellow-500/10 border border-yellow-500/20">
          <Loader2 className="w-4 h-4 text-yellow-500 shrink-0 mt-0.5 animate-spin" />
          <div className="text-xs">
            <div className="font-medium text-yellow-500">Reconnecting…</div>
            <div className="text-muted-foreground">
              Lost connection to {status.backendUrl || PLACEHOLDER_BACKEND_URL}; retrying.
            </div>
          </div>
        </div>
      );
    }
    if (status.registrationError) {
      return (
        <div className="flex items-start gap-2 p-3 rounded-md bg-destructive/10 border border-destructive/20">
          <XCircle className="w-4 h-4 text-destructive shrink-0 mt-0.5" />
          <div className="text-xs flex-1 min-w-0">
            <div className="font-medium text-destructive">Registration failed</div>
            <div className="text-muted-foreground truncate" title={status.registrationError}>
              {truncate(status.registrationError, 140)}
            </div>
            <button
              onClick={handleRetryRegistration}
              className="mt-1.5 text-primary hover:underline text-xs"
            >
              Retry
            </button>
          </div>
        </div>
      );
    }
    return (
      <div className="flex items-start gap-2 p-3 rounded-md bg-yellow-500/10 border border-yellow-500/20">
        <Loader2 className="w-4 h-4 text-yellow-500 shrink-0 mt-0.5 animate-spin" />
        <div className="text-xs">
          <div className="font-medium text-yellow-500">Registering…</div>
          <div className="text-muted-foreground">
            Contacting {status.backendUrl || PLACEHOLDER_BACKEND_URL}
          </div>
        </div>
      </div>
    );
  };

  // --- Test result inline indicator -----------------------------------------
  const renderTestResult = () => {
    switch (testState.state) {
      case "testing":
        return (
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Loader2 className="w-3.5 h-3.5 animate-spin" /> Testing…
          </div>
        );
      case "success":
        return (
          <div className="flex items-center gap-1.5 text-xs text-green-500">
            <CheckCircle2 className="w-3.5 h-3.5" />
            Connected as <code className="font-mono">{testState.runnerId}</code>
          </div>
        );
      case "error":
        return (
          <div
            className="flex items-center gap-1.5 text-xs text-destructive"
            title={testState.error}
          >
            <XCircle className="w-3.5 h-3.5" />
            {truncate(testState.error ?? "Connection failed", 100)}
          </div>
        );
      default:
        return null;
    }
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Web Integration"
        description="Register this runner with qontinui-web so it can be dispatched to and its phase events stream back."
        icon={<Globe className="w-6 h-6" />}
      />

      {renderStatusBanner()}

      {/* Master toggle */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
          <div className="space-y-1">
            <div className="text-sm font-medium">Enabled</div>
            <div className="text-xs text-muted-foreground">
              When enabled, the runner self-registers with qontinui-web and heartbeats every 30s.
              Disabling stops heartbeats immediately; in-flight workflows are not interrupted.
            </div>
          </div>
          <button
            type="button"
            onClick={handleEnabledToggle}
            className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
              formEnabled ? "bg-primary" : "bg-muted"
            }`}
            aria-pressed={formEnabled}
            aria-label="Toggle web integration"
          >
            <span
              className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                formEnabled ? "translate-x-4" : "translate-x-1"
              }`}
            />
          </button>
        </label>
      </div>

      {/* Backend URL + Token */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Backend</h4>

        <div className="space-y-1.5">
          <label htmlFor="web-integration-backend-url" className="text-xs font-medium">
            Backend URL
          </label>
          <input
            id="web-integration-backend-url"
            type="url"
            autoComplete="off"
            spellCheck={false}
            value={formBackendUrl}
            onChange={(e) => handleBackendUrlChange(e.target.value)}
            placeholder={PLACEHOLDER_BACKEND_URL}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
          />
          {urlError ? (
            <p className="text-[10px] text-destructive flex items-center gap-1">
              <AlertTriangle className="w-3 h-3" /> {urlError}
            </p>
          ) : (
            <p className="text-[10px] text-muted-foreground">
              Base URL of the qontinui-web backend this runner will register with.
            </p>
          )}
        </div>

        {/* Pair code paste flow (Phase 2a.3) — recommended for new pairs.
            Sits above the long-lived runner-token input because pair
            codes are the simpler/safer path for new operators. */}
        <div className="space-y-1.5">
          <label htmlFor="web-integration-pair-code" className="text-xs font-medium">
            Pair code (6 chars)
          </label>
          <div className="flex items-stretch gap-2">
            <input
              id="web-integration-pair-code"
              type="text"
              inputMode="text"
              autoComplete="off"
              autoCorrect="off"
              spellCheck={false}
              maxLength={6}
              value={pairCodeInput}
              onChange={(e) => {
                setPairCodeInput(e.target.value.toUpperCase());
                if (pairCodeState.state !== "idle") {
                  setPairCodeState({ state: "idle" });
                }
              }}
              placeholder="A7K2P3"
              className="flex-1 min-w-0 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50 font-mono tracking-[0.3em] uppercase"
            />
            <button
              type="button"
              onClick={handleRedeemPairCode}
              disabled={
                pairCodeState.state === "redeeming" || pairCodeInput.trim().length === 0
              }
              className="px-3 py-1.5 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed whitespace-nowrap"
            >
              {pairCodeState.state === "redeeming" ? "Pairing…" : "Pair"}
            </button>
          </div>
          {pairCodeState.state === "success" && pairCodeState.message && (
            <p className="text-[10px] text-green-500 flex items-center gap-1">
              <CheckCircle2 className="w-3 h-3" /> {pairCodeState.message}
            </p>
          )}
          {pairCodeState.state === "error" && pairCodeState.message && (
            <p className="text-[10px] text-destructive flex items-center gap-1">
              <AlertTriangle className="w-3 h-3" /> {truncate(pairCodeState.message, 120)}
            </p>
          )}
          {pairCodeState.state === "idle" && (
            <p className="text-[10px] text-muted-foreground">
              Generate a one-time pair code on qontinui-web (Runners &rarr; Auth
              Tokens) and paste it here. The code expires after 5 minutes.
            </p>
          )}
        </div>

        <div className="space-y-1.5">
          <label htmlFor="web-integration-token" className="text-xs font-medium">
            Runner Token
          </label>
          <div className="flex items-stretch gap-2">
            <input
              id="web-integration-token"
              type="password"
              autoComplete="new-password"
              autoCorrect="off"
              spellCheck={false}
              data-lpignore="true"
              data-1p-ignore="true"
              value={formToken}
              onChange={(e) => {
                setFormToken(e.target.value);
                setTokenEdited(true);
              }}
              placeholder={
                status?.runnerTokenMasked && status.runnerTokenMasked.length > 0
                  ? status.runnerTokenMasked
                  : "qontinui_runner_…"
              }
              className="flex-1 min-w-0 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50 font-mono"
            />
            <button
              type="button"
              onClick={handleTestConnection}
              disabled={testState.state === "testing"}
              className="px-3 py-1.5 text-xs font-medium rounded-md bg-muted hover:bg-muted/70 transition-colors disabled:opacity-50 disabled:cursor-not-allowed whitespace-nowrap"
            >
              {testState.state === "testing" ? "Testing…" : "Test connection"}
            </button>
          </div>
          <div className="min-h-[1rem]">{renderTestResult()}</div>
          <p className="text-[10px] text-muted-foreground">
            Paste a token from qontinui-web. The token is never displayed in full;{" "}
            {status?.runnerTokenMasked
              ? "the placeholder shows its masked prefix."
              : "no token is currently saved."}
          </p>
        </div>

        {tokenFlowAvailable && formBackendUrl.trim() && (
          <div>
            <button
              type="button"
              onClick={handleStartWebTokenFlow}
              className="text-xs text-primary hover:underline inline-flex items-center gap-1"
            >
              Or connect with a web login <ExternalLink className="w-3 h-3" />
            </button>
          </div>
        )}
      </div>

      {/* Save button */}
      <div className="flex justify-end">
        <button
          type="button"
          onClick={handleSave}
          disabled={!canSave}
          className={`px-4 py-2 rounded-md text-sm font-medium transition-colors ${
            canSave
              ? "bg-primary text-primary-foreground hover:bg-primary/90"
              : "bg-muted text-muted-foreground cursor-not-allowed"
          }`}
        >
          {saving ? "Saving…" : "Save"}
        </button>
      </div>

      {/* Help */}
      <div className="space-y-2">
        <button
          type="button"
          onClick={() => setHelpTokenOpen((v) => !v)}
          className="w-full flex items-center gap-1.5 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors"
        >
          {helpTokenOpen ? (
            <ChevronDown className="w-3.5 h-3.5" />
          ) : (
            <ChevronRight className="w-3.5 h-3.5" />
          )}
          How do I get a runner token?
        </button>
        {helpTokenOpen && (
          <div className="text-xs text-muted-foreground pl-5 space-y-1">
            <p>
              1. Log into{" "}
              {webAppUrl ? (
                <a
                  href={`${webAppUrl}/login`}
                  target="_blank"
                  rel="noreferrer"
                  className="text-primary hover:underline inline-flex items-center gap-1"
                >
                  qontinui-web <ExternalLink className="w-3 h-3" />
                </a>
              ) : (
                <span>qontinui-web</span>
              )}
              .
            </p>
            <p>2. Go to Settings &rarr; Runner Fleet.</p>
            <p>
              3. Click <em>Create Token</em>, copy it, and paste it into the Runner Token field
              above.
            </p>
          </div>
        )}

        <button
          type="button"
          onClick={() => setHelpWhatOpen((v) => !v)}
          className="w-full flex items-center gap-1.5 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors"
        >
          {helpWhatOpen ? (
            <ChevronDown className="w-3.5 h-3.5" />
          ) : (
            <ChevronRight className="w-3.5 h-3.5" />
          )}
          What does this do?
        </button>
        {helpWhatOpen && (
          <div className="text-xs text-muted-foreground pl-5">
            <p>
              When enabled, this runner self-registers with qontinui-web, heartbeats every 30s, and
              streams phase events back to the web backend. You can then dispatch workflows from the
              web UI and follow their progress remotely. All connections are outbound from this
              machine.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
