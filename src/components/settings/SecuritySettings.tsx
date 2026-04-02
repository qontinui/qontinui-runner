import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Shield, Check, Info, FileSearch, RefreshCw } from "lucide-react";
import { getApiBase } from "@/lib/runner-api";

interface SecuritySettingsData {
  default_profile: string;
  custom_policy: SecurityPolicy | null;
  audit_enabled: boolean;
  audit_retention_days: number;
  credential_proxy_enabled: boolean;
  network_proxy_enabled: boolean;
}

interface SecurityPolicy {
  profile_name: string;
  filesystem: FilesystemPolicy;
  network: NetworkPolicy;
  actions: ActionPolicy;
  resources: ResourcePolicy;
  credentials: CredentialPolicy;
}

interface FilesystemPolicy {
  read_write_paths: string[];
  read_only_paths: string[];
  denied_paths: string[];
  read_only_root: boolean;
}

interface NetworkPolicy {
  mode: string;
  allowed_domains: string[];
  denied_domains: string[];
  block_metadata_endpoints: boolean;
  allowed_protocols: string[];
  log_requests: boolean;
}

interface ActionPolicy {
  allowed_commands: string[] | null;
  blocked_commands: string[];
  block_destructive: boolean;
  max_recursion_depth: number;
}

interface ResourcePolicy {
  cpu_limit: number | null;
  memory_limit_mb: number | null;
  pids_limit: number | null;
  timeout_secs: number;
}

interface CredentialPolicy {
  mode: string;
  allowed_credentials: string[];
}

interface ProfileSummary {
  name: string;
  description: string;
  fingerprint: string;
}

interface SecuritySettingsProps {
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

interface AuditEvent {
  id: string;
  timestamp: string;
  task_run_id: string | null;
  step_name: string | null;
  event_type: string;
  action: string;
  decision: string;
  reason: string | null;
}

interface AuditSummary {
  total: number;
  allowed: number;
  denied: number;
  warnings: number;
}

const DECISION_COLORS: Record<string, string> = {
  allowed: "bg-green-500/20 text-green-400",
  denied: "bg-red-500/20 text-red-400",
  warning: "bg-yellow-500/20 text-yellow-400",
};

const EVENT_TYPE_LABELS: Record<string, string> = {
  policy_evaluation: "Policy",
  capability_grant: "Grant",
  capability_denial: "Denial",
  network_request: "Network",
  filesystem_access: "Filesystem",
  credential_injection: "Credential",
  command_blocked: "Command",
  container_lifecycle: "Container",
};

const DEFAULT_SETTINGS: SecuritySettingsData = {
  default_profile: "permissive",
  custom_policy: null,
  audit_enabled: false,
  audit_retention_days: 30,
  credential_proxy_enabled: false,
  network_proxy_enabled: false,
};

function ToggleSwitch({
  checked,
  onChange,
  disabled,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={() => !disabled && onChange(!checked)}
      disabled={disabled}
      className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
        checked ? "bg-blue-600" : "bg-neutral-600"
      } ${disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
    >
      <span
        className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
          checked ? "translate-x-4" : "translate-x-1"
        }`}
      />
    </button>
  );
}

export function SecuritySettings({ onLog }: SecuritySettingsProps) {
  const [config, setConfig] = useState<SecuritySettingsData>(DEFAULT_SETTINGS);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [auditEvents, setAuditEvents] = useState<AuditEvent[]>([]);
  const [auditSummary, setAuditSummary] = useState<AuditSummary | null>(null);
  const [auditLoading, setAuditLoading] = useState(false);
  const [auditFilter, setAuditFilter] = useState<string>("all");

  useEffect(() => {
    loadSettings();
    loadProfiles();
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<SecuritySettingsData>("get_security_settings");
      if (result) {
        setConfig({ ...DEFAULT_SETTINGS, ...result });
        onLog("debug", "Security settings loaded");
      }
    } catch (err) {
      console.error("Failed to load security settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load security settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const loadProfiles = async () => {
    try {
      const result = await invoke<ProfileSummary[]>("get_security_profiles");
      if (result) {
        setProfiles(result);
      }
    } catch (err) {
      console.error("Failed to load security profiles:", err);
    }
  };

  const loadAuditData = useCallback(async () => {
    setAuditLoading(true);
    try {
      const decisionParam = auditFilter === "all" ? "" : `&decision=${auditFilter}`;
      const base = getApiBase();
      const [eventsResp, summaryResp] = await Promise.all([
        fetch(`${base}/security/audit/events?limit=50${decisionParam}`),
        fetch(`${base}/security/audit/summary`),
      ]);
      if (eventsResp.ok) {
        const eventsData = await eventsResp.json();
        setAuditEvents(eventsData.data || []);
      }
      if (summaryResp.ok) {
        const summaryData = await summaryResp.json();
        setAuditSummary(summaryData.data || null);
      }
    } catch (err) {
      console.error("Failed to load audit data:", err);
    } finally {
      setAuditLoading(false);
    }
  }, [auditFilter]);

  // Auto-load audit data when enabled and filter changes
  useEffect(() => {
    if (config.audit_enabled) {
      loadAuditData();
    }
  }, [config.audit_enabled, loadAuditData]);

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);

      await invoke("update_security_settings", { config });

      setSaveSuccess(true);
      onLog("success", "Security settings saved successfully");
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (err) {
      console.error("Failed to save security settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save security settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-neutral-400">Loading security settings...</div>
      </div>
    );
  }

  const activeProfile = profiles.find((p) => p.name === config.default_profile);

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Shield className="w-6 h-6 text-blue-400" />
        <div>
          <h3 className="text-lg font-semibold text-white">Security & Sandboxing</h3>
          <p className="text-xs text-neutral-400">
            Configure security policies and sandbox constraints for workflow execution
          </p>
        </div>
      </div>

      {error && (
        <div className="p-3 bg-red-500/10 border border-red-500/20 rounded-lg">
          <span className="text-red-400 text-xs">{error}</span>
        </div>
      )}

      {saveSuccess && (
        <div className="p-3 bg-green-500/10 border border-green-500/20 rounded-lg flex items-center gap-2">
          <Check className="w-4 h-4 text-green-400" />
          <span className="text-green-400 text-xs">Settings saved</span>
        </div>
      )}

      {/* Security Profile */}
      <div className="rounded-lg bg-neutral-800/50 border border-neutral-700/50 overflow-hidden">
        <div className="p-4 space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-neutral-300">Security Profile</label>
            <select
              value={config.default_profile}
              onChange={(e) =>
                setConfig((prev) => ({
                  ...prev,
                  default_profile: e.target.value,
                }))
              }
              className="w-full px-2.5 py-1.5 text-sm bg-neutral-700/50 text-white border border-neutral-600/50 rounded-md outline-none focus:ring-1 focus:ring-blue-500/50"
            >
              {profiles.map((profile) => (
                <option key={profile.name} value={profile.name}>
                  {profile.name.charAt(0).toUpperCase() + profile.name.slice(1)}
                </option>
              ))}
              <option value="custom">Custom</option>
            </select>
          </div>

          {activeProfile && (
            <div className="p-3 bg-neutral-700/30 rounded-md space-y-2">
              <p className="text-xs text-neutral-300">{activeProfile.description}</p>
              <div className="flex items-center gap-2">
                <span className="text-[10px] text-neutral-500 font-mono">AST Fingerprint:</span>
                <code className="text-[10px] text-blue-400 font-mono bg-neutral-800/50 px-1.5 py-0.5 rounded">
                  {activeProfile.fingerprint}
                </code>
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Features */}
      <div className="rounded-lg bg-neutral-800/50 border border-neutral-700/50 overflow-hidden">
        <div className="px-4 py-3 border-b border-neutral-700/50">
          <h4 className="text-sm font-medium text-neutral-200">Features</h4>
        </div>
        <div className="p-4 space-y-4">
          <div className="flex items-center justify-between">
            <div>
              <h4 className="text-xs font-medium text-neutral-300">Credential Proxy</h4>
              <p className="text-[10px] text-neutral-500">
                Replace API keys with placeholders; real keys injected by host-side proxy
              </p>
            </div>
            <ToggleSwitch
              checked={config.credential_proxy_enabled}
              onChange={(credential_proxy_enabled) =>
                setConfig((prev) => ({ ...prev, credential_proxy_enabled }))
              }
            />
          </div>

          <div className="flex items-center justify-between">
            <div>
              <h4 className="text-xs font-medium text-neutral-300">Network Mediation</h4>
              <p className="text-[10px] text-neutral-500">
                Route container network through filtering proxy with domain allowlisting
              </p>
            </div>
            <ToggleSwitch
              checked={config.network_proxy_enabled}
              onChange={(network_proxy_enabled) =>
                setConfig((prev) => ({ ...prev, network_proxy_enabled }))
              }
            />
          </div>

          <div className="flex items-center justify-between">
            <div>
              <h4 className="text-xs font-medium text-neutral-300">Security Audit Log</h4>
              <p className="text-[10px] text-neutral-500">
                Record policy decisions, network requests, and credential activity
              </p>
            </div>
            <ToggleSwitch
              checked={config.audit_enabled}
              onChange={(audit_enabled) => setConfig((prev) => ({ ...prev, audit_enabled }))}
            />
          </div>

          {config.audit_enabled && (
            <div className="space-y-1.5 pl-4 border-l-2 border-neutral-700">
              <label className="text-[10px] font-medium text-neutral-400">
                Audit Retention (days)
              </label>
              <input
                type="number"
                min={1}
                max={365}
                value={config.audit_retention_days}
                onChange={(e) =>
                  setConfig((prev) => ({
                    ...prev,
                    audit_retention_days: parseInt(e.target.value) || 30,
                  }))
                }
                className="w-24 px-2 py-1 text-xs bg-neutral-700/50 text-white border border-neutral-600/50 rounded-md outline-none focus:ring-1 focus:ring-blue-500/50"
              />
            </div>
          )}
        </div>
      </div>

      {/* Audit Log Viewer */}
      {config.audit_enabled && (
        <div className="rounded-lg bg-neutral-800/50 border border-neutral-700/50 overflow-hidden">
          <div className="px-4 py-3 border-b border-neutral-700/50 flex items-center justify-between">
            <div className="flex items-center gap-2">
              <FileSearch className="w-4 h-4 text-neutral-400" />
              <h4 className="text-sm font-medium text-neutral-200">Audit Log</h4>
              {auditSummary && (
                <div className="flex gap-1.5 ml-3">
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-500/20 text-green-400">
                    {auditSummary.allowed} allowed
                  </span>
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-red-500/20 text-red-400">
                    {auditSummary.denied} denied
                  </span>
                  {auditSummary.warnings > 0 && (
                    <span className="text-[10px] px-1.5 py-0.5 rounded bg-yellow-500/20 text-yellow-400">
                      {auditSummary.warnings} warnings
                    </span>
                  )}
                </div>
              )}
            </div>
            <div className="flex items-center gap-2">
              <select
                value={auditFilter}
                onChange={(e) => setAuditFilter(e.target.value)}
                className="text-[10px] px-1.5 py-1 bg-neutral-700/50 text-neutral-300 border border-neutral-600/50 rounded outline-none"
              >
                <option value="all">All</option>
                <option value="denied">Denied</option>
                <option value="allowed">Allowed</option>
                <option value="warning">Warnings</option>
              </select>
              <button
                onClick={loadAuditData}
                disabled={auditLoading}
                className="p-1 hover:bg-neutral-700 rounded transition-colors"
                title="Refresh audit log"
              >
                <RefreshCw
                  className={`w-3.5 h-3.5 text-neutral-400 ${auditLoading ? "animate-spin" : ""}`}
                />
              </button>
            </div>
          </div>
          <div className="max-h-64 overflow-y-auto">
            {auditEvents.length === 0 ? (
              <div className="p-4 text-center">
                <p className="text-xs text-neutral-500">
                  {auditLoading ? "Loading..." : "No audit events. Click refresh to load."}
                </p>
              </div>
            ) : (
              <table className="w-full text-[10px]">
                <thead className="sticky top-0 bg-neutral-800">
                  <tr className="text-neutral-500 border-b border-neutral-700/50">
                    <th className="px-3 py-1.5 text-left font-medium">Time</th>
                    <th className="px-3 py-1.5 text-left font-medium">Type</th>
                    <th className="px-3 py-1.5 text-left font-medium">Decision</th>
                    <th className="px-3 py-1.5 text-left font-medium">Action</th>
                    <th className="px-3 py-1.5 text-left font-medium">Reason</th>
                  </tr>
                </thead>
                <tbody>
                  {auditEvents.map((event) => (
                    <tr
                      key={event.id}
                      className="border-b border-neutral-700/30 hover:bg-neutral-700/20"
                    >
                      <td className="px-3 py-1.5 text-neutral-400 whitespace-nowrap">
                        {new Date(event.timestamp).toLocaleTimeString()}
                      </td>
                      <td className="px-3 py-1.5">
                        <span className="px-1 py-0.5 rounded bg-neutral-700/50 text-neutral-300">
                          {EVENT_TYPE_LABELS[event.event_type] || event.event_type}
                        </span>
                      </td>
                      <td className="px-3 py-1.5">
                        <span
                          className={`px-1.5 py-0.5 rounded ${DECISION_COLORS[event.decision] || "text-neutral-400"}`}
                        >
                          {event.decision}
                        </span>
                      </td>
                      <td
                        className="px-3 py-1.5 text-neutral-300 max-w-48 truncate"
                        title={event.action}
                      >
                        {event.action}
                      </td>
                      <td
                        className="px-3 py-1.5 text-neutral-500 max-w-48 truncate"
                        title={event.reason || ""}
                      >
                        {event.reason || "-"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        </div>
      )}

      {/* AST 7-Layer Info */}
      <div className="p-3 bg-blue-500/5 border border-blue-500/10 rounded-lg flex gap-2">
        <Info className="w-4 h-4 text-blue-400 shrink-0 mt-0.5" />
        <div className="text-xs text-blue-300/80 space-y-1">
          <p>
            Security policies follow the <strong>Agent Sandbox Taxonomy (AST)</strong> 7-layer
            model:
          </p>
          <p className="font-mono text-[10px] text-blue-400/60">
            L1:Compute / L2:Resources / L3:Filesystem / L4:Network / L5:Credentials / L6:Actions /
            L7:Audit
          </p>
        </div>
      </div>

      {/* Actions */}
      <div className="flex justify-between">
        <button
          onClick={() => setConfig(DEFAULT_SETTINGS)}
          className="px-4 py-2 bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded-md text-sm transition-colors"
        >
          Reset to Defaults
        </button>

        <button
          onClick={saveSettings}
          disabled={saving}
          className="px-6 py-2 bg-blue-600 hover:bg-blue-500 text-white rounded-md font-medium text-sm transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
        >
          {saving ? (
            <>
              <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
              Saving...
            </>
          ) : saveSuccess ? (
            <>
              <Check className="w-4 h-4" />
              Saved
            </>
          ) : (
            <>
              <Shield className="w-4 h-4" />
              Save Security Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}
