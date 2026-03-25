/**
 * GraphQL document definitions for queries, mutations, and subscriptions.
 *
 * These are the typed GraphQL operations used by the Apollo hooks.
 * They map 1:1 to the Rust resolvers in src-tauri/src/graphql/.
 */

import { gql } from "@apollo/client/core";

// ==========================================================================
// Fragments
// ==========================================================================

export const TASK_RUN_FIELDS = gql`
  fragment TaskRunFields on GqlTaskRun {
    id
    taskName
    prompt
    taskType
    status
    sessionsCount
    maxSessions
    autoContinue
    errorMessage
    configId
    workflowName
    workflowId
    summary
    goalAchieved
    remainingWork
    workflowType
    parentTaskRunId
    rootTaskRunId
    depth
    isReflection
    isFollowUp
    isFixer
    isMetaOptimizer
    createdAt
    updatedAt
    completedAt
  }
`;

export const FINDING_FIELDS = gql`
  fragment FindingFields on GqlFinding {
    id
    taskRunId
    sessionNum
    category
    severity
    status
    actionType
    title
    description
    resolution
    codeContext {
      file
      line
      column
      snippet
    }
    signatureHash
    userResponse
    detectedAt
    resolvedAt
    resolvedInSession
    updatedAt
  }
`;

// ==========================================================================
// Queries
// ==========================================================================

export const TASK_RUNS_QUERY = gql`
  ${TASK_RUN_FIELDS}
  query TaskRuns($limit: Int = 50, $workflowType: String) {
    taskRuns(limit: $limit, workflowType: $workflowType) {
      ...TaskRunFields
    }
  }
`;

export const TASK_RUN_QUERY = gql`
  ${TASK_RUN_FIELDS}
  query TaskRun($id: String!) {
    taskRun(id: $id) {
      ...TaskRunFields
    }
  }
`;

export const RUNNING_TASK_RUNS_QUERY = gql`
  ${TASK_RUN_FIELDS}
  query RunningTaskRuns {
    runningTaskRuns {
      ...TaskRunFields
    }
  }
`;

export const WORKFLOWS_QUERY = gql`
  query Workflows {
    workflows {
      id
      name
      description
      category
      tags
      maxIterations
      timeoutSeconds
      provider
      model
      isFavorite
      multiAgentMode
      workflowArchitecture
      stageCount
      createdAt
      updatedAt
    }
  }
`;

export const WORKFLOW_QUERY = gql`
  query Workflow($id: String!) {
    workflow(id: $id) {
      id
      name
      description
      category
      tags
      setupSteps
      verificationSteps
      agenticSteps
      completionSteps
      maxIterations
      timeoutSeconds
      provider
      model
      isFavorite
      multiAgentMode
      reflectionMode
      approvalGate
      stopOnFailure
      useWorktree
      strictCwd
      enforceTokenBudget
      workflowArchitecture
      stages
      createdAt
      updatedAt
    }
  }
`;

export const FINDINGS_QUERY = gql`
  ${FINDING_FIELDS}
  query Findings($taskRunId: String!) {
    findings(taskRunId: $taskRunId) {
      ...FindingFields
    }
  }
`;

export const FINDING_SUMMARY_QUERY = gql`
  query FindingSummary($taskRunId: String!) {
    findingSummary(taskRunId: $taskRunId) {
      taskRunId
      total
      byCategory
      bySeverity
      byStatus
      needsInputCount
      resolvedCount
      outstandingCount
    }
  }
`;

export const TASK_RUN_OUTPUT_QUERY = gql`
  query TaskRunOutput($id: String!, $offset: Int = 0, $limit: Int = 10000) {
    taskRunOutput(id: $id, offset: $offset, limit: $limit) {
      taskRunId
      content
      totalLength
      offset
      hasMore
    }
  }
`;

export const RUNNER_STATUS_QUERY = gql`
  query RunnerStatus {
    runnerStatus {
      instanceName
      apiPort
      executorRunning
      executorState
      configLoaded
      configPath
      aiAnalysisRunning
    }
  }
`;

export const ORCHESTRATION_LOOP_STATUS_QUERY = gql`
  query OrchestrationLoopStatus {
    orchestrationLoopStatus {
      running
      phase
      currentIteration
      startedAt
      error
    }
  }
`;

export const UI_BRIDGE_HEALTH_QUERY = gql`
  query UiBridgeHealth {
    uiBridgeHealth {
      responsive
      lastHeartbeat
      heartbeatAgeMs
      uptimeSeconds
      pendingRequests
      circuitBreaker
      semaphoreAvailable
    }
  }
`;

// ==========================================================================
// Mutations
// ==========================================================================

export const STOP_TASK_RUN_MUTATION = gql`
  mutation StopTaskRun($id: String!) {
    stopTaskRun(id: $id)
  }
`;

export const PAUSE_TASK_RUN_MUTATION = gql`
  mutation PauseTaskRun($id: String!) {
    pauseTaskRun(id: $id)
  }
`;

export const UNPAUSE_TASK_RUN_MUTATION = gql`
  mutation UnpauseTaskRun($id: String!) {
    unpauseTaskRun(id: $id)
  }
`;

export const DELETE_TASK_RUN_MUTATION = gql`
  mutation DeleteTaskRun($id: String!) {
    deleteTaskRun(id: $id)
  }
`;

export const CREATE_TASK_RUN_MUTATION = gql`
  ${TASK_RUN_FIELDS}
  mutation CreateTaskRun($input: CreateTaskRunInput!) {
    createTaskRun(input: $input) {
      ...TaskRunFields
    }
  }
`;

export const RUN_WORKFLOW_MUTATION = gql`
  ${TASK_RUN_FIELDS}
  mutation RunWorkflow($workflowId: String!, $prompt: String) {
    runWorkflow(workflowId: $workflowId, prompt: $prompt) {
      ...TaskRunFields
    }
  }
`;

export const UPDATE_FINDING_STATUS_MUTATION = gql`
  mutation UpdateFindingStatus($input: UpdateFindingStatusInput!) {
    updateFindingStatus(input: $input)
  }
`;

export const RESPOND_TO_FINDING_MUTATION = gql`
  mutation RespondToFinding($findingId: String!, $response: String!) {
    respondToFinding(findingId: $findingId, response: $response)
  }
`;

export const DELETE_WORKFLOW_MUTATION = gql`
  mutation DeleteWorkflow($id: String!) {
    deleteWorkflow(id: $id)
  }
`;

// ==========================================================================
// Subscriptions
// ==========================================================================

export const HEALTH_STREAM_SUBSCRIPTION = gql`
  subscription UiBridgeHealthStream($intervalMs: Int = 3000) {
    uiBridgeHealthStream(intervalMs: $intervalMs) {
      responsive
      lastHeartbeat
      heartbeatAgeMs
      uptimeSeconds
      pendingRequests
      circuitBreaker
      semaphoreAvailable
    }
  }
`;

export const RUNNER_EVENTS_SUBSCRIPTION = gql`
  subscription RunnerEvents($filter: [String!]) {
    runnerEvents(filter: $filter) {
      ... on OrchestratorStateChangeEvent {
        taskRunId
        workflowStage
        iteration
        phase
        stateData
      }
      ... on StepProgressEvent {
        taskRunId
        stepIndex
        stepName
        status
        details
        timestamp
      }
      ... on TaskRunUpdateEvent {
        taskRunId
        status
        iteration
        details
        timestamp
      }
      ... on FindingDetectedEvent {
        finding
      }
      ... on FindingResolvedEvent {
        finding
      }
      ... on AiOutputChunkEvent {
        taskRunId
        chunk
        accumulatedLength
      }
      ... on GenericRunnerEvent {
        eventType
        data
      }
    }
  }
`;

export const TASK_RUN_PROGRESS_SUBSCRIPTION = gql`
  subscription TaskRunProgress($taskRunId: String!) {
    taskRunProgress(taskRunId: $taskRunId) {
      ... on OrchestratorStateChangeEvent {
        taskRunId
        workflowStage
        iteration
        phase
        stateData
      }
      ... on StepProgressEvent {
        taskRunId
        stepIndex
        stepName
        status
        details
        timestamp
      }
      ... on TaskRunUpdateEvent {
        taskRunId
        status
        iteration
        details
        timestamp
      }
      ... on AiOutputChunkEvent {
        taskRunId
        chunk
        accumulatedLength
      }
      ... on GenericRunnerEvent {
        eventType
        data
      }
    }
  }
`;

export const FINDING_UPDATES_SUBSCRIPTION = gql`
  ${FINDING_FIELDS}
  subscription FindingUpdates($taskRunId: String!, $intervalMs: Int = 2000) {
    findingUpdates(taskRunId: $taskRunId, intervalMs: $intervalMs) {
      ...FindingFields
    }
  }
`;

export const ORCHESTRATION_LOOP_STATUS_SUBSCRIPTION = gql`
  subscription OrchestrationLoopStatusStream($intervalMs: Int = 2000) {
    orchestrationLoopStatusStream(intervalMs: $intervalMs) {
      running
      phase
      currentIteration
      startedAt
      error
    }
  }
`;
