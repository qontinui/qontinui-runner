/**
 * SchedulerTab (Tasks)
 *
 * Main component for the task scheduler.
 * Provides scheduled task management, execution history, and settings configuration.
 *
 * Note: This component is designed to work standalone and fill its parent container.
 * It handles its own scrolling internally.
 */

import { useState } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { Calendar, History, Settings, Plus, RefreshCw, Wrench } from "lucide-react";
import { getAccentColors, getStatusColors } from "@/design-system";
import { useScheduler } from "../../hooks/useScheduler";
import { SchedulerTaskList } from "./SchedulerTaskList";
import { SchedulerTaskForm } from "./SchedulerTaskForm";
import { SchedulerHistory } from "./SchedulerHistory";
import { SchedulerSettings } from "./SchedulerSettings";
import { SchedulerStats } from "./SchedulerStats";
import type { ScheduledTask, CreateScheduledTaskRequest } from "../../types/scheduler";

type SchedulerSubTab = "tasks" | "history" | "settings";

interface SchedulerTabProps {
  className?: string;
}

export function SchedulerTab({ className = "" }: SchedulerTabProps) {
  const [activeSubTab, setActiveSubTab] = useState<SchedulerSubTab>("tasks");
  const [isCreating, setIsCreating] = useState(false);
  const [editingTask, setEditingTask] = useState<ScheduledTask | null>(null);

  const {
    tasks,
    settings,
    status,
    selectedTask,
    taskHistory,
    loading,
    error,
    createTask,
    updateTask,
    deleteTask,
    runTaskNow,
    selectTask,
    updateSettings,
    triggerAutoFix,
    refresh,
  } = useScheduler();

  const handleCreateTask = async (request: CreateScheduledTaskRequest) => {
    const task = await createTask(request);
    if (task) {
      setIsCreating(false);
    }
  };

  const handleUpdateTask = async (request: CreateScheduledTaskRequest) => {
    if (!editingTask) return;
    const task = await updateTask(editingTask.id, request);
    if (task) {
      setEditingTask(null);
    }
  };

  const handleDeleteTask = async (task: ScheduledTask) => {
    if (window.confirm(`Delete scheduled task "${task.name}"?`)) {
      await deleteTask(task.id);
    }
  };

  const handleToggleEnabled = async (task: ScheduledTask) => {
    await updateTask(task.id, { enabled: !task.enabled });
  };

  return (
    <div className={`flex flex-col h-full ${className}`}>
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border">
        <div className="flex items-center gap-3">
          <Calendar className="w-5 h-5 text-primary" />
          <h2 className="text-lg font-semibold">Scheduled Tasks</h2>
          {status && (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <span
                className={`w-2 h-2 rounded-full ${status.enabled ? getStatusColors("success").icon.replace("text-", "bg-") : "bg-text-muted"}`}
              />
              <span>{status.enabled ? "Running" : "Paused"}</span>
              {status.running_tasks > 0 && (
                <span className={getStatusColors("running").text}>
                  ({status.running_tasks} running)
                </span>
              )}
            </div>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => triggerAutoFix()}
            disabled={loading}
            className={`flex items-center gap-2 px-3 py-1.5 text-sm ${getAccentColors("orange").bg} ${getAccentColors("orange").text} rounded-lg hover:bg-orange-500/20 transition-colors`}
            title="Trigger Auto-Fix Now"
          >
            <Wrench className="w-4 h-4" />
            Auto-Fix
          </button>
          <button
            onClick={refresh}
            disabled={loading}
            className="flex items-center gap-2 px-3 py-1.5 text-sm bg-muted rounded-lg hover:bg-muted/80 transition-colors"
            title="Refresh"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
          </button>
          {activeSubTab === "tasks" && !isCreating && !editingTask && (
            <button
              onClick={() => setIsCreating(true)}
              className="flex items-center gap-2 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors"
            >
              <Plus className="w-4 h-4" />
              New Task
            </button>
          )}
        </div>
      </div>

      {/* Error display */}
      {error && (
        <div className="mx-4 mt-4 p-3 bg-destructive/10 text-destructive rounded-lg text-sm">
          {error}
        </div>
      )}

      {/* Statistics dashboard - hidden when creating/editing a task */}
      {!isCreating && !editingTask && (
        <SchedulerStats tasks={tasks} taskHistory={taskHistory} status={status} />
      )}

      {/* Sub-tabs */}
      <Tabs.Root
        value={activeSubTab}
        onValueChange={(v) => setActiveSubTab(v as SchedulerSubTab)}
        className="flex-1 flex flex-col min-h-0"
      >
        <Tabs.List className="flex items-center gap-1 px-4 pt-2 border-b border-border">
          <Tabs.Trigger
            value="tasks"
            className="flex items-center gap-2 px-3 py-2 text-sm text-muted-foreground data-[state=active]:text-foreground data-[state=active]:border-b-2 data-[state=active]:border-primary transition-colors"
          >
            <Calendar className="w-4 h-4" />
            Scheduled ({tasks.length})
          </Tabs.Trigger>
          <Tabs.Trigger
            value="history"
            className="flex items-center gap-2 px-3 py-2 text-sm text-muted-foreground data-[state=active]:text-foreground data-[state=active]:border-b-2 data-[state=active]:border-primary transition-colors"
          >
            <History className="w-4 h-4" />
            History
          </Tabs.Trigger>
          <Tabs.Trigger
            value="settings"
            className="flex items-center gap-2 px-3 py-2 text-sm text-muted-foreground data-[state=active]:text-foreground data-[state=active]:border-b-2 data-[state=active]:border-primary transition-colors"
          >
            <Settings className="w-4 h-4" />
            Settings
          </Tabs.Trigger>
        </Tabs.List>

        <div className="flex-1 overflow-auto">
          <Tabs.Content value="tasks" className="h-full p-4">
            {isCreating ? (
              <SchedulerTaskForm
                onSubmit={handleCreateTask}
                onCancel={() => setIsCreating(false)}
                loading={loading}
              />
            ) : editingTask ? (
              <SchedulerTaskForm
                task={editingTask}
                onSubmit={handleUpdateTask}
                onCancel={() => setEditingTask(null)}
                loading={loading}
              />
            ) : (
              <SchedulerTaskList
                tasks={tasks}
                selectedTask={selectedTask}
                onSelectTask={selectTask}
                onEditTask={setEditingTask}
                onDeleteTask={handleDeleteTask}
                onRunNow={runTaskNow}
                onToggleEnabled={handleToggleEnabled}
                loading={loading}
              />
            )}
          </Tabs.Content>

          <Tabs.Content value="history" className="h-full p-4">
            <SchedulerHistory
              tasks={tasks}
              selectedTask={selectedTask}
              taskHistory={taskHistory}
              onSelectTask={selectTask}
            />
          </Tabs.Content>

          <Tabs.Content value="settings" className="h-full p-4">
            {settings && (
              <SchedulerSettings
                settings={settings}
                status={status}
                onUpdateSettings={updateSettings}
                loading={loading}
              />
            )}
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}
