import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Terminal,
  AlertTriangle,
  RefreshCw,
  ShieldCheck,
  Plus,
  FolderSearch,
  Users,
  X,
} from "lucide-react";
import { getAccentColors } from "@/design-system";
import type {
  AiSettings,
  AccountUsageInfo,
  LogFunction,
  CliAuthStatus,
  TauriResult,
  ClaudeConfigDirsData,
  DiscoveredDir,
} from "./types";
import { EXECUTION_MODE_OPTIONS } from "./types";
import type { CliExecutionMode } from "../types";

interface ClaudeCliSectionProps {
  settings: AiSettings;
  setSettings: React.Dispatch<React.SetStateAction<AiSettings>>;
  onLog: LogFunction;
  cliAuth: CliAuthStatus | null;
  refreshingAuth: boolean;
  onRefreshAuth: () => void;
  onCheckAuth: () => void;
  claudeConfigDirs: string[];
  setClaudeConfigDirs: React.Dispatch<React.SetStateAction<string[]>>;
  accountUsages: AccountUsageInfo[];
  setAccountUsages: React.Dispatch<React.SetStateAction<AccountUsageInfo[]>>;
}

export function ClaudeCliSection({
  settings,
  setSettings,
  onLog,
  cliAuth,
  refreshingAuth,
  onRefreshAuth,
  onCheckAuth,
  claudeConfigDirs,
  setClaudeConfigDirs,
  accountUsages,
  setAccountUsages,
}: ClaudeCliSectionProps) {
  const [checkingUsage, setCheckingUsage] = useState(false);
  const [detectingDirs, setDetectingDirs] = useState(false);

  const saveClaudeConfigDirs = useCallback(
    async (dirs: string[]) => {
      try {
        const result = await invoke<TauriResult<ClaudeConfigDirsData>>("save_claude_config_dirs", {
          dirs,
        });
        if (result?.success && result.data) {
          setClaudeConfigDirs(result.data.dirs);
          onLog("success", `Saved ${result.data.dirs.length} Claude account directories`);
        } else {
          onLog("error", "Failed to save Claude account dirs");
        }
      } catch (err) {
        console.error("Failed to save Claude config dirs:", err);
        onLog("error", `Failed to save Claude account dirs: ${err}`);
      }
    },
    [onLog, setClaudeConfigDirs],
  );

  const handleRemoveClaudeDir = useCallback(
    (path: string) => {
      const updated = claudeConfigDirs.filter((d) => d !== path);
      saveClaudeConfigDirs(updated);
      setAccountUsages((prev) => prev.filter((a) => a.config_dir !== path));
    },
    [claudeConfigDirs, saveClaudeConfigDirs, setAccountUsages],
  );

  const handleAddClaudeDir = useCallback(async () => {
    const selected = await open({ directory: true, multiple: false });
    if (selected) {
      const path = selected as string;
      if (!claudeConfigDirs.includes(path)) {
        saveClaudeConfigDirs([...claudeConfigDirs, path]);
      }
    }
  }, [claudeConfigDirs, saveClaudeConfigDirs]);

  const handleAutoDetectClaudeDirs = useCallback(async () => {
    setDetectingDirs(true);
    try {
      const found = await invoke<DiscoveredDir[]>("discover_claude_config_dirs");
      const newPaths = found.map((d) => d.path);
      const merged = [...new Set([...claudeConfigDirs, ...newPaths])];
      await saveClaudeConfigDirs(merged);
    } catch (err) {
      console.error("Failed to auto-detect Claude config dirs:", err);
      onLog("error", `Auto-detect failed: ${err}`);
    } finally {
      setDetectingDirs(false);
    }
  }, [claudeConfigDirs, saveClaudeConfigDirs, onLog]);

  const checkAccountsUsage = async () => {
    if (claudeConfigDirs.length === 0) {
      onLog("warning", "No Claude accounts configured. Add directories below.");
      return;
    }
    try {
      setCheckingUsage(true);
      const result = await invoke<TauriResult<AccountUsageInfo[]>>("check_accounts_usage", {
        configDirs: claudeConfigDirs,
      });
      if (result?.success && result.data) {
        setAccountUsages(result.data);
        const best = result.data
          .filter((a) => !a.error)
          .sort((a, b) => a.utilization - b.utilization)[0];
        if (best) {
          onLog(
            "success",
            `Checked ${result.data.length} accounts. Least used: ${best.label} (${Math.round(best.utilization * 100)}%)`,
          );
        }
      }
    } catch (err) {
      console.error("Failed to check accounts usage:", err);
      onLog("error", `Failed to check accounts usage: ${err}`);
    } finally {
      setCheckingUsage(false);
    }
  };

  return (
    <div className="space-y-4 rounded-lg bg-card/50 p-4">
      <h4 className="font-medium text-sm flex items-center gap-2">
        <Terminal className="w-4 h-4 text-primary" />
        Claude Code CLI Settings
      </h4>

      {cliAuth && (
        <div
          className={`p-3 rounded-lg flex items-start gap-2 ${
            cliAuth.expired
              ? `${getAccentColors("red").bg}`
              : cliAuth.has_credentials
                ? `${getAccentColors("green").bg}`
                : "bg-muted/50"
          }`}
        >
          {cliAuth.expired ? (
            <AlertTriangle className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
          ) : cliAuth.has_credentials ? (
            <ShieldCheck className={`w-4 h-4 ${getAccentColors("green").text} shrink-0 mt-0.5`} />
          ) : null}
          <div className="flex-1 space-y-1.5">
            <div
              className={`text-xs font-medium ${
                cliAuth.expired
                  ? getAccentColors("red").text
                  : cliAuth.has_credentials
                    ? getAccentColors("green").text
                    : "text-muted-foreground"
              }`}
            >
              {cliAuth.expired
                ? "Authentication expired \u2014 workflows will fail"
                : cliAuth.has_credentials
                  ? `Authenticated${cliAuth.subscription_type ? ` (${cliAuth.subscription_type})` : ""}`
                  : "No credentials found"}
            </div>
            {cliAuth.has_credentials &&
              !cliAuth.expired &&
              cliAuth.minutes_until_expiry != null && (
                <div className="text-[10px] text-muted-foreground">
                  {cliAuth.minutes_until_expiry > 60
                    ? `Expires in ${Math.round(cliAuth.minutes_until_expiry / 60)} hours`
                    : `Expires in ${cliAuth.minutes_until_expiry} minutes`}
                </div>
              )}
            {(cliAuth.expired || !cliAuth.has_credentials) && (
              <button
                onClick={onRefreshAuth}
                disabled={refreshingAuth}
                className="mt-1 inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 transition-colors"
              >
                <RefreshCw className={`w-3 h-3 ${refreshingAuth ? "animate-spin" : ""}`} />
                {refreshingAuth ? "Opening..." : "Re-authenticate"}
              </button>
            )}
          </div>
          {cliAuth.has_credentials && !cliAuth.expired && (
            <button
              onClick={onCheckAuth}
              className="text-xs text-muted-foreground hover:text-foreground transition-colors"
              title="Refresh auth status"
            >
              <RefreshCw className="w-3.5 h-3.5" />
            </button>
          )}
        </div>
      )}

      <div className="space-y-4">
        <div className="space-y-1.5">
          <label htmlFor="claude-cli-execution-mode" className="text-xs font-medium">
            Execution Mode
          </label>
          <select
            id="claude-cli-execution-mode"
            value={settings.claude_cli.execution_mode}
            onChange={(e) =>
              setSettings((prev) => ({
                ...prev,
                claude_cli: {
                  ...prev.claude_cli,
                  execution_mode: e.target.value as CliExecutionMode,
                },
              }))
            }
            className="w-full px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm"
          >
            {EXECUTION_MODE_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
          <p className="text-[10px] text-muted-foreground">
            {
              EXECUTION_MODE_OPTIONS.find((o) => o.value === settings.claude_cli.execution_mode)
                ?.description
            }
          </p>
        </div>

        <div className="space-y-1.5">
          <label htmlFor="claude-cli-custom-path" className="text-xs font-medium">
            Custom Executable Path (Optional)
          </label>
          <input
            id="claude-cli-custom-path"
            type="text"
            value={settings.claude_cli.custom_path || ""}
            onChange={(e) =>
              setSettings((prev) => ({
                ...prev,
                claude_cli: {
                  ...prev.claude_cli,
                  custom_path: e.target.value || undefined,
                },
              }))
            }
            placeholder="Leave empty to use default (claude or claude.exe)"
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
          />
          <p className="text-[10px] text-muted-foreground">
            Specify a custom path to the Claude Code executable if it's not in your PATH.
          </p>
        </div>

        {/* Claude Accounts */}
        <div className="space-y-3 border-t border-border/50 pt-4 mt-2">
          <div className="flex items-center gap-2">
            <Users className="w-4 h-4 text-primary" />
            <span className="text-xs font-medium">Claude Accounts</span>
          </div>

          <p className="text-[10px] text-muted-foreground">
            Manage Claude Code config directories. Each directory represents a separate account and
            must contain a <code>projects/</code> subdirectory.
          </p>

          <div className="space-y-1.5">
            {claudeConfigDirs.length === 0 ? (
              <div className="text-xs text-muted-foreground text-center py-3 bg-muted/20 rounded-lg">
                No accounts configured. Add directories or use Auto-Detect.
              </div>
            ) : (
              claudeConfigDirs.map((dir) => {
                const usage = accountUsages.find((a) => a.config_dir === dir);
                const label = dir.split(/[\\/]/).filter(Boolean).pop() || dir;
                const isSelected =
                  settings.claude_cli.account_selection_mode !== "least_usage" &&
                  settings.claude_cli.config_dir === dir;
                const isBest =
                  settings.claude_cli.account_selection_mode === "least_usage" &&
                  accountUsages.length > 0 &&
                  accountUsages
                    .filter((a) => !a.error)
                    .sort((a, b) => a.utilization - b.utilization)[0]?.config_dir === dir;

                return (
                  <div
                    key={dir}
                    className={`p-2.5 rounded-lg group transition-colors ${
                      isBest
                        ? `${getAccentColors("green").bg} ring-1 ring-green-500/30`
                        : isSelected
                          ? "bg-primary/10 ring-1 ring-primary/30"
                          : "bg-muted/30 hover:bg-muted/40"
                    }`}
                  >
                    <div className="flex items-center gap-2">
                      {settings.claude_cli.account_selection_mode !== "least_usage" && (
                        <input
                          type="radio"
                          name="manual_account"
                          checked={settings.claude_cli.config_dir === dir}
                          onChange={() =>
                            setSettings((prev) => ({
                              ...prev,
                              claude_cli: {
                                ...prev.claude_cli,
                                config_dir: dir,
                              },
                            }))
                          }
                          className="accent-primary shrink-0"
                        />
                      )}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="text-xs font-medium truncate">{label}</span>
                          {isBest && (
                            <span
                              className={`text-[10px] px-1.5 py-0.5 rounded-full ${getAccentColors("green").bg} ${getAccentColors("green").text} font-medium shrink-0`}
                            >
                              Auto-selected
                            </span>
                          )}
                          {isSelected && (
                            <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-primary/20 text-primary font-medium shrink-0">
                              Selected
                            </span>
                          )}
                        </div>
                        <div className="text-[10px] text-muted-foreground truncate">{dir}</div>
                      </div>

                      {usage && !usage.error && (
                        <div className="flex items-center gap-1.5 shrink-0">
                          <div className="w-16 h-1.5 bg-muted/50 rounded-full overflow-hidden">
                            <div
                              className={`h-full rounded-full transition-all ${
                                usage.utilization >= 0.8
                                  ? "bg-red-500"
                                  : usage.utilization >= 0.5
                                    ? "bg-yellow-500"
                                    : "bg-green-500"
                              }`}
                              style={{
                                width: `${Math.max(Math.round(usage.utilization * 100), 2)}%`,
                              }}
                            />
                          </div>
                          <span
                            className={`text-[10px] font-mono w-8 text-right ${
                              usage.utilization >= 0.8
                                ? getAccentColors("red").text
                                : usage.utilization >= 0.5
                                  ? getAccentColors("yellow").text
                                  : getAccentColors("green").text
                            }`}
                          >
                            {Math.round(usage.utilization * 100)}%
                          </span>
                        </div>
                      )}
                      {usage?.error && (
                        <span className={`text-[10px] ${getAccentColors("red").text} shrink-0`}>
                          Error
                        </span>
                      )}

                      <button
                        onClick={() => handleRemoveClaudeDir(dir)}
                        className="text-zinc-500 hover:text-red-400 transition-colors opacity-0 group-hover:opacity-100 shrink-0"
                        title="Remove account"
                      >
                        <X className="w-3.5 h-3.5" />
                      </button>
                    </div>

                    {usage?.resets_at && !usage.error && (
                      <div className="text-[10px] text-muted-foreground mt-1 ml-6">
                        Weekly limit resets {new Date(usage.resets_at * 1000).toLocaleString()}
                      </div>
                    )}
                    {usage?.error && (
                      <div
                        className={`text-[10px] ${getAccentColors("red").text} mt-1 ml-6 truncate`}
                      >
                        {usage.error}
                      </div>
                    )}
                  </div>
                );
              })
            )}
          </div>

          <div className="flex flex-wrap gap-2">
            <button
              className="px-2.5 py-1.5 text-xs rounded-md bg-muted/50 hover:bg-muted/70 transition-colors flex items-center gap-1.5"
              onClick={handleAddClaudeDir}
            >
              <Plus className="w-3 h-3" />
              Add Directory
            </button>
            <button
              className="px-2.5 py-1.5 text-xs rounded-md bg-muted/50 hover:bg-muted/70 transition-colors flex items-center gap-1.5"
              onClick={handleAutoDetectClaudeDirs}
              disabled={detectingDirs}
            >
              <FolderSearch className={`w-3 h-3 ${detectingDirs ? "animate-pulse" : ""}`} />
              {detectingDirs ? "Detecting..." : "Auto-Detect"}
            </button>
            {claudeConfigDirs.length > 0 && (
              <button
                onClick={checkAccountsUsage}
                disabled={checkingUsage}
                className="px-2.5 py-1.5 text-xs rounded-md bg-primary/10 hover:bg-primary/20 text-primary transition-colors flex items-center gap-1.5 disabled:opacity-50"
              >
                <RefreshCw className={`w-3 h-3 ${checkingUsage ? "animate-spin" : ""}`} />
                {checkingUsage ? "Checking..." : "Check Usage"}
              </button>
            )}
          </div>

          {claudeConfigDirs.length > 1 && (
            <div className="space-y-2 pt-2">
              <span className="text-[10px] font-medium text-muted-foreground uppercase tracking-wide">
                Selection Mode
              </span>
              <div className="grid grid-cols-2 gap-2">
                <button
                  onClick={() =>
                    setSettings((prev) => ({
                      ...prev,
                      claude_cli: {
                        ...prev.claude_cli,
                        account_selection_mode: "manual",
                      },
                    }))
                  }
                  className={`p-2.5 rounded-lg text-left transition-colors ${
                    settings.claude_cli.account_selection_mode !== "least_usage"
                      ? "bg-primary/10 ring-1 ring-primary/30"
                      : "bg-muted/30 hover:bg-muted/50"
                  }`}
                >
                  <div className="text-xs font-medium">Manual</div>
                  <div className="text-[10px] text-muted-foreground">Select account above</div>
                </button>
                <button
                  onClick={() =>
                    setSettings((prev) => ({
                      ...prev,
                      claude_cli: {
                        ...prev.claude_cli,
                        account_selection_mode: "least_usage",
                      },
                    }))
                  }
                  className={`p-2.5 rounded-lg text-left transition-colors ${
                    settings.claude_cli.account_selection_mode === "least_usage"
                      ? "bg-primary/10 ring-1 ring-primary/30"
                      : "bg-muted/30 hover:bg-muted/50"
                  }`}
                >
                  <div className="text-xs font-medium">Least Usage</div>
                  <div className="text-[10px] text-muted-foreground">Auto-select on startup</div>
                </button>
              </div>
            </div>
          )}

          {settings.claude_cli.account_selection_mode !== "least_usage" && (
            <div className="space-y-1">
              <label
                htmlFor="claude-cli-custom-config-dir"
                className="text-[10px] text-muted-foreground"
              >
                Or enter a custom config directory:
              </label>
              <input
                id="claude-cli-custom-config-dir"
                type="text"
                value={
                  claudeConfigDirs.includes(settings.claude_cli.config_dir || "")
                    ? ""
                    : settings.claude_cli.config_dir || ""
                }
                onChange={(e) =>
                  setSettings((prev) => ({
                    ...prev,
                    claude_cli: {
                      ...prev.claude_cli,
                      config_dir: e.target.value || undefined,
                    },
                  }))
                }
                placeholder="Leave empty to use selection above"
                className="w-full px-2.5 py-1.5 text-xs bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
              />
            </div>
          )}
        </div>

        <div className="space-y-1.5">
          <label htmlFor="claude-cli-timeout" className="text-xs font-medium">
            Timeout (seconds)
          </label>
          <input
            id="claude-cli-timeout"
            type="number"
            min="60"
            max="3600"
            value={settings.claude_cli.timeout_seconds}
            onChange={(e) =>
              setSettings((prev) => ({
                ...prev,
                claude_cli: {
                  ...prev.claude_cli,
                  timeout_seconds: Math.max(60, Math.min(3600, parseInt(e.target.value) || 600)),
                },
              }))
            }
            className="w-32 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
          />
          <p className="text-[10px] text-muted-foreground">
            Maximum time to wait for Claude Code to respond (60-3600 seconds).
          </p>
        </div>
      </div>
    </div>
  );
}
