//! Schema Export Module
//!
//! Aggregator over the cross-repo type registry. Most types come from
//! `qontinui-types` (`qontinui-schemas/rust/`); a handful of runner-local
//! payload + relay envelope types are also registered from
//! `crate::tauri_event_payloads` and `crate::relay_envelopes`.
//!
//! Used by the `export_schemas` binary to produce a single JSON object
//! mapping type names to their JSON Schemas for cross-language type
//! generation (TypeScript, Python).
//!
//! Drift between this registry and qontinui-schemas's checked-in TS /
//! Python artifacts is caught by two CI workflows: schemas-side
//! `schema-drift.yml` fires on `rust/src/**` changes; runner-side
//! `qontinui-types-drift.yml` fires on changes to this file (and the
//! three other runner-side schema files).

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
        accessibility as qa, ai_workflows as qaw, app_events as qae, apps as qap, config as qcfg,
        constraints as qc, discovery as qdc, execution as qe, findings as qfn, geometry as qg,
        ir as qir, mcp_config as qmc, orchestration_config as qoc, process_management as qpm,
        rag as qr, runner as qrn, scheduler as qs, spec_api_events as qsae, spec_check as qsc,
        state_machine as qsm, targets as qt, task_run as qtr, terminal as qtm,
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
    // Wire-format canvas panel struct carried inside AppEvent::CanvasUpdate's
    // `data.panel` field. Mirrors the runner's `mcp::canvas::StoredPanel`.
    add!("CanvasPanel", qae::CanvasPanel);
    // Typed UI Bridge IPC envelopes. The structs live in
    // `qontinui-schemas/rust/src/app_events.rs` (added by schemas commit
    // `2aca16e feat(events): typed UI Bridge request/response envelopes`)
    // and are serialized live by the runner at
    // `mcp/ui_bridge/request.rs:292` and `mcp/ui_bridge/screenshots.rs:1597`.
    // Registering them here keeps the `.d.ts` bindings already checked in at
    // `qontinui-schemas/ts/src/generated/` matching the runner's regen
    // output — without these, the gen-events-drift hook reports a
    // delete-only diff against the checked-in TS files.
    add!("UiBridgeRequestEnvelope", qae::UiBridgeRequestEnvelope);
    add!("UiBridgeResponseEnvelope", qae::UiBridgeResponseEnvelope);

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
    // Phase A (scheduler reliability) types: explicit top-level entries so
    // they get their own per-type TS / Python files (rather than being
    // inlined as anonymous defs inside ScheduledTask / ScheduledTaskType).
    add!("CatchUpPolicy", qs::CatchUpPolicy);
    add!("McpConnectionRef", qs::McpConnectionRef);
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
    add!("VgaActionKind", qws::VgaActionKind);
    add!("VgaAction", qws::VgaAction);
    add!("VgaAutomateStep", qws::VgaAutomateStep);
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

    // ── qontinui-types: ir (Spec API IR + Legacy wire types) ──
    add!("IrPageSpec", qir::IrPageSpec);
    add!("IrElementCriteria", qir::IrElementCriteria);
    add!("IrProvenance", qir::IrProvenance);
    add!("ProposalStatus", qir::ProposalStatus);
    add!("IrMetadata", qir::IrMetadata);
    add!("IrCrossRef", qir::IrCrossRef);
    add!("IrWaitSpec", qir::IrWaitSpec);
    add!("IrTransitionAction", qir::IrTransitionAction);
    add!("IrStateCondition", qir::IrStateCondition);
    add!("IrState", qir::IrState);
    add!("IrTransition", qir::IrTransition);
    add!("IrAssertionTarget", qir::IrAssertionTarget);
    add!("IrAssertion", qir::IrAssertion);
    add!("IrGroup", qir::IrGroup);
    add!("LegacyAssertionTarget", qir::LegacyAssertionTarget);
    add!("LegacyAssertion", qir::LegacyAssertion);
    add!("LegacyGroup", qir::LegacyGroup);
    add!("LegacyProcessStep", qir::LegacyProcessStep);
    add!("LegacyTransition", qir::LegacyTransition);
    add!("LegacyStateMachineState", qir::LegacyStateMachineState);
    add!("LegacyStateMachine", qir::LegacyStateMachine);
    add!("LegacySpec", qir::LegacySpec);

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

    // ── runner-local: findings (Tauri `finding_detected` / `finding_resolved`
    // payloads). Distinct from `qontinui_types::verification::Finding`
    // (different shape: `confidence`, `findingType`, `evidence`). Registered
    // under `Runner*` titles so both schemas coexist in the registry. The
    // canonical structs live in `crate::tauri_event_payloads` so the schema
    // export pipeline (here, in the lib) and the binary's `findings::types`
    // module share one source of truth. ──
    use crate::tauri_event_payloads as tep;
    add!("RunnerFinding", tep::Finding);
    add!("RunnerFindingCodeContext", tep::FindingCodeContext);
    add!("RunnerFindingUserInput", tep::FindingUserInput);

    // ── runner-local: dev-only seed-finding payload (`dev:seed-finding`
    // Tauri event from `commands::dev_findings`). Schema lives in
    // `tauri_event_payloads` to be visible to this lib-side aggregator. ──
    add!("DevSeedFindingPayload", tep::DevSeedFindingPayload);

    // ── runner-local: review-approved / review-rejected payloads
    // (`commands::productivity::approve_recommendation` /
    // `reject_recommendation`). Same shape on both channels — only the
    // `user_decision` string ("approved" | "rejected") differs. ──
    add!(
        "RecommendationReviewDecisionPayload",
        tep::RecommendationReviewDecisionPayload
    );

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

    // ── qontinui-types: spec_check (Plan 01 foundation) ──
    // `Confidence` collides with `qv::Confidence` (registered above) so the
    // spec-check variant is registered as `SpecCheckConfidence`. Downstream
    // Rust consumers use `qontinui_types::spec_check::Confidence`
    // directly; the generated TS / Python bindings emit
    // `SpecCheckConfidence` as the type name.
    // Step 1 — result + fingerprint + validation types (16)
    add!("SpecCheckResult", qsc::SpecCheckResult);
    add!("SpecCheckSummary", qsc::SpecCheckSummary);
    add!("RecommendedState", qsc::RecommendedState);
    add!("MatchOutcome", qsc::MatchOutcome);
    add!("SpecCheckConfidence", qsc::Confidence);
    add!("StateMatchResult", qsc::StateMatchResult);
    add!("AssertionResult", qsc::AssertionResult);
    add!("AssertionOutcome", qsc::AssertionOutcome);
    add!("MatchedElement", qsc::MatchedElement);
    add!("AssertionMiss", qsc::AssertionMiss);
    add!("CandidateMiss", qsc::CandidateMiss);
    add!("FieldDiff", qsc::FieldDiff);
    add!("MissReason", qsc::MissReason);
    add!("AssertionSeverityCounts", qsc::AssertionSeverityCounts);
    add!("BridgeFingerprint", qsc::BridgeFingerprint);
    add!("SpecValidation", qsc::SpecValidation);
    // Step 2 — policy + step-config types (8)
    add!("SpecCheckStepConfig", qsc::SpecCheckStepConfig);
    add!("SpecCheckPolicy", qsc::SpecCheckPolicy);
    add!("PolicyConjunct", qsc::PolicyConjunct);
    add!("AssertionScope", qsc::AssertionScope);
    add!("ConjunctRule", qsc::ConjunctRule);
    add!("PolicyEvaluation", qsc::PolicyEvaluation);
    add!("ConjunctEvaluation", qsc::ConjunctEvaluation);
    add!("PolicyStatus", qsc::PolicyStatus);

    // ── qontinui-types: spec_api_events (Plan 06 broadcast taxonomy) ──
    add!("SpecApiEvent", qsae::SpecApiEvent);

    // ── qontinui-types: apps (multi-tenant Spec API registry — Stream A) ──
    add!("App", qap::App);
    add!("RegisterAppRequest", qap::RegisterAppRequest);
    add!("UpdateAppRequest", qap::UpdateAppRequest);
    add!("AppListResponse", qap::AppListResponse);
    add!("AppError", qap::AppError);

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
    add!("UIBridgeModalInfo", qub::UIBridgeModalInfo);
    add!("UIBridgeModalStack", qub::UIBridgeModalStack);
    add!("UIBridgeCapturedToast", qub::UIBridgeCapturedToast);
    add!("UIBridgeToastContext", qub::UIBridgeToastContext);
    add!("UIBridgeUndoContext", qub::UIBridgeUndoContext);

    // ── qontinui-types: rag (extended) ──
    add!("ElementType", qr::ElementType);
    add!("BoundingBox", qr::BoundingBox);
    add!("GUIElementChunk", qr::GUIElementChunk);
    add!("EmbeddedElement", qr::EmbeddedElement);
    add!("VectorSearchResult", qr::VectorSearchResult);
    add!("ExportResult", qr::ExportResult);

    // ── qontinui-types: runner (canonical Runner entity) ──
    add!("Runner", qrn::Runner);
    add!("RunnerStatus", qrn::RunnerStatus);
    add!("RunnerUiError", qrn::RunnerUiError);
    add!("RunnerCrash", qrn::RunnerCrash);

    // ── runner-local: WS relay envelopes + backend-originated runner-status
    // events (`crate::relay_envelopes`). These bind the previously-untyped
    // payloads `mcp/backend_relay.rs::handle_outbound` ships over the WS
    // relay AND the events the Python backend pushes on the per-user
    // runner-status channel. Source of truth for the consumer-side
    // discriminated unions in `@qontinui/shared-types/tauri-events`. ──
    use crate::relay_envelopes as re;
    add!("RunnerRelayMessage", re::RunnerRelayMessage);
    add!("RunnerStatusEvent", re::RunnerStatusEvent);
    add!("RunnerConnectedConnection", re::RunnerConnectedConnection);

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
        assert!(
            obj.contains_key("UiBridgeRequestEnvelope"),
            "Missing UiBridgeRequestEnvelope schema"
        );
        assert!(
            obj.contains_key("UiBridgeResponseEnvelope"),
            "Missing UiBridgeResponseEnvelope schema"
        );
        assert!(
            obj.contains_key("SpecApiEvent"),
            "Missing SpecApiEvent schema (Plan 06)"
        );
        assert_eq!(obj.len(), 478, "Expected 478 schema entries");

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
