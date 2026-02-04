/**
 * SpecFileLoader
 *
 * File picker for loading spec files. Supports both:
 * - Legacy TestGeneratorOutput JSON (auto-migrated via migrateFromTestGeneratorOutput)
 * - New SpecConfig (.spec.uibridge.json) format
 */

import { useState, useCallback, useRef } from "react";
import { Upload, FileJson, AlertCircle, Check } from "lucide-react";
import { validateSpecConfig } from "@qontinui/ui-bridge/specs";
import { migrateFromTestGeneratorOutput } from "@qontinui/ui-bridge/specs";
import type { SpecConfig } from "@qontinui/ui-bridge/specs";
import type { TestGeneratorOutput } from "./types";

interface SpecFileLoaderProps {
  onLoad: (output: TestGeneratorOutput) => void;
  onLoadSpecConfig?: (config: SpecConfig) => void;
  currentFile?: string;
}

function isLegacyFormat(parsed: Record<string, unknown>): boolean {
  return (
    typeof parsed.generatorType === "string" &&
    Array.isArray(parsed.testSpecifications)
  );
}

function isSpecConfigFormat(parsed: Record<string, unknown>): boolean {
  return Array.isArray(parsed.groups) && typeof parsed.version === "string";
}

export function SpecFileLoader({ onLoad, onLoadSpecConfig, currentFile }: SpecFileLoaderProps) {
  const [error, setError] = useState<string | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [migratedFromLegacy, setMigratedFromLegacy] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const validateAndLoad = useCallback(
    (content: string, fileName: string) => {
      try {
        const parsed = JSON.parse(content);
        setMigratedFromLegacy(false);

        // Try new SpecConfig format first
        if (isSpecConfigFormat(parsed)) {
          const validation = validateSpecConfig(parsed);
          if (!validation.valid) {
            setError(`Invalid spec config: ${validation.errors[0]?.message || "unknown error"}`);
            return;
          }
          setError(null);
          if (onLoadSpecConfig) {
            onLoadSpecConfig(parsed as SpecConfig);
          } else {
            // Fallback: convert SpecConfig to legacy format for backward compatibility
            // (This path is for when the parent only handles legacy format)
            setError("This component requires onLoadSpecConfig to handle .spec.uibridge.json files");
          }
          return;
        }

        // Try legacy TestGeneratorOutput format
        if (isLegacyFormat(parsed)) {
          if (!parsed.version || parsed.version !== "1.0.0") {
            setError("Invalid file: missing or unsupported version");
            return;
          }
          if (!["snapshot", "navigation"].includes(parsed.generatorType as string)) {
            setError("Invalid file: missing or invalid generatorType");
            return;
          }
          if (!Array.isArray(parsed.testSpecifications)) {
            setError("Invalid file: missing testSpecifications array");
            return;
          }
          if (!Array.isArray(parsed.states)) {
            setError("Invalid file: missing states array");
            return;
          }

          setError(null);

          // If onLoadSpecConfig is provided, migrate and use it
          if (onLoadSpecConfig) {
            const specConfig = migrateFromTestGeneratorOutput(parsed as TestGeneratorOutput);
            setMigratedFromLegacy(true);
            onLoadSpecConfig(specConfig);
          }

          // Always call onLoad for backward compatibility
          onLoad(parsed as TestGeneratorOutput);
          return;
        }

        setError("Unrecognized file format. Expected a .spec.uibridge.json or legacy test spec JSON.");
      } catch {
        setError(`Failed to parse ${fileName}: invalid JSON`);
      }
    },
    [onLoad, onLoadSpecConfig],
  );

  const handleFileSelect = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const file = e.target.files?.[0];
      if (!file) return;
      const reader = new FileReader();
      reader.onload = (ev) => {
        validateAndLoad(ev.target?.result as string, file.name);
      };
      reader.readAsText(file);
    },
    [validateAndLoad],
  );

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      setIsDragging(false);
      const file = e.dataTransfer.files[0];
      if (!file) return;
      const reader = new FileReader();
      reader.onload = (ev) => {
        validateAndLoad(ev.target?.result as string, file.name);
      };
      reader.readAsText(file);
    },
    [validateAndLoad],
  );

  return (
    <div className="p-4 space-y-3">
      {/* Drop zone */}
      <div
        onDragOver={(e) => { e.preventDefault(); setIsDragging(true); }}
        onDragLeave={() => setIsDragging(false)}
        onDrop={handleDrop}
        onClick={() => fileInputRef.current?.click()}
        className={`border-2 border-dashed rounded-lg p-6 text-center cursor-pointer transition-colors ${
          isDragging
            ? "border-blue-500 bg-blue-500/10"
            : "border-neutral-600 hover:border-neutral-500 bg-neutral-800/30"
        }`}
      >
        <Upload className="w-8 h-8 text-neutral-500 mx-auto mb-2" />
        <p className="text-sm text-neutral-300">
          Drop a spec file here, or click to browse
        </p>
        <p className="text-xs text-neutral-500 mt-1">
          Supports .spec.uibridge.json and legacy test spec JSON formats
        </p>
        <input
          ref={fileInputRef}
          type="file"
          accept=".json"
          onChange={handleFileSelect}
          className="hidden"
        />
      </div>

      {/* Current file indicator */}
      {currentFile && (
        <div className="flex items-center gap-2 px-3 py-2 bg-emerald-500/10 border border-emerald-500/20 rounded-md">
          <FileJson className="w-4 h-4 text-emerald-400" />
          <span className="text-xs text-emerald-300">{currentFile}</span>
          {migratedFromLegacy && (
            <span className="text-[10px] px-1.5 py-0.5 bg-yellow-500/20 text-yellow-400 rounded">
              migrated
            </span>
          )}
          <Check className="w-3 h-3 text-emerald-400 ml-auto" />
        </div>
      )}

      {/* Error */}
      {error && (
        <div className="flex items-center gap-2 px-3 py-2 bg-red-500/10 border border-red-500/20 rounded-md">
          <AlertCircle className="w-4 h-4 text-red-400" />
          <span className="text-xs text-red-300">{error}</span>
        </div>
      )}
    </div>
  );
}
