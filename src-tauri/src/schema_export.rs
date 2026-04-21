//! Schema Export Module
//!
//! Pure re-export aggregator — contains **zero** local type definitions.
//! All types come from `qontinui-types` (`qontinui-schemas/rust/`).
//!
//! Used by the `export_schemas` binary to produce a single JSON object
//! mapping type names to their JSON Schemas for cross-language type
//! generation (TypeScript, Python).

use schemars::schema_for;
use serde_json::{Map, Value};

// ============================================================================
// Export function
// ============================================================================

/// Export all registered JSON Schemas as a single JSON object.
///
/// Keys are type names, values are full JSON Schema objects.
/// Used by the `export_schemas` binary and the type generation pipeline.
///
/// All types are re-exported from the `qontinui-types` crate
/// (`qontinui-schemas/rust/`). Those types are the source of truth for the
/// TypeScript and Python bindings; we emit schemas via `schema_for!` on the
/// canonical types directly so there is no duplication.
pub fn export_all_schemas() -> Value {
    use qontinui_types::{
        accessibility as qa, ai_workflows as qaw, app_events as qae, config as qcfg,
        constraints as qc, discovery as qdc, execution as qe, findings as qfn, geometry as qg,
        mcp_config as qmc, orchestration_config as qoc, process_management as qpm, rag as qr,
        scheduler as qs, state_machine as qsm, targets as qt, task_run as qtr, terminal as qtm,
        ticket_system as qts, tree_events as qte, ui_bridge as qub, verification as qv,
        worker_output as qwo, workflow as qw, workflow_step as qws,
    };

    // Built via a plain Map instead of `json!` to avoid the
    // `recursion_limit` ceiling that the `json!` macro hits on large
    // object literals.
    let mut m: Map<String, Value> = Map::new();

    macro_rules! add {
        ($name:literal, $ty:ty) => {
            m.insert(
                $name.to_string(),
                serde_json::to_value(schema_for!($ty)).unwrap(),
            );
        };
    }

    // ── qontinui-types: worker_output ──
    add!("WorkerOutput", qwo::WorkerOutput);
    add!("StructuredSignal", qwo::StructuredSignal);
    add!("StructuredFinding", qwo::StructuredFinding);
    add!("StructuredOverride", qwo::StructuredOverride);
    add!("ConfidenceLevel", qwo::ConfidenceLevel);
    add!("AgenticPhaseOutput", qwo::AgenticPhaseOutput);
    add!("AgenticStatus", qwo::AgenticStatus);
    add!("FileChange", qwo::FileChange);
    add!("FindingOutput", qwo::FindingOutput);
    add!("ReflectionFixOutput", qwo::ReflectionFixOutput);
    add!("FlowEvent", qae::FlowEvent);
    add!("AppEvent", qae::AppEvent);

    // ── qontinui-types: ai_workflows ──
    add!("ExecutionStep", qaw::ExecutionStep);
    add!("AiWorkflow", qaw::AiWorkflow);

    // ── qontinui-types: constraints ──
    add!("ConstraintSeverity", qc::ConstraintSeverity);
    add!("ConstraintCheck", qc::ConstraintCheck);
    add!("Constraint", qc::Constraint);
    add!("ConstraintResult", qc::ConstraintResult);
    add!("ConstraintViolation", qc::ConstraintViolation);
    add!("ResourceLimits", qc::ResourceLimits);
    add!("ConstraintProposal", qc::ConstraintProposal);
    add!("NewConstraintProposal", qc::NewConstraintProposal);
    add!("BuiltinOverrideProposal", qc::BuiltinOverrideProposal);
    add!("ValidateConfigRequest", qc::ValidateConfigRequest);
    add!("ValidateConfigResponse", qc::ValidateConfigResponse);
    add!("ReadConfigResponse", qc::ReadConfigResponse);
    add!("WriteConfigRequest", qc::WriteConfigRequest);
    add!("WriteConfigResponse", qc::WriteConfigResponse);

    // ── qontinui-types: scheduler ──
    add!("ScheduleExpression", qs::ScheduleExpression);
    add!("ConditionScheduleConfig", qs::ConditionScheduleConfig);
    add!("IdleCondition", qs::IdleCondition);
    add!("RepositoryWatch", qs::RepositoryWatch);
    add!(
        "RepositoryInactiveCondition",
        qs::RepositoryInactiveCondition
    );
    add!("ScheduleConditions", qs::ScheduleConditions);
    add!("ConditionStatus", qs::ConditionStatus);
    add!("ScheduledTaskType", qs::ScheduledTaskType);
    add!("ScheduledTaskStatus", qs::ScheduledTaskStatus);
    add!("TaskExecutionRecord", qs::TaskExecutionRecord);
    add!("ScheduledTask", qs::ScheduledTask);
    add!("SchedulerSettings", qs::SchedulerSettings);
    add!("SchedulerStatus", qs::SchedulerStatus);
    add!("NextTaskInfo", qs::NextTaskInfo);
    add!("CreateScheduledTaskRequest", qs::CreateScheduledTaskRequest);
    add!("UpdateScheduledTaskRequest", qs::UpdateScheduledTaskRequest);

    // ── qontinui-types: workflow frame ──
    add!("RoutingRule", qw::RoutingRule);
    add!("ModelOverrideConfig", qw::ModelOverrideConfig);
    add!("LogSourceSelection", qw::LogSourceSelection);
    add!("HealthCheckUrl", qw::HealthCheckUrl);
    add!("WorkflowArchitecture", qw::WorkflowArchitecture);
    add!("StageCondition", qw::StageCondition);
    add!("RetryPolicy", qw::RetryPolicy);
    add!("StageOutput", qw::StageOutput);
    add!("StageInput", qw::StageInput);
    add!("WorkflowStage", qw::WorkflowStage);
    add!("UnifiedWorkflow", qw::UnifiedWorkflow);

    // ── qontinui-types: workflow_step (canonical 4-variant step DTOs) ──
    // `CanonicalStep` is the strict 4-variant discriminated union.
    // `UnifiedStep` is the canonical-first, Value-fallback wrapper used for
    // payloads that may include runner-specific step types.
    add!("CanonicalStep", qws::CanonicalStep);
    add!("UnifiedStep", qws::UnifiedStep);
    add!("CommandStep", qws::CommandStep);
    add!("PromptStep", qws::PromptStep);
    add!("UiBridgeStep", qws::UiBridgeStep);
    add!("WorkflowStep", qws::WorkflowStep);
    add!("BaseStepFields", qws::BaseStepFields);
    add!("RetrySpec", qws::RetrySpec);
    add!("HttpMethod", qws::HttpMethod);
    add!("ApiContentType", qws::ApiContentType);
    add!("ApiAssertion", qws::ApiAssertion);
    add!("ApiAssertionType", qws::ApiAssertionType);
    add!("ApiAssertionOperator", qws::ApiAssertionOperator);
    add!("ApiVariableExtraction", qws::ApiVariableExtraction);
    add!("TestType", qws::TestType);
    add!("PlaywrightExecutionMode", qws::PlaywrightExecutionMode);
    add!("CheckType", qws::CheckType);
    add!("CommandMode", qws::CommandMode);
    add!("UiBridgeAction", qws::UiBridgeAction);
    add!("UiBridgeAssertType", qws::UiBridgeAssertType);
    add!("UiBridgeComparisonMode", qws::UiBridgeComparisonMode);
    add!("UiBridgeSeverity", qws::UiBridgeSeverity);
    add!("VerificationCategoryKind", qws::VerificationCategoryKind);
    add!("CommandStepPhase", qws::CommandStepPhase);
    add!("PromptStepPhase", qws::PromptStepPhase);
    add!("UiBridgeStepPhase", qws::UiBridgeStepPhase);
    add!("WorkflowStepPhase", qws::WorkflowStepPhase);

    // ── qontinui-types: FullRunnerStep (all 16 handler-registered variants) ──
    add!("FullRunnerStep", qws::FullRunnerStep);
    add!("CodeExecutionStep", qws::CodeExecutionStep);
    add!("ExecutePlaybookStep", qws::ExecutePlaybookStep);
    add!("A11yAction", qws::A11yAction);
    add!("NativeAccessibilityStep", qws::NativeAccessibilityStep);
    add!("RestartProcessStep", qws::RestartProcessStep);
    add!("SaveWorkflowArtifactStep", qws::SaveWorkflowArtifactStep);
    add!("WorkflowFixupMode", qws::WorkflowFixupMode);
    add!("WorkflowFixupStep", qws::WorkflowFixupStep);
    add!("UiBridgeDesignAuditStep", qws::UiBridgeDesignAuditStep);
    add!("VisualAssertionType", qws::VisualAssertionType);
    add!(
        "UiBridgeVisualAssertionStep",
        qws::UiBridgeVisualAssertionStep
    );
    add!("WorkflowRefStep", qws::WorkflowRefStep);
    add!("DagCancelStep", qws::DagCancelStep);
    add!("DagApprovalStep", qws::DagApprovalStep);
    add!("DagLoopStep", qws::DagLoopStep);

    // ── qontinui-types: geometry ──
    add!("CoordinateSystem", qg::CoordinateSystem);
    add!("Coordinates", qg::Coordinates);
    add!("Region", qg::Region);
    add!("MonitorPosition", qg::MonitorPosition);
    add!("Monitor", qg::Monitor);
    add!("VirtualDesktop", qg::VirtualDesktop);

    // ── qontinui-types: tree_events ──
    add!("NodeType", qte::NodeType);
    add!("NodeStatus", qte::NodeStatus);
    add!("TreeEventType", qte::TreeEventType);
    add!("ActionType", qte::ActionType);
    add!("MatchLocation", qte::MatchLocation);
    add!("TopMatch", qte::TopMatch);
    add!("RuntimeData", qte::RuntimeData);
    add!("StateContext", qte::StateContext);
    add!("TimingInfo", qte::TimingInfo);
    add!("Outcome", qte::Outcome);
    add!("NodeMetadata", qte::NodeMetadata);
    add!("TreeNode", qte::TreeNode);
    add!("PathElement", qte::PathElement);
    add!("TreeEvent", qte::TreeEvent);
    add!("DisplayNode", qte::DisplayNode);
    add!("TreeEventCreate", qte::TreeEventCreate);
    add!("TreeEventResponse", qte::TreeEventResponse);
    add!("TreeEventListResponse", qte::TreeEventListResponse);
    add!("ExecutionTreeResponse", qte::ExecutionTreeResponse);

    // ── qontinui-types: config (AI context + workflow category) ──
    add!("ContextAutoInclude", qcfg::ContextAutoInclude);
    add!("Context", qcfg::Context);
    add!("Category", qcfg::Category);

    // ── qontinui-types: accessibility ──
    add!("AccessibilityRole", qa::AccessibilityRole);
    add!("AccessibilityBackend", qa::AccessibilityBackend);
    add!("AccessibilityState", qa::AccessibilityState);
    add!("AccessibilityBounds", qa::AccessibilityBounds);
    add!("AccessibilityNode", qa::AccessibilityNode);
    add!("AccessibilitySnapshot", qa::AccessibilitySnapshot);
    add!("AccessibilitySelector", qa::AccessibilitySelector);
    add!("RoleCriterion", qa::RoleCriterion);

    // ── qontinui-types: targets (action-target discriminated union) ──
    add!("SearchStrategy", qt::SearchStrategy);
    add!("MatchMethod", qt::MatchMethod);
    add!("PollingConfig", qt::PollingConfig);
    add!("PatternOptions", qt::PatternOptions);
    add!("MatchAdjustment", qt::MatchAdjustment);
    add!("SearchOptions", qt::SearchOptions);
    add!("OcrEngine", qt::OcrEngine);
    add!("TextMatchType", qt::TextMatchType);
    add!("TextSearchOptions", qt::TextSearchOptions);
    add!("AccessibilityRoleCriterion", qt::AccessibilityRoleCriterion);
    add!("TargetConfig", qt::TargetConfig);

    // ── qontinui-types: rag (RAG dashboard + embedding sync) ──
    add!("JobStatus", qr::JobStatus);
    add!("RagProcessingStatus", qr::RagProcessingStatus);
    add!(
        "ComputeTextEmbeddingRequest",
        qr::ComputeTextEmbeddingRequest
    );
    add!(
        "ComputeTextEmbeddingResponse",
        qr::ComputeTextEmbeddingResponse
    );
    add!("ComputeEmbeddingRequest", qr::ComputeEmbeddingRequest);
    add!("ComputeEmbeddingResponse", qr::ComputeEmbeddingResponse);
    add!(
        "BatchComputeEmbeddingRequest",
        qr::BatchComputeEmbeddingRequest
    );
    add!("BatchEmbeddingResult", qr::BatchEmbeddingResult);
    add!(
        "BatchComputeEmbeddingResponse",
        qr::BatchComputeEmbeddingResponse
    );
    add!("EmbeddingResultItem", qr::EmbeddingResultItem);
    add!("EmbeddingResultsRequest", qr::EmbeddingResultsRequest);
    add!("EmbeddingResultsResponse", qr::EmbeddingResultsResponse);
    add!("RagProgressEvent", qr::RagProgressEvent);
    add!("RagCompletionEvent", qr::RagCompletionEvent);
    add!("JobSummary", qr::JobSummary);
    add!("RAGDashboardStats", qr::RAGDashboardStats);
    add!("EmbeddingItem", qr::EmbeddingItem);
    add!("EmbeddingListResponse", qr::EmbeddingListResponse);
    add!("JobItem", qr::JobItem);
    add!("JobListResponse", qr::JobListResponse);
    add!("SemanticSearchRequest", qr::SemanticSearchRequest);
    add!("SearchResultItem", qr::SearchResultItem);
    add!("SemanticSearchResponse", qr::SemanticSearchResponse);
    add!("StateFilterItem", qr::StateFilterItem);
    add!("StatesResponse", qr::StatesResponse);

    // ── qontinui-types: orchestration_config ──
    add!("OlConfig", qoc::OlConfig);
    add!("CreateOlConfigRequest", qoc::CreateOlConfigRequest);
    add!("UpdateOlConfigRequest", qoc::UpdateOlConfigRequest);
    add!("StallDetectorConfig", qoc::StallDetectorConfig);
    add!("SummarizationConfig", qoc::SummarizationConfig);
    add!("DecomposerConfig", qoc::DecomposerConfig);
    add!("OrchestrationLoopConfig", qoc::OrchestrationLoopConfig);
    add!("PipelineConfig", qoc::PipelineConfig);
    add!("BuildPhaseConfig", qoc::BuildPhaseConfig);
    add!("ImplementFixesConfig", qoc::ImplementFixesConfig);
    add!("DiagnosePhaseConfig", qoc::DiagnosePhaseConfig);
    add!("RootCauseCategory", qoc::RootCauseCategory);
    add!("DiagnosticResult", qoc::DiagnosticResult);
    add!("ExitStrategy", qoc::ExitStrategy);
    add!("BetweenIterations", qoc::BetweenIterations);
    add!("LoopPhase", qoc::LoopPhase);
    add!("OrchestrationLoopStatus", qoc::OrchestrationLoopStatus);
    add!("IterationResult", qoc::IterationResult);
    add!("ExitCheckResult", qoc::ExitCheckResult);
    add!("MultiLoopConfig", qoc::MultiLoopConfig);
    add!("MultiLoopEntry", qoc::MultiLoopEntry);
    add!("MultiLoopStatus", qoc::MultiLoopStatus);
    add!("LoopInstanceStatus", qoc::LoopInstanceStatus);

    // ── qontinui-types: process_management ──
    add!("ParserType", qpm::ParserType);
    add!("ProcessState", qpm::ProcessState);
    add!("OutputStream", qpm::OutputStream);
    add!("OutputLine", qpm::OutputLine);
    add!("ProcessConfig", qpm::ProcessConfig);
    add!("ProcessStatus", qpm::ProcessStatus);

    // ── qontinui-types: findings ──
    add!("FindingCategory", qfn::FindingCategory);
    add!("FindingSeverity", qfn::FindingSeverity);
    add!("FindingStatus", qfn::FindingStatus);
    add!("FindingActionType", qfn::FindingActionType);
    add!("FindingCodeContext", qfn::FindingCodeContext);
    add!("FindingUserInput", qfn::FindingUserInput);
    add!("FindingCreate", qfn::FindingCreate);
    add!("FindingBatchCreate", qfn::FindingBatchCreate);
    add!("FindingUpdate", qfn::FindingUpdate);
    add!("FindingDetail", qfn::FindingDetail);
    add!("FindingListResponse", qfn::FindingListResponse);
    add!("FindingSummary", qfn::FindingSummary);

    // ── qontinui-types: discovery ──
    add!("DiscoverySourceType", qdc::DiscoverySourceType);
    add!("TransitionTriggerType", qdc::TransitionTriggerType);
    add!("DiscoveryBoundingBox", qdc::DiscoveryBoundingBox);
    add!(
        "DiscoveryTransitionTrigger",
        qdc::DiscoveryTransitionTrigger
    );
    add!("DiscoveredStateImage", qdc::DiscoveredStateImage);
    add!("DiscoveredState", qdc::DiscoveredState);
    add!("DiscoveredTransition", qdc::DiscoveredTransition);
    add!("StateDiscoveryResult", qdc::StateDiscoveryResult);
    add!(
        "StateDiscoveryResultSummary",
        qdc::StateDiscoveryResultSummary
    );
    add!(
        "StateDiscoveryResultListResponse",
        qdc::StateDiscoveryResultListResponse
    );
    add!(
        "StateDiscoveryResultCreate",
        qdc::StateDiscoveryResultCreate
    );
    add!(
        "StateDiscoveryResultUpdate",
        qdc::StateDiscoveryResultUpdate
    );
    add!("StateMachineExport", qdc::StateMachineExport);
    add!("StateMachineImport", qdc::StateMachineImport);

    // ── qontinui-types: task_run (Session 4b path A) ──
    add!("TaskRunStatus", qtr::TaskRunStatus);
    add!("TaskType", qtr::TaskType);
    add!("TaskRun", qtr::TaskRun);
    add!("TaskRunBackend", qtr::TaskRunBackend);
    add!("TaskRunSession", qtr::TaskRunSession);
    add!("TaskRunFindingCategory", qtr::TaskRunFindingCategory);
    add!("TaskRunFindingSeverity", qtr::TaskRunFindingSeverity);
    add!("TaskRunFindingStatus", qtr::TaskRunFindingStatus);
    add!("TaskRunFindingActionType", qtr::TaskRunFindingActionType);
    add!("TaskRunFinding", qtr::TaskRunFinding);
    add!("TaskRunFindingSummary", qtr::TaskRunFindingSummary);
    add!("TaskRunBackendDetail", qtr::TaskRunBackendDetail);
    add!("TaskRunCreate", qtr::TaskRunCreate);
    add!("TaskRunUpdate", qtr::TaskRunUpdate);
    add!("TaskRunFindingCreate", qtr::TaskRunFindingCreate);
    add!("TaskRunFindingUpdate", qtr::TaskRunFindingUpdate);
    add!("RunPromptResponseData", qtr::RunPromptResponseData);
    add!("RunPromptResponse", qtr::RunPromptResponse);
    add!("RunPromptRequest", qtr::RunPromptRequest);
    add!("CreateTaskRunRequest", qtr::CreateTaskRunRequest);
    add!("TaskRunFilters", qtr::TaskRunFilters);
    add!("TaskRunFindingFilters", qtr::TaskRunFindingFilters);
    add!("Pagination", qtr::Pagination);
    add!("TaskRunListResponse", qtr::TaskRunListResponse);
    add!(
        "TaskRunFindingsListResponse",
        qtr::TaskRunFindingsListResponse
    );
    add!("FindingsSummary", qtr::FindingsSummary);
    add!("CheckIssueDetail", qtr::CheckIssueDetail);
    add!("IndividualCheckResult", qtr::IndividualCheckResult);
    add!("VerificationStepDetails", qtr::VerificationStepDetails);
    add!("StepExecutionConfig", qtr::StepExecutionConfig);
    add!("VerificationStepResult", qtr::VerificationStepResult);
    add!("GateEvaluationResult", qtr::GateEvaluationResult);
    add!("VerificationPhaseResult", qtr::VerificationPhaseResult);
    add!(
        "VerificationResultResponse",
        qtr::VerificationResultResponse
    );
    add!(
        "VerificationResultsListResponse",
        qtr::VerificationResultsListResponse
    );

    // ── qontinui-types: mcp_config ──
    add!("McpTransport", qmc::McpTransport);
    add!("StdioConfig", qmc::StdioConfig);
    add!("HttpConfig", qmc::HttpConfig);
    add!("McpServerConfig", qmc::McpServerConfig);
    add!("McpToolInputSchema", qmc::McpToolInputSchema);
    add!("McpToolInfo", qmc::McpToolInfo);
    add!("McpServerStatus", qmc::McpServerStatus);
    add!("CreateMcpServerInput", qmc::CreateMcpServerInput);
    add!("UpdateMcpServerInput", qmc::UpdateMcpServerInput);
    add!("McpToolCallResult", qmc::McpToolCallResult);
    add!("CreateMcpCallInput", qmc::CreateMcpCallInput);
    add!("McpCallRecord", qmc::McpCallRecord);
    add!("McpCallsResult", qmc::McpCallsResult);

    // ── qontinui-types: terminal ──
    add!("TerminalInfo", qtm::TerminalInfo);
    add!("TerminalOutputEvent", qtm::TerminalOutputEvent);
    add!("TerminalExitEvent", qtm::TerminalExitEvent);

    // ── qontinui-types: ticket_system ──
    add!("TicketSource", qts::TicketSource);
    add!("TicketState", qts::TicketState);
    add!("Ticket", qts::Ticket);
    add!("TicketComment", qts::TicketComment);
    add!("TicketProviderConfig", qts::TicketProviderConfig);

    // ── qontinui-types: execution (Session 4c tiers 1+2) ──
    // `ActionType` and `MatchLocation` also exist in tree_events; register
    // the execution-domain versions under prefixed names so both can coexist
    // in the shared schema registry.  TS re-exports them under the local
    // names via `export type { ExecutionActionType as ActionType }`.
    add!("RunType", qe::RunType);
    add!("RunStatus", qe::RunStatus);
    add!("ActionStatus", qe::ActionStatus);
    add!("ExecutionActionType", qe::ActionType);
    add!("ErrorType", qe::ErrorType);
    add!("IssueSeverity", qe::IssueSeverity);
    add!("ScreenshotType", qe::ScreenshotType);
    add!("RunnerMetadata", qe::RunnerMetadata);
    add!("WorkflowMetadata", qe::WorkflowMetadata);
    add!("ExecutionStats", qe::ExecutionStats);
    add!("CoverageData", qe::CoverageData);
    add!("LLMMetrics", qe::LLMMetrics);
    add!("ExecutionRunCreate", qe::ExecutionRunCreate);
    add!("ExecutionRunResponse", qe::ExecutionRunResponse);
    add!("ExecutionMatchLocation", qe::MatchLocation);
    add!("ActionExecutionCreate", qe::ActionExecutionCreate);
    add!("ActionExecutionResponse", qe::ActionExecutionResponse);
    add!("ScreenshotAnnotationShape", qe::ScreenshotAnnotationShape);
    add!("ScreenshotAnnotation", qe::ScreenshotAnnotation);
    add!("ExecutionScreenshotCreate", qe::ExecutionScreenshotCreate);
    add!(
        "ExecutionScreenshotResponse",
        qe::ExecutionScreenshotResponse
    );
    add!("ExecutionIssueCreate", qe::ExecutionIssueCreate);
    add!("ExecutionIssueResponse", qe::ExecutionIssueResponse);
    add!("ExecutionRunComplete", qe::ExecutionRunComplete);
    add!(
        "ExecutionRunCompleteResponse",
        qe::ExecutionRunCompleteResponse
    );
    // Additional types ported from api/execution.py.
    add!("IssueStatus", qe::IssueStatus);
    add!("IssueType", qe::IssueType);
    add!("IssueSource", qe::IssueSource);
    add!("ActionExecutionBatch", qe::ActionExecutionBatch);
    add!(
        "ActionExecutionBatchResponse",
        qe::ActionExecutionBatchResponse
    );
    add!("ExecutionIssueBatch", qe::ExecutionIssueBatch);
    add!(
        "ExecutionIssueBatchResponse",
        qe::ExecutionIssueBatchResponse
    );
    add!("ExecutionIssueUpdate", qe::ExecutionIssueUpdate);
    add!("VisualComparisonResult", qe::VisualComparisonResult);
    add!("ExecutionRunDetail", qe::ExecutionRunDetail);
    add!("ExecutionIssueDetail", qe::ExecutionIssueDetail);
    add!("ExecutionRunListResponse", qe::ExecutionRunListResponse);
    add!(
        "ActionExecutionListResponse",
        qe::ActionExecutionListResponse
    );
    add!("ExecutionIssueListResponse", qe::ExecutionIssueListResponse);
    add!("ActionReliabilityStats", qe::ActionReliabilityStats);
    add!("ExecutionTrendDataPoint", qe::ExecutionTrendDataPoint);
    add!("ExecutionTrendResponse", qe::ExecutionTrendResponse);
    add!("ModelCostBreakdown", qe::ModelCostBreakdown);
    add!("LLMCostSummary", qe::LLMCostSummary);
    add!("CostTrendDataPoint", qe::CostTrendDataPoint);
    add!("CostTrendResponse", qe::CostTrendResponse);
    add!("HistoricalActionQuery", qe::HistoricalActionQuery);
    add!("HistoricalActionResult", qe::HistoricalActionResult);
    add!("PlaybackFrameRequest", qe::PlaybackFrameRequest);

    // ── qontinui-types: state_machine (Session 4d) ──
    add!("StandardActionType", qsm::StandardActionType);
    add!("Point", qsm::Point);
    add!("ScrollDirection", qsm::ScrollDirection);
    add!("MouseButton", qsm::MouseButton);
    add!("TransitionActionValue", qsm::TransitionActionValue);
    add!("TransitionAction", qsm::TransitionAction);
    add!("DomainKnowledge", qsm::DomainKnowledge);
    add!("StateMachineConfig", qsm::StateMachineConfig);
    add!("StateMachineConfigCreate", qsm::StateMachineConfigCreate);
    add!("StateMachineConfigUpdate", qsm::StateMachineConfigUpdate);
    add!("StateMachineConfigFull", qsm::StateMachineConfigFull);
    add!("StateMachineState", qsm::StateMachineState);
    add!("StateMachineStateCreate", qsm::StateMachineStateCreate);
    add!("StateMachineStateUpdate", qsm::StateMachineStateUpdate);
    add!("StateMachineTransition", qsm::StateMachineTransition);
    add!(
        "StateMachineTransitionCreate",
        qsm::StateMachineTransitionCreate
    );
    add!(
        "StateMachineTransitionUpdate",
        qsm::StateMachineTransitionUpdate
    );
    add!("PathfindingRequest", qsm::PathfindingRequest);
    add!("PathfindingStep", qsm::PathfindingStep);
    add!("PathfindingResult", qsm::PathfindingResult);
    add!("TransitionExecutionResult", qsm::TransitionExecutionResult);
    add!("NavigationResult", qsm::NavigationResult);
    add!("TransitionInfo", qsm::TransitionInfo);
    add!(
        "AvailableTransitionsResult",
        qsm::AvailableTransitionsResult
    );
    add!("ActiveStatesResult", qsm::ActiveStatesResult);
    add!("InitialStatesSource", qsm::InitialStatesSource);
    add!("InitialStateRef", qsm::InitialStateRef);
    add!("ResolvedInitialStates", qsm::ResolvedInitialStates);
    add!(
        "ResolvedInitialStatesResult",
        qsm::ResolvedInitialStatesResult
    );
    add!("DiscoveryStrategy", qsm::DiscoveryStrategy);
    add!("StateNodeData", qsm::StateNodeData);
    add!("TransitionEdgeData", qsm::TransitionEdgeData);
    add!("StateMachineExportFormat", qsm::StateMachineExportFormat);

    // ── qontinui-types: verification ──
    add!("SuccessCriterion", qv::SuccessCriterion);
    add!("CriterionType", qv::CriterionType);
    add!("VerificationMethod", qv::VerificationMethod);
    add!("VerificationPlan", qv::VerificationPlan);
    add!("WorkerDomain", qv::WorkerDomain);
    add!("DomainAssignment", qv::DomainAssignment);
    add!("WorkerStatus", qv::WorkerStatus);
    add!("WorkerInstance", qv::WorkerInstance);
    add!("WorkerCoordinationMessage", qv::WorkerCoordinationMessage);
    add!("DomainVerificationResult", qv::DomainVerificationResult);
    add!("WorkerSignal", qv::WorkerSignal);
    add!("Finding", qv::Finding);
    add!("Confidence", qv::Confidence);
    add!("VerificationResult", qv::VerificationResult);
    add!(
        "IterationVerificationResults",
        qv::IterationVerificationResults
    );
    add!("VerificationAgentContext", qv::VerificationAgentContext);
    add!("ExtendIterationsRequest", qv::ExtendIterationsRequest);
    add!("CriterionOverride", qv::CriterionOverride);
    add!("OverrideCollection", qv::OverrideCollection);
    add!("TaskCompletionResult", qv::TaskCompletionResult);
    add!("StageTransition", qv::StageTransition);

    // ── qontinui-types: ui_bridge ──
    add!("ElementBbox", qub::ElementBbox);
    add!("ElementRect", qub::ElementRect);
    add!("ElementState", qub::ElementState);
    add!("ElementIdentifier", qub::ElementIdentifier);
    add!("UIBridgeElement", qub::UIBridgeElement);
    add!("ComponentActionInfo", qub::ComponentActionInfo);
    add!("UIBridgeComponent", qub::UIBridgeComponent);
    add!("WaitOptions", qub::WaitOptions);
    add!("ElementActionRequest", qub::ElementActionRequest);
    add!("ComponentActionRequest", qub::ComponentActionRequest);
    add!("ActionResponse", qub::ActionResponse);
    add!("DiscoveryRequest", qub::DiscoveryRequest);
    add!("DiscoveredElement", qub::DiscoveredElement);
    add!("DiscoveryResponse", qub::DiscoveryResponse);
    add!("WorkflowInfo", qub::WorkflowInfo);
    add!("UIBridgeSnapshot", qub::UIBridgeSnapshot);

    // ── qontinui-types: rag (extended) ──
    add!("ElementType", qr::ElementType);
    add!("BoundingBox", qr::BoundingBox);
    add!("GUIElementChunk", qr::GUIElementChunk);
    add!("EmbeddedElement", qr::EmbeddedElement);
    add!("VectorSearchResult", qr::VectorSearchResult);
    add!("ExportResult", qr::ExportResult);

    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_all_schemas() {
        let schemas = export_all_schemas();
        let obj = schemas.as_object().unwrap();

        // Verify all expected types are present
        assert!(
            obj.contains_key("WorkerOutput"),
            "Missing WorkerOutput schema"
        );
        assert!(
            obj.contains_key("AgenticPhaseOutput"),
            "Missing AgenticPhaseOutput schema"
        );
        assert!(obj.contains_key("AppEvent"), "Missing AppEvent schema");
        assert!(obj.contains_key("FlowEvent"), "Missing FlowEvent schema");
        assert_eq!(obj.len(), 405, "Expected 405 schema entries");

        // Sanity-check that qontinui_types re-exports are present
        assert!(
            obj.contains_key("Constraint"),
            "Missing Constraint schema from qontinui_types::constraints"
        );
        assert!(
            obj.contains_key("ScheduledTask"),
            "Missing ScheduledTask schema from qontinui_types::scheduler"
        );
        assert!(
            obj.contains_key("UnifiedWorkflow"),
            "Missing UnifiedWorkflow schema from qontinui_types::workflow"
        );
    }

    #[test]
    fn test_worker_output_schema_has_properties() {
        let schemas = export_all_schemas();
        let worker = &schemas["WorkerOutput"];
        assert!(
            worker.get("properties").is_some() || worker.get("$defs").is_some(),
            "WorkerOutput schema should have properties or $defs"
        );
    }

    #[test]
    fn test_app_event_schema_has_variants() {
        let schemas = export_all_schemas();
        let app_event = &schemas["AppEvent"];
        // Tagged enum should produce oneOf or similar structure
        let schema_str = serde_json::to_string(app_event).unwrap();
        assert!(
            schema_str.contains("ExecutorEvent") || schema_str.contains("event_type"),
            "AppEvent schema should reference its variants"
        );
    }
}
