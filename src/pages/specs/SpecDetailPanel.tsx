import { useState } from "react";
import { Shield, AlertTriangle, Plus, Bug } from "lucide-react";
import type { LoadedSpec } from "./types";
import type { SpecGroup, SpecAssertion, SetupAction } from "ui-bridge";
import { SpecIssuesPanel } from "./SpecIssuesPanel";
import { GlobalIssuesPanel } from "./GlobalIssuesPanel";
import { SpecTriageView } from "@/components/specs/SpecTriageView";
import { GroupDetail, PageSpecOverview } from "./PageSpecComponents";
import { ArchitectureOverview } from "./ArchitectureOverview";
import { ApiOverview } from "./ApiOverview";
import { DataOverview } from "./DataOverview";
import { DependencyOverview } from "./DependencyOverview";
import { ConstraintOverview } from "./ConstraintOverview";

function UnspeccedPageView({
  label,
  description,
  expectedSpecId,
  onGenerateSpec,
}: {
  label: string;
  description: string;
  expectedSpecId: string;
  onGenerateSpec: () => void;
}) {
  return (
    <div className="h-full flex items-center justify-center p-8">
      <div className="text-center max-w-md space-y-4">
        <div className="w-12 h-12 rounded-full bg-amber-500/10 border border-amber-500/20 flex items-center justify-center mx-auto">
          <Shield className="w-6 h-6 text-amber-500/60" />
        </div>
        <div>
          <h2 className="text-lg font-semibold text-foreground">{label}</h2>
          <p className="text-xs text-muted-foreground mt-1">{description}</p>
        </div>
        <div className="text-xs text-muted-foreground/60 bg-white/[0.02] rounded-lg border border-white/5 px-4 py-3">
          <p>No spec exists yet for this page.</p>
          <p className="mt-1 text-[10px] opacity-70">
            Expected spec ID: <code className="text-amber-400/60">{expectedSpecId}</code>
          </p>
        </div>
        <button
          onClick={onGenerateSpec}
          className="inline-flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg
            bg-purple-500/15 text-purple-400 border border-purple-500/25
            hover:bg-purple-500/25 transition-colors"
        >
          <Plus className="w-4 h-4" />
          Generate Spec
        </button>
      </div>
    </div>
  );
}

interface SpecDetailPanelProps {
  selectedSpec: LoadedSpec | null;
  selectedGroup: SpecGroup | null;
  selectionType: "none" | "spec" | "group" | "assertion" | "unspecced-page";
  editMode?: boolean;
  unspeccedPageInfo?: { label: string; description: string; expectedSpecId: string } | null;
  onToggleAssertion?: (specId: string, groupId: string, assertionId: string) => void;
  onRemoveAssertion?: (specId: string, groupId: string, assertionId: string) => void;
  onAddAssertion?: (specId: string, groupId: string, assertion: SpecAssertion) => void;
  onAddGroup?: (specId: string, group: SpecGroup) => void;
  onRemoveGroup?: (specId: string, groupId: string) => void;
  onUpdateSetupActions?: (specId: string, groupId: string, actions: SetupAction[]) => void;
  onClearSelection?: () => void;
  onGenerateSpec?: () => void;
  onUpdateSpec?: (groupId: string) => void;
  onFixCode?: (groupId: string) => void;
}

export function SpecDetailPanel({
  selectedSpec,
  selectedGroup,
  selectionType,
  editMode,
  unspeccedPageInfo,
  onToggleAssertion,
  onRemoveAssertion,
  onAddAssertion,
  onAddGroup,
  onRemoveGroup,
  onUpdateSetupActions,
  onClearSelection,
  onGenerateSpec,
  onUpdateSpec,
  onFixCode,
}: SpecDetailPanelProps) {
  const [activeTab, setActiveTab] = useState<"assertions" | "issues" | "triage">("assertions");

  if (selectionType === "unspecced-page" && unspeccedPageInfo) {
    return (
      <UnspeccedPageView
        label={unspeccedPageInfo.label}
        description={unspeccedPageInfo.description}
        expectedSpecId={unspeccedPageInfo.expectedSpecId}
        onGenerateSpec={onGenerateSpec || (() => {})}
      />
    );
  }

  if (!selectedSpec) {
    return <GlobalIssuesPanel />;
  }

  if (selectionType === "group" && selectedGroup && selectedSpec.kind === "page-spec") {
    return (
      <div className="h-full overflow-y-auto p-4">
        <GroupDetail
          group={selectedGroup}
          specId={selectedSpec.specId}
          editMode={editMode}
          onToggleAssertion={
            onToggleAssertion
              ? (aid) => onToggleAssertion(selectedSpec.specId, selectedGroup.id, aid)
              : undefined
          }
          onRemoveAssertion={
            onRemoveAssertion
              ? (aid) => onRemoveAssertion(selectedSpec.specId, selectedGroup.id, aid)
              : undefined
          }
          onAddAssertion={
            onAddAssertion
              ? (a) => onAddAssertion(selectedSpec.specId, selectedGroup.id, a)
              : undefined
          }
          onRemoveGroup={
            onRemoveGroup ? () => onRemoveGroup(selectedSpec.specId, selectedGroup.id) : undefined
          }
          onUpdateSetupActions={
            onUpdateSetupActions
              ? (actions) => onUpdateSetupActions(selectedSpec.specId, selectedGroup.id, actions)
              : undefined
          }
        />
      </div>
    );
  }

  if (selectedSpec.kind === "page-spec") {
    const pageUrl = (selectedSpec.config as { metadata?: { pageUrl?: string } })?.metadata?.pageUrl;

    return (
      <div className="h-full flex flex-col">
        <div className="flex border-b border-border px-4 pt-2 gap-1 shrink-0">
          <button
            onClick={() => setActiveTab("assertions")}
            className={`px-3 py-1.5 text-xs font-medium rounded-t border border-b-0 transition-colors ${
              activeTab === "assertions"
                ? "bg-background text-foreground border-border"
                : "text-muted-foreground border-transparent hover:text-foreground"
            }`}
          >
            <Shield className="w-3 h-3 inline-block mr-1 -mt-0.5" />
            Assertions
          </button>
          <button
            onClick={() => setActiveTab("issues")}
            className={`px-3 py-1.5 text-xs font-medium rounded-t border border-b-0 transition-colors ${
              activeTab === "issues"
                ? "bg-background text-foreground border-border"
                : "text-muted-foreground border-transparent hover:text-foreground"
            }`}
          >
            <Bug className="w-3 h-3 inline-block mr-1 -mt-0.5" />
            Issues
          </button>
          <button
            onClick={() => setActiveTab("triage")}
            className={`px-3 py-1.5 text-xs font-medium rounded-t border border-b-0 transition-colors ${
              activeTab === "triage"
                ? "bg-background text-foreground border-border"
                : "text-muted-foreground border-transparent hover:text-foreground"
            }`}
          >
            <AlertTriangle className="w-3 h-3 inline-block mr-1 -mt-0.5" />
            Triage
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-4">
          {activeTab === "assertions" && (
            <PageSpecOverview
              config={selectedSpec.config}
              specId={selectedSpec.specId}
              source={selectedSpec.source}
              appName={selectedSpec.appName}
              editMode={editMode}
              onAddGroup={onAddGroup ? (g) => onAddGroup(selectedSpec.specId, g) : undefined}
              onRemoveGroup={
                onRemoveGroup ? (gid) => onRemoveGroup(selectedSpec.specId, gid) : undefined
              }
            />
          )}
          {activeTab === "issues" && (
            <SpecIssuesPanel
              specId={selectedSpec.specId}
              pageUrl={pageUrl}
              onViewAllIssues={onClearSelection}
            />
          )}
          {activeTab === "triage" && (
            <SpecTriageView
              specId={selectedSpec.specId}
              specConfig={
                selectedSpec.config as unknown as {
                  description?: string;
                  groups: Array<{
                    id: string;
                    name: string;
                    category?: string;
                    assertions: Array<{
                      id: string;
                      description: string;
                      severity: string;
                      assertionType: string;
                      enabled?: boolean;
                    }>;
                  }>;
                }
              }
              onUpdateSpec={onUpdateSpec}
              onFixCode={onFixCode}
            />
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto p-4">
      {selectedSpec.kind === "architecture" && (
        <ArchitectureOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
      {selectedSpec.kind === "api" && (
        <ApiOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
      {selectedSpec.kind === "data" && (
        <DataOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
      {selectedSpec.kind === "dependency" && (
        <DependencyOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
      {selectedSpec.kind === "constraint" && (
        <ConstraintOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
    </div>
  );
}
