//! Types for the step injection system.

/// Context required for step injection parsing.
///
/// When present, the AI session will parse `[INJECT_STEP]...[/INJECT_STEP]` markers
/// and collect them for use as dynamic verification steps.
#[derive(Debug, Clone)]
pub struct StepInjectionContext {
    /// The execution ID of the current workflow run.
    pub execution_id: String,
}
