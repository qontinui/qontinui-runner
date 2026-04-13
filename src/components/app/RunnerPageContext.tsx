import { useMemo } from "react";
import { usePageContext } from "ui-bridge";
import type { MainTabId } from "./tab-types";

export function RunnerPageContext({ activeTab }: { activeTab: MainTabId }) {
  const context = useMemo(() => {
    if (activeTab.startsWith("settings")) {
      return { name: "Settings", section: "configure", breadcrumb: ["Configure", "Settings"] };
    }

    const map: Record<string, { name: string; section: string; breadcrumb: string[] }> = {
      "prompt-home": { name: "Home", section: "run", breadcrumb: ["Home"] },
      "gui-automation": { name: "Execute", section: "run", breadcrumb: ["Run", "Execute"] },
      active: { name: "Active Dashboard", section: "run", breadcrumb: ["Run", "Active Dashboard"] },
      history: { name: "History", section: "run", breadcrumb: ["Run", "History"] },
      runs: { name: "History", section: "run", breadcrumb: ["Run", "History"] },
      "workflow-queue": {
        name: "Workflow Queue",
        section: "run",
        breadcrumb: ["Run", "Workflow Queue"],
      },
      "run-recap": { name: "Run Recap", section: "observe", breadcrumb: ["Observe", "Run Recap"] },
      "run-actions": {
        name: "Run Actions",
        section: "observe",
        breadcrumb: ["Observe", "Run Actions"],
      },
      "run-image": {
        name: "Image Recognition",
        section: "observe",
        breadcrumb: ["Observe", "Image Recognition"],
      },
      "run-findings": { name: "Findings", section: "observe", breadcrumb: ["Observe", "Findings"] },
      "run-state-explorer": {
        name: "State Explorer",
        section: "observe",
        breadcrumb: ["Observe", "State Explorer"],
      },
      "run-tests": {
        name: "Test Results",
        section: "observe",
        breadcrumb: ["Observe", "Test Results"],
      },
      "run-ai-output": {
        name: "AI Output",
        section: "observe",
        breadcrumb: ["Observe", "AI Output"],
      },
      "run-ai-data": {
        name: "AI Data Viewer",
        section: "observe",
        breadcrumb: ["Observe", "AI Data Viewer"],
      },
      "run-statistics": {
        name: "Statistics",
        section: "observe",
        breadcrumb: ["Observe", "Statistics"],
      },
      "run-traces": { name: "Traces", section: "observe", breadcrumb: ["Observe", "Traces"] },
      "unified-workflow-builder": {
        name: "Workflow Builder",
        section: "build",
        breadcrumb: ["Build", "Workflow Builder"],
      },
      "dag-workflow-editor": {
        name: "DAG Workflow Editor",
        section: "build",
        breadcrumb: ["Build", "DAG Workflow Editor"],
      },
      library: { name: "Library", section: "build", breadcrumb: ["Build", "Library"] },
      "step-builders": { name: "Library", section: "build", breadcrumb: ["Build", "Library"] },
      capture: { name: "Capture", section: "build", breadcrumb: ["Build", "Capture"] },
      triggers: { name: "Triggers", section: "configure", breadcrumb: ["Configure", "Triggers"] },
      tasks: { name: "Scheduler", section: "configure", breadcrumb: ["Configure", "Scheduler"] },
      "config-log-sources": {
        name: "Log Sources",
        section: "configure",
        breadcrumb: ["Configure", "Log Sources"],
      },
      "config-findings": {
        name: "Findings Config",
        section: "configure",
        breadcrumb: ["Configure", "Findings"],
      },
      "config-hooks": { name: "Hooks", section: "configure", breadcrumb: ["Configure", "Hooks"] },
      "config-ui-bridge": {
        name: "UI Bridge",
        section: "tools",
        breadcrumb: ["Tools", "UI Bridge"],
      },
      terminal: { name: "Terminal", section: "tools", breadcrumb: ["Tools", "Terminal"] },
      specs: { name: "Specs", section: "tools", breadcrumb: ["Tools", "Specs"] },
      "state-machine": {
        name: "UI Bridge State Machine",
        section: "tools",
        breadcrumb: ["Tools", "UI Bridge State Machine"],
      },
      "generator-eval": {
        name: "Generator Eval",
        section: "tools",
        breadcrumb: ["Tools", "Generator Eval"],
      },
      "meta-optimizer": {
        name: "Meta-Optimizer",
        section: "observe",
        breadcrumb: ["Observe", "Meta-Optimizer"],
      },
      "orchestration-loop": {
        name: "Orchestration Loop",
        section: "tools",
        breadcrumb: ["Tools", "Orchestration Loop"],
      },
      "image-quality-tests": {
        name: "Image Quality Tests",
        section: "tools",
        breadcrumb: ["Tools", "Image Quality Tests"],
      },
      "error-monitor": {
        name: "Error Monitor",
        section: "system",
        breadcrumb: ["System", "Error Monitor"],
      },
      processes: {
        name: "Process Manager",
        section: "system",
        breadcrumb: ["System", "Process Manager"],
      },
      reflection: { name: "Reflection", section: "system", breadcrumb: ["System", "Reflection"] },
      architecture: {
        name: "Architecture",
        section: "system",
        breadcrumb: ["System", "Architecture"],
      },
      logs: { name: "Logs", section: "system", breadcrumb: ["System", "Logs"] },
      help: { name: "Help", section: "system", breadcrumb: ["Help"] },
      ai: { name: "AI Output", section: "observe", breadcrumb: ["Observe", "AI Output"] },
    };

    return map[activeTab] ?? { name: activeTab, section: "other", breadcrumb: [activeTab] };
  }, [activeTab]);

  usePageContext(context);
  return null;
}
