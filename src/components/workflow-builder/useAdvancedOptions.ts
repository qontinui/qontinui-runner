/**
 * useAdvancedOptions.ts
 *
 * Hook for managing advanced generation options state and building
 * the base request object for the AiGeneratePanel.
 */

import { useState, useEffect, useMemo, useCallback } from "react";

// Provider and model constants
export const PROVIDERS = [
  { value: "claude_cli", label: "Claude CLI" },
  { value: "claude_api", label: "Claude API" },
  { value: "gemini_cli", label: "Gemini CLI" },
  { value: "gemini_api", label: "Gemini API" },
] as const;

export const CLAUDE_MODELS = [
  { value: "claude-sonnet-4", label: "Claude Sonnet 4" },
  { value: "claude-opus-4", label: "Claude Opus 4" },
  { value: "claude-3-5-sonnet", label: "Claude 3.5 Sonnet" },
  { value: "claude-3-opus", label: "Claude 3 Opus" },
];

export const GEMINI_MODELS = [
  { value: "gemini-3-flash", label: "Gemini 3 Flash" },
  { value: "gemini-3-pro", label: "Gemini 3 Pro" },
  { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
  { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
  { value: "gemini-2.0-flash", label: "Gemini 2.0 Flash" },
];

// =============================================================================
// Hook
// =============================================================================

export function useAdvancedOptions(aiSettings: Record<string, unknown> | null) {
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [category, setCategory] = useState("");
  const [tagsInput, setTagsInput] = useState("");
  const [maxIterations, setMaxIterations] = useState("");
  const [provider, setProvider] = useState("");
  const [model, setModel] = useState("");
  const [maxFixIterations, setMaxFixIterations] = useState("");
  const [autoIncludeContexts, setAutoIncludeContexts] = useState(true);
  const [investigateCodebase, setInvestigateCodebase] = useState(true);
  const [includeDesignGuidance, setIncludeDesignGuidance] = useState(false);
  const [verificationDepth, setVerificationDepth] = useState<
    "smoke" | "standard" | "thorough" | "regression"
  >("standard");
  const [discoveryMode, setDiscoveryMode] = useState<"auto" | "enabled" | "disabled">("auto");
  const [generationModelOverrides, setGenerationModelOverrides] = useState<
    Record<string, { provider?: string; model?: string }> | undefined
  >(undefined);

  // Initialize provider/model from settings when loaded
  useEffect(() => {
    if (!aiSettings) return;
    const settings = aiSettings as Record<string, unknown>;
    if (!provider && typeof settings.provider === "string") {
      setProvider(settings.provider);
    }
    if (!model) {
      const p = provider || (settings.provider as string) || "";
      const sub = settings[p] as Record<string, unknown> | undefined;
      if (sub && typeof sub.model === "string") {
        setModel(sub.model);
      }
    }
  }, [aiSettings, provider, model]);

  // Models list changes based on selected provider
  const modelsForProvider = useMemo(() => {
    if (provider.startsWith("gemini")) return GEMINI_MODELS;
    return CLAUDE_MODELS;
  }, [provider]);

  // Build the base request object (everything except description)
  const buildBaseRequest = useCallback(
    (extras: { selectedContextIds: string[]; inlineContext: string }) => {
      const tags = tagsInput
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean);

      const request: Record<string, unknown> = {};
      if (category.trim()) request.category = category.trim();
      if (tags.length > 0) request.tags = tags;
      if (extras.selectedContextIds.length > 0) request.context_ids = extras.selectedContextIds;
      if (extras.inlineContext.trim()) request.inline_context = extras.inlineContext.trim();
      if (maxIterations) request.max_iterations = parseInt(maxIterations, 10);
      if (provider.trim()) request.provider = provider.trim();
      if (model.trim()) request.model = model.trim();
      if (maxFixIterations) request.max_fix_iterations = parseInt(maxFixIterations, 10);
      request.auto_include_contexts = autoIncludeContexts;
      request.investigate_codebase = investigateCodebase;
      if (includeDesignGuidance) request.include_design_guidance = true;
      if (verificationDepth !== "standard") request.verification_depth = verificationDepth;
      if (discoveryMode !== "auto") request.discovery_mode = discoveryMode;
      if (generationModelOverrides) request.model_overrides = generationModelOverrides;

      return request;
    },
    [
      tagsInput,
      category,
      maxIterations,
      provider,
      model,
      maxFixIterations,
      autoIncludeContexts,
      investigateCodebase,
      includeDesignGuidance,
      verificationDepth,
      discoveryMode,
      generationModelOverrides,
    ],
  );

  return {
    showAdvanced,
    setShowAdvanced,
    category,
    setCategory,
    tagsInput,
    setTagsInput,
    maxIterations,
    setMaxIterations,
    provider,
    setProvider,
    model,
    setModel,
    maxFixIterations,
    setMaxFixIterations,
    autoIncludeContexts,
    setAutoIncludeContexts,
    investigateCodebase,
    setInvestigateCodebase,
    includeDesignGuidance,
    setIncludeDesignGuidance,
    verificationDepth,
    setVerificationDepth,
    discoveryMode,
    setDiscoveryMode,
    generationModelOverrides,
    setGenerationModelOverrides,
    modelsForProvider,
    buildBaseRequest,
  };
}
