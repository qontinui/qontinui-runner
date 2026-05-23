//! Workflow transport.
//!
//! Phase 2 boundary: workflow sessions are currently driven by
//! `unified_workflow_executor` end-to-end (the `task_runs` table is the
//! durable record). The Session primitive's responsibility for workflow
//! kinds is bookkeeping — register a `coord.sessions` row and an outbox
//! entry that the executor can link by `task_run_id`. The actual workflow
//! execution lifecycle stays in `unified_workflow_executor` for Phase 2;
//! Phase 9 collapses the legacy session tables (`project.sessions`,
//! `project.{agent,ai,automation,task_run,workflow_ai}_sessions`) into
//! `coord.sessions` with a `session_kind` discriminator.
//!
//! Until Phase 9, [`WorkflowTransport::start`] materializes a placeholder
//! task-run id; the executor links its real id via
//! [`crate::session::SessionRegistry::link_task_run`] once it allocates
//! one.

use tracing::warn;

use crate::session::intent::Intent;
use crate::session::SessionKind;

use super::{Transport, TransportError, TransportHandle};

pub struct WorkflowTransport;

impl WorkflowTransport {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WorkflowTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for WorkflowTransport {
    fn start(&self, intent: &Intent) -> Result<TransportHandle, TransportError> {
        if !matches!(
            intent.kind,
            SessionKind::Workflow | SessionKind::Automation | SessionKind::Debug
        ) {
            return Err(TransportError::InvalidIntent(format!(
                "Workflow transport only handles Workflow/Automation/Debug, got {:?}",
                intent.kind
            )));
        }
        let placeholder = format!("pending-{}", uuid::Uuid::new_v4());
        Ok(TransportHandle::Workflow {
            task_run_id: placeholder,
        })
    }

    fn write_input(&self, _handle: &TransportHandle, _bytes: &[u8]) -> Result<(), TransportError> {
        // Workflows don't accept ad-hoc input — the workflow definition
        // is the input. Mid-run intervention happens via the workflow
        // executor's own surfaces (step injection etc.).
        Err(TransportError::Unsupported(
            "workflow write_input — workflows are step-driven, not stream-driven",
        ))
    }

    fn resize(
        &self,
        _handle: &TransportHandle,
        _cols: u16,
        _rows: u16,
    ) -> Result<(), TransportError> {
        // No terminal — no resize.
        Ok(())
    }

    fn close(&self, handle: &TransportHandle) -> Result<(), TransportError> {
        match handle {
            TransportHandle::Workflow { task_run_id } => {
                if !task_run_id.starts_with("pending-") {
                    warn!(
                        "Workflow close on real task_run_id {} — workflow executor owns teardown",
                        task_run_id
                    );
                }
                Ok(())
            }
            other => Err(TransportError::Runtime(format!(
                "Workflow transport got non-Workflow handle: {:?}",
                other
            ))),
        }
    }
}
