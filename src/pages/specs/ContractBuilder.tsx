/**
 * ContractBuilder — Author and edit .contract.uibridge.json files
 *
 * Form-based UI for creating VerificationContracts with:
 * - Contract name, description
 * - Preconditions list (add/remove, check type, onFailure)
 * - Verification section (specGroupRef or inline)
 * - Postconditions list
 *
 * Generates valid ContractConfig JSON and saves/loads via Tauri file APIs.
 */

import { useState, useCallback, useMemo, useReducer } from "react";
import {
  Plus,
  Trash2,
  Save,
  FolderOpen,
  FileJson,
  ChevronDown,
  ChevronRight,
  AlertCircle,
  CheckCircle2,
  Copy,
} from "lucide-react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";
import type {
  VerificationContract,
  ContractConfig,
  ContractCondition,
  ContractCheck,
  ContractVerification,
} from "@qontinui/ui-bridge/contracts";
import { CONTRACT_CONFIG_VERSION, validateContractConfig } from "@qontinui/ui-bridge/contracts";
import type { SpecAssertion } from "ui-bridge";
import { getAllSpecs } from "@/lib/spec-registry";

// =============================================================================
// State management
// =============================================================================

interface ContractBuilderState {
  config: ContractConfig;
  activeContractIndex: number;
  expandedSections: Set<string>;
  validationErrors: string[];
  saveStatus: "idle" | "saving" | "saved" | "error";
  saveError: string | null;
  filePath: string | null;
  jsonPreviewOpen: boolean;
}

type ContractBuilderAction =
  | { type: "SET_CONFIG"; config: ContractConfig }
  | { type: "SET_ACTIVE_CONTRACT"; index: number }
  | { type: "TOGGLE_SECTION"; section: string }
  | { type: "SET_VALIDATION_ERRORS"; errors: string[] }
  | { type: "SET_SAVE_STATUS"; status: ContractBuilderState["saveStatus"]; error?: string }
  | { type: "SET_FILE_PATH"; path: string | null }
  | { type: "TOGGLE_JSON_PREVIEW" }
  | { type: "UPDATE_CONTRACT"; index: number; contract: VerificationContract }
  | { type: "ADD_CONTRACT" }
  | { type: "REMOVE_CONTRACT"; index: number };

function createEmptyCondition(): ContractCondition {
  return {
    id: crypto.randomUUID().slice(0, 8),
    description: "",
    check: { type: "api", endpoint: "", method: "GET", expectedStatus: 200 },
    onFailure: "fail",
  };
}

function createEmptyContract(): VerificationContract {
  return {
    id: crypto.randomUUID().slice(0, 8),
    name: "",
    description: "",
    version: CONTRACT_CONFIG_VERSION,
    preconditions: [],
    verification: { type: "specGroupRef", specId: "", groupId: "" },
    postconditions: [],
  };
}

function createEmptyConfig(): ContractConfig {
  return {
    version: "1.0.0",
    contracts: [createEmptyContract()],
  };
}

function builderReducer(
  state: ContractBuilderState,
  action: ContractBuilderAction,
): ContractBuilderState {
  switch (action.type) {
    case "SET_CONFIG":
      return { ...state, config: action.config, validationErrors: [] };
    case "SET_ACTIVE_CONTRACT":
      return { ...state, activeContractIndex: action.index };
    case "TOGGLE_SECTION": {
      const next = new Set(state.expandedSections);
      if (next.has(action.section)) next.delete(action.section);
      else next.add(action.section);
      return { ...state, expandedSections: next };
    }
    case "SET_VALIDATION_ERRORS":
      return { ...state, validationErrors: action.errors };
    case "SET_SAVE_STATUS":
      return { ...state, saveStatus: action.status, saveError: action.error ?? null };
    case "SET_FILE_PATH":
      return { ...state, filePath: action.path };
    case "TOGGLE_JSON_PREVIEW":
      return { ...state, jsonPreviewOpen: !state.jsonPreviewOpen };
    case "UPDATE_CONTRACT": {
      const contracts = [...state.config.contracts];
      contracts[action.index] = action.contract;
      return { ...state, config: { ...state.config, contracts } };
    }
    case "ADD_CONTRACT": {
      const contracts = [...state.config.contracts, createEmptyContract()];
      return {
        ...state,
        config: { ...state.config, contracts },
        activeContractIndex: contracts.length - 1,
      };
    }
    case "REMOVE_CONTRACT": {
      const contracts = state.config.contracts.filter(
        (_: VerificationContract, i: number) => i !== action.index,
      );
      const newIndex = Math.min(state.activeContractIndex, Math.max(0, contracts.length - 1));
      return {
        ...state,
        config: { ...state.config, contracts },
        activeContractIndex: newIndex,
      };
    }
  }
}

const INITIAL_STATE: ContractBuilderState = {
  config: createEmptyConfig(),
  activeContractIndex: 0,
  expandedSections: new Set(["preconditions", "verification", "postconditions"]),
  validationErrors: [],
  saveStatus: "idle",
  saveError: null,
  filePath: null,
  jsonPreviewOpen: false,
};

// =============================================================================
// Sub-components
// =============================================================================

function SectionHeader({
  label,
  count,
  expanded,
  onToggle,
  onAdd,
}: {
  label: string;
  count: number;
  expanded: boolean;
  onToggle: () => void;
  onAdd?: () => void;
}) {
  return (
    <div className="flex items-center gap-2 py-1.5">
      <button
        onClick={onToggle}
        className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground hover:text-foreground"
      >
        {expanded ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        {label}
        <span className="text-[10px] font-normal text-muted-foreground/60">({count})</span>
      </button>
      {onAdd && (
        <button
          onClick={onAdd}
          className="ml-auto flex items-center gap-1 px-2 py-0.5 text-[10px] font-medium rounded
            bg-emerald-500/10 text-emerald-400 border border-emerald-500/20
            hover:bg-emerald-500/20 transition-colors"
        >
          <Plus className="w-3 h-3" />
          Add
        </button>
      )}
    </div>
  );
}

function CheckTypeEditor({
  check,
  onChange,
}: {
  check: ContractCheck;
  onChange: (check: ContractCheck) => void;
}) {
  if (check.type === "api") {
    return (
      <div className="grid grid-cols-[1fr_auto_auto] gap-2">
        <input
          type="text"
          value={check.endpoint}
          onChange={(e) => onChange({ ...check, endpoint: e.target.value })}
          placeholder="https://api.example.com/health"
          className="px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground
            placeholder:text-muted-foreground/50 focus:outline-hidden focus:border-cyan-500/50"
        />
        <select
          value={check.method}
          onChange={(e) =>
            onChange({ ...check, method: e.target.value as "GET" | "POST" | "PUT" | "DELETE" })
          }
          className="px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground"
        >
          <option value="GET">GET</option>
          <option value="POST">POST</option>
          <option value="PUT">PUT</option>
          <option value="DELETE">DELETE</option>
        </select>
        <input
          type="number"
          value={check.expectedStatus}
          onChange={(e) => onChange({ ...check, expectedStatus: parseInt(e.target.value) || 200 })}
          className="w-20 px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground
            focus:outline-hidden focus:border-cyan-500/50"
          placeholder="200"
        />
      </div>
    );
  }

  // assertion type: simplified inline editor
  const assertion = check.assertion;
  return (
    <div className="space-y-2">
      <input
        type="text"
        value={assertion.description || ""}
        onChange={(e) =>
          onChange({
            ...check,
            assertion: { ...assertion, description: e.target.value },
          })
        }
        placeholder="Assertion description"
        className="w-full px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground
          placeholder:text-muted-foreground/50 focus:outline-hidden focus:border-cyan-500/50"
      />
      <div className="grid grid-cols-2 gap-2">
        <select
          value={assertion.assertionType || "exists"}
          onChange={(e) =>
            onChange({
              ...check,
              assertion: {
                ...assertion,
                assertionType: e.target.value as SpecAssertion["assertionType"],
              },
            })
          }
          className="px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground"
        >
          <option value="exists">exists</option>
          <option value="visible">visible</option>
          <option value="hasText">hasText</option>
          <option value="enabled">enabled</option>
          <option value="checked">checked</option>
        </select>
        <select
          value={assertion.severity || "warning"}
          onChange={(e) =>
            onChange({
              ...check,
              assertion: { ...assertion, severity: e.target.value as SpecAssertion["severity"] },
            })
          }
          className="px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground"
        >
          <option value="critical">critical</option>
          <option value="warning">warning</option>
          <option value="info">info</option>
        </select>
      </div>
    </div>
  );
}

function ConditionEditor({
  condition,
  onChange,
  onRemove,
}: {
  condition: ContractCondition;
  onChange: (condition: ContractCondition) => void;
  onRemove: () => void;
}) {
  const checkType = condition.check.type;

  return (
    <div className="border border-white/5 rounded-lg bg-white/[0.02] p-3 space-y-2">
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={condition.id}
          onChange={(e) => onChange({ ...condition, id: e.target.value })}
          className="w-24 px-2 py-1 text-[10px] font-mono rounded bg-white/5 border border-white/10
            text-muted-foreground focus:outline-hidden focus:border-cyan-500/50"
          placeholder="id"
        />
        <input
          type="text"
          value={condition.description}
          onChange={(e) => onChange({ ...condition, description: e.target.value })}
          placeholder="Condition description"
          className="flex-1 px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground
            placeholder:text-muted-foreground/50 focus:outline-hidden focus:border-cyan-500/50"
        />
        <button
          onClick={onRemove}
          className="p-1 rounded text-red-400/60 hover:text-red-400 hover:bg-red-500/10 transition-colors"
        >
          <Trash2 className="w-3.5 h-3.5" />
        </button>
      </div>

      <div className="flex items-center gap-2">
        <label className="text-[10px] text-muted-foreground shrink-0">Check:</label>
        <select
          value={checkType}
          onChange={(e) => {
            const newType = e.target.value as "assertion" | "api";
            if (newType === "api") {
              onChange({
                ...condition,
                check: { type: "api", endpoint: "", method: "GET", expectedStatus: 200 },
              });
            } else {
              onChange({
                ...condition,
                check: {
                  type: "assertion",
                  assertion: {
                    id: crypto.randomUUID().slice(0, 8),
                    description: "",
                    category: "element-presence",
                    severity: "warning",
                    target: { type: "search", criteria: { textContent: "" } },
                    assertionType: "exists",
                    source: "manual",
                    reviewed: false,
                    enabled: true,
                  } as SpecAssertion,
                },
              });
            }
          }}
          className="px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground"
        >
          <option value="api">API</option>
          <option value="assertion">Assertion</option>
        </select>

        <label className="text-[10px] text-muted-foreground shrink-0 ml-2">On Failure:</label>
        <select
          value={condition.onFailure}
          onChange={(e) =>
            onChange({ ...condition, onFailure: e.target.value as "skip" | "fail" | "warn" })
          }
          className="px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground"
        >
          <option value="fail">fail</option>
          <option value="skip">skip</option>
          <option value="warn">warn</option>
        </select>
      </div>

      <CheckTypeEditor
        check={condition.check}
        onChange={(check) => onChange({ ...condition, check })}
      />
    </div>
  );
}

function VerificationEditor({
  verification,
  onChange,
  specOptions,
}: {
  verification: ContractVerification;
  onChange: (v: ContractVerification) => void;
  specOptions: Array<{ specId: string; groups: Array<{ id: string; name: string }> }>;
}) {
  const isRef = verification.type === "specGroupRef";

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-3">
        <label className="flex items-center gap-1.5 text-xs">
          <input
            type="radio"
            name="verificationType"
            checked={isRef}
            onChange={() => onChange({ type: "specGroupRef", specId: "", groupId: "" })}
            className="accent-purple-500"
          />
          Spec Group Ref
        </label>
        <label className="flex items-center gap-1.5 text-xs">
          <input
            type="radio"
            name="verificationType"
            checked={!isRef}
            onChange={() => onChange({ type: "inline", assertions: [] })}
            className="accent-purple-500"
          />
          Inline Assertions
        </label>
      </div>

      {isRef && verification.type === "specGroupRef" && (
        <div className="grid grid-cols-2 gap-2">
          <div>
            <label className="text-[10px] text-muted-foreground block mb-1">Spec ID</label>
            <select
              value={verification.specId}
              onChange={(e) => {
                const specId = e.target.value;
                onChange({ ...verification, specId, groupId: "" });
              }}
              className="w-full px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground"
            >
              <option value="">-- select spec --</option>
              {specOptions.map((s) => (
                <option key={s.specId} value={s.specId}>
                  {s.specId}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label className="text-[10px] text-muted-foreground block mb-1">Group ID</label>
            <select
              value={verification.groupId}
              onChange={(e) => onChange({ ...verification, groupId: e.target.value })}
              className="w-full px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground"
            >
              <option value="">-- select group --</option>
              {specOptions
                .find((s) => s.specId === verification.specId)
                ?.groups.map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.name} ({g.id})
                  </option>
                ))}
            </select>
          </div>
        </div>
      )}

      {!isRef && verification.type === "inline" && (
        <div className="space-y-2">
          <div className="text-[10px] text-muted-foreground">
            Inline assertions ({verification.assertions.length})
          </div>
          {verification.assertions.map((assertion: SpecAssertion, idx: number) => (
            <div key={assertion.id} className="border border-white/5 rounded p-2 space-y-1">
              <div className="flex items-center gap-2">
                <input
                  type="text"
                  value={assertion.description}
                  onChange={(e: React.ChangeEvent<HTMLInputElement>) => {
                    const assertions = [...verification.assertions];
                    assertions[idx] = { ...assertions[idx], description: e.target.value };
                    onChange({ ...verification, assertions });
                  }}
                  placeholder="Assertion description"
                  className="flex-1 px-2 py-1 text-xs rounded bg-white/5 border border-white/10 text-foreground
                    placeholder:text-muted-foreground/50 focus:outline-hidden focus:border-cyan-500/50"
                />
                <button
                  onClick={() => {
                    const assertions = verification.assertions.filter(
                      (_: SpecAssertion, i: number) => i !== idx,
                    );
                    onChange({ ...verification, assertions });
                  }}
                  className="p-1 rounded text-red-400/60 hover:text-red-400 hover:bg-red-500/10 transition-colors"
                >
                  <Trash2 className="w-3 h-3" />
                </button>
              </div>
            </div>
          ))}
          <button
            onClick={() => {
              const newAssertion: SpecAssertion = {
                id: crypto.randomUUID().slice(0, 8),
                description: "",
                category: "element-presence",
                severity: "warning",
                target: { type: "search", criteria: { textContent: "" } },
                assertionType: "exists",
                source: "manual",
                reviewed: false,
                enabled: true,
              };
              onChange({
                ...verification,
                assertions: [...verification.assertions, newAssertion],
              });
            }}
            className="flex items-center gap-1 px-2 py-1 text-[10px] font-medium rounded
              bg-purple-500/10 text-purple-400 border border-purple-500/20
              hover:bg-purple-500/20 transition-colors"
          >
            <Plus className="w-3 h-3" />
            Add Assertion
          </button>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Main Component
// =============================================================================

export function ContractBuilder() {
  const [state, dispatch] = useReducer(builderReducer, INITIAL_STATE);
  const {
    config,
    activeContractIndex,
    expandedSections,
    validationErrors,
    saveStatus,
    saveError,
    filePath,
    jsonPreviewOpen,
  } = state;

  // Load spec IDs + group IDs from the spec registry
  const specOptions = useMemo(() => {
    const specs = getAllSpecs();
    return specs.map((spec) => ({
      specId: spec.specId,
      groups: (spec.config.groups || []).map((g: { id: string; name: string }) => ({
        id: g.id,
        name: g.name,
      })),
    }));
  }, []);

  const activeContract = config.contracts[activeContractIndex] ?? null;

  const updateActiveContract = useCallback(
    (contract: VerificationContract) => {
      dispatch({ type: "UPDATE_CONTRACT", index: activeContractIndex, contract });
    },
    [activeContractIndex],
  );

  // ---- Validation ----
  const validate = useCallback((): boolean => {
    const result = validateContractConfig(config);
    if (!result.valid) {
      dispatch({
        type: "SET_VALIDATION_ERRORS",
        errors: result.errors.map(
          (e: { path: string; message: string }) => `${e.path}: ${e.message}`,
        ),
      });
      return false;
    }
    dispatch({ type: "SET_VALIDATION_ERRORS", errors: [] });
    return true;
  }, [config]);

  // ---- File save ----
  const handleSave = useCallback(async () => {
    if (!validate()) return;

    dispatch({ type: "SET_SAVE_STATUS", status: "saving" });
    try {
      let savePath = filePath;
      if (!savePath) {
        const contractName = activeContract?.name?.replace(/\s+/g, "-").toLowerCase() || "contract";
        savePath = await save({
          defaultPath: `${contractName}.contract.uibridge.json`,
          filters: [{ name: "Contract Files", extensions: ["json"] }],
        });
      }
      if (!savePath) {
        dispatch({ type: "SET_SAVE_STATUS", status: "idle" });
        return;
      }

      const json = JSON.stringify(config, null, 2);
      await writeTextFile(savePath, json);
      dispatch({ type: "SET_FILE_PATH", path: savePath });
      dispatch({ type: "SET_SAVE_STATUS", status: "saved" });

      // Reset status after 2s
      setTimeout(() => dispatch({ type: "SET_SAVE_STATUS", status: "idle" }), 2000);
    } catch (err) {
      dispatch({
        type: "SET_SAVE_STATUS",
        status: "error",
        error: err instanceof Error ? err.message : "Save failed",
      });
    }
  }, [config, filePath, validate, activeContract]);

  // ---- File load ----
  const handleLoad = useCallback(async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "Contract Files", extensions: ["json"] }],
      });
      if (!selected) return;

      const content = await readTextFile(selected);
      const data = JSON.parse(content) as ContractConfig;
      const result = validateContractConfig(data);

      if (!result.valid) {
        dispatch({
          type: "SET_VALIDATION_ERRORS",
          errors: result.errors.map(
            (e: { path: string; message: string }) => `${e.path}: ${e.message}`,
          ),
        });
        return;
      }

      dispatch({ type: "SET_CONFIG", config: data });
      dispatch({ type: "SET_FILE_PATH", path: selected });
      dispatch({ type: "SET_ACTIVE_CONTRACT", index: 0 });
    } catch (err) {
      dispatch({
        type: "SET_SAVE_STATUS",
        status: "error",
        error: err instanceof Error ? err.message : "Load failed",
      });
    }
  }, []);

  // ---- Copy JSON ----
  const handleCopyJson = useCallback(() => {
    const json = JSON.stringify(config, null, 2);
    navigator.clipboard.writeText(json);
  }, [config]);

  // ---- Condition updaters ----
  const updatePrecondition = useCallback(
    (index: number, condition: ContractCondition) => {
      if (!activeContract) return;
      const preconditions = [...activeContract.preconditions];
      preconditions[index] = condition;
      updateActiveContract({ ...activeContract, preconditions });
    },
    [activeContract, updateActiveContract],
  );

  const removePrecondition = useCallback(
    (index: number) => {
      if (!activeContract) return;
      updateActiveContract({
        ...activeContract,
        preconditions: activeContract.preconditions.filter(
          (_: ContractCondition, i: number) => i !== index,
        ),
      });
    },
    [activeContract, updateActiveContract],
  );

  const updatePostcondition = useCallback(
    (index: number, condition: ContractCondition) => {
      if (!activeContract) return;
      const postconditions = [...activeContract.postconditions];
      postconditions[index] = condition;
      updateActiveContract({ ...activeContract, postconditions });
    },
    [activeContract, updateActiveContract],
  );

  const removePostcondition = useCallback(
    (index: number) => {
      if (!activeContract) return;
      updateActiveContract({
        ...activeContract,
        postconditions: activeContract.postconditions.filter(
          (_: ContractCondition, i: number) => i !== index,
        ),
      });
    },
    [activeContract, updateActiveContract],
  );

  // ---- Render ----
  return (
    <div className="h-full flex flex-col">
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-4 py-2 border-b border-border bg-white/[0.01]">
        <FileJson className="w-4 h-4 text-amber-400" />
        <span className="text-xs font-semibold">Contract Builder</span>

        {filePath && (
          <span
            className="text-[10px] text-muted-foreground/60 truncate max-w-[200px]"
            title={filePath}
          >
            {filePath.replace(/\\/g, "/").split("/").pop()}
          </span>
        )}

        <div className="ml-auto flex items-center gap-1.5">
          <button
            onClick={handleLoad}
            className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
              bg-emerald-500/10 text-emerald-400 border border-emerald-500/20
              hover:bg-emerald-500/20 transition-colors"
          >
            <FolderOpen className="w-3.5 h-3.5" />
            Load
          </button>
          <button
            onClick={handleSave}
            disabled={saveStatus === "saving"}
            className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
              bg-purple-500/10 text-purple-400 border border-purple-500/20
              hover:bg-purple-500/20 disabled:opacity-50 transition-colors"
          >
            <Save className="w-3.5 h-3.5" />
            {saveStatus === "saving" ? "Saving..." : saveStatus === "saved" ? "Saved" : "Save"}
          </button>
          <button
            onClick={() => dispatch({ type: "TOGGLE_JSON_PREVIEW" })}
            className={`flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
              border transition-colors ${
                jsonPreviewOpen
                  ? "bg-cyan-500/15 text-cyan-400 border-cyan-500/30"
                  : "bg-white/5 text-muted-foreground border-white/10 hover:bg-white/10"
              }`}
          >
            <FileJson className="w-3.5 h-3.5" />
            JSON
          </button>
        </div>
      </div>

      {/* Validation errors */}
      {validationErrors.length > 0 && (
        <div className="px-4 py-2 border-b border-red-500/20 bg-red-500/5">
          <div className="flex items-start gap-2">
            <AlertCircle className="w-3.5 h-3.5 text-red-400 shrink-0 mt-0.5" />
            <div className="space-y-0.5">
              {validationErrors.map((err, i) => (
                <p key={i} className="text-[10px] text-red-400">
                  {err}
                </p>
              ))}
            </div>
          </div>
        </div>
      )}

      {/* Save error */}
      {saveError && (
        <div className="px-4 py-2 border-b border-red-500/20 bg-red-500/5">
          <div className="flex items-center gap-2">
            <AlertCircle className="w-3.5 h-3.5 text-red-400" />
            <p className="text-[10px] text-red-400">{saveError}</p>
          </div>
        </div>
      )}

      {/* Save success */}
      {saveStatus === "saved" && (
        <div className="px-4 py-1.5 border-b border-green-500/20 bg-green-500/5">
          <div className="flex items-center gap-2">
            <CheckCircle2 className="w-3.5 h-3.5 text-green-400" />
            <p className="text-[10px] text-green-400">Contract saved successfully</p>
          </div>
        </div>
      )}

      <div className="flex-1 flex min-h-0">
        {/* Contract list sidebar */}
        <div className="w-48 shrink-0 border-r border-border overflow-y-auto">
          <div className="px-3 py-2 border-b border-border flex items-center justify-between">
            <h2 className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              Contracts
            </h2>
            <button
              onClick={() => dispatch({ type: "ADD_CONTRACT" })}
              className="p-1 rounded text-emerald-400/60 hover:text-emerald-400 hover:bg-emerald-500/10 transition-colors"
              title="Add contract"
            >
              <Plus className="w-3.5 h-3.5" />
            </button>
          </div>
          {config.contracts.map((contract: VerificationContract, idx: number) => (
            <div
              key={contract.id}
              onClick={() => dispatch({ type: "SET_ACTIVE_CONTRACT", index: idx })}
              className={`flex items-center gap-2 px-3 py-2 text-xs cursor-pointer border-b border-white/[0.03] transition-colors ${
                idx === activeContractIndex
                  ? "bg-purple-500/10 text-purple-400"
                  : "text-foreground hover:bg-white/[0.03]"
              }`}
            >
              <FileJson className="w-3 h-3 shrink-0" />
              <span className="truncate">{contract.name || "(untitled)"}</span>
              {config.contracts.length > 1 && (
                <button
                  onClick={(e: React.MouseEvent) => {
                    e.stopPropagation();
                    dispatch({ type: "REMOVE_CONTRACT", index: idx });
                  }}
                  className="ml-auto p-0.5 rounded text-red-400/40 hover:text-red-400 hover:bg-red-500/10 transition-colors"
                >
                  <Trash2 className="w-3 h-3" />
                </button>
              )}
            </div>
          ))}
        </div>

        {/* Editor area */}
        <div className="flex-1 min-w-0 overflow-y-auto">
          {activeContract ? (
            <div className="p-4 space-y-4 max-w-2xl">
              {/* Name & description */}
              <div className="space-y-2">
                <div>
                  <label className="text-[10px] text-muted-foreground block mb-1">
                    Contract Name
                  </label>
                  <input
                    type="text"
                    value={activeContract.name}
                    onChange={(e) =>
                      updateActiveContract({ ...activeContract, name: e.target.value })
                    }
                    placeholder="e.g. Dashboard Health Check"
                    className="w-full px-3 py-1.5 text-sm rounded bg-white/5 border border-white/10 text-foreground
                      placeholder:text-muted-foreground/50 focus:outline-hidden focus:border-purple-500/50"
                  />
                </div>
                <div>
                  <label className="text-[10px] text-muted-foreground block mb-1">
                    Description
                  </label>
                  <textarea
                    value={activeContract.description}
                    onChange={(e) =>
                      updateActiveContract({ ...activeContract, description: e.target.value })
                    }
                    placeholder="What does this contract verify?"
                    rows={2}
                    className="w-full px-3 py-1.5 text-xs rounded bg-white/5 border border-white/10 text-foreground
                      placeholder:text-muted-foreground/50 focus:outline-hidden focus:border-purple-500/50 resize-y"
                  />
                </div>
              </div>

              {/* Preconditions */}
              <div className="border border-white/5 rounded-lg p-3">
                <SectionHeader
                  label="Preconditions"
                  count={activeContract.preconditions.length}
                  expanded={expandedSections.has("preconditions")}
                  onToggle={() => dispatch({ type: "TOGGLE_SECTION", section: "preconditions" })}
                  onAdd={() =>
                    updateActiveContract({
                      ...activeContract,
                      preconditions: [...activeContract.preconditions, createEmptyCondition()],
                    })
                  }
                />
                {expandedSections.has("preconditions") && (
                  <div className="space-y-2 mt-2">
                    {activeContract.preconditions.length === 0 && (
                      <p className="text-[10px] text-muted-foreground/40 italic px-1">
                        No preconditions. Add conditions that must hold before verification runs.
                      </p>
                    )}
                    {activeContract.preconditions.map((cond: ContractCondition, idx: number) => (
                      <ConditionEditor
                        key={cond.id}
                        condition={cond}
                        onChange={(c) => updatePrecondition(idx, c)}
                        onRemove={() => removePrecondition(idx)}
                      />
                    ))}
                  </div>
                )}
              </div>

              {/* Verification */}
              <div className="border border-purple-500/20 rounded-lg p-3">
                <SectionHeader
                  label="Verification"
                  count={
                    activeContract.verification.type === "inline"
                      ? activeContract.verification.assertions.length
                      : 1
                  }
                  expanded={expandedSections.has("verification")}
                  onToggle={() => dispatch({ type: "TOGGLE_SECTION", section: "verification" })}
                />
                {expandedSections.has("verification") && (
                  <div className="mt-2">
                    <VerificationEditor
                      verification={activeContract.verification}
                      onChange={(v) => updateActiveContract({ ...activeContract, verification: v })}
                      specOptions={specOptions}
                    />
                  </div>
                )}
              </div>

              {/* Postconditions */}
              <div className="border border-white/5 rounded-lg p-3">
                <SectionHeader
                  label="Postconditions"
                  count={activeContract.postconditions.length}
                  expanded={expandedSections.has("postconditions")}
                  onToggle={() => dispatch({ type: "TOGGLE_SECTION", section: "postconditions" })}
                  onAdd={() =>
                    updateActiveContract({
                      ...activeContract,
                      postconditions: [...activeContract.postconditions, createEmptyCondition()],
                    })
                  }
                />
                {expandedSections.has("postconditions") && (
                  <div className="space-y-2 mt-2">
                    {activeContract.postconditions.length === 0 && (
                      <p className="text-[10px] text-muted-foreground/40 italic px-1">
                        No postconditions. Add conditions that must hold after verification passes.
                      </p>
                    )}
                    {activeContract.postconditions.map((cond: ContractCondition, idx: number) => (
                      <ConditionEditor
                        key={cond.id}
                        condition={cond}
                        onChange={(c) => updatePostcondition(idx, c)}
                        onRemove={() => removePostcondition(idx)}
                      />
                    ))}
                  </div>
                )}
              </div>
            </div>
          ) : (
            <div className="h-full flex items-center justify-center text-xs text-muted-foreground">
              No contract selected
            </div>
          )}
        </div>

        {/* JSON preview panel */}
        {jsonPreviewOpen && (
          <div className="w-80 shrink-0 border-l border-border flex flex-col">
            <div className="flex items-center justify-between px-3 py-2 border-b border-border">
              <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                JSON Preview
              </span>
              <button
                onClick={handleCopyJson}
                className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-white/10 transition-colors"
                title="Copy JSON"
              >
                <Copy className="w-3.5 h-3.5" />
              </button>
            </div>
            <pre className="flex-1 overflow-auto p-3 text-[10px] font-mono text-muted-foreground leading-relaxed">
              {JSON.stringify(config, null, 2)}
            </pre>
          </div>
        )}
      </div>
    </div>
  );
}
