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

    /// Phase 8 (plan §D10) — **deliberately `None`.** Stated here rather than
    /// left to the trait default so the reason is at the site, and so a future
    /// reader does not re-derive it.
    ///
    /// A workflow run has no byte stream to tap. Three independent facts, each
    /// sufficient on its own:
    ///
    /// 1. [`WorkflowTransport`] is a unit struct — it holds no executor
    ///    handle, no manager, and no channel, so there is nothing to look a
    ///    stream up in.
    /// 2. `start` above stamps a `pending-<uuid>` placeholder into
    ///    [`TransportHandle::Workflow`], and nothing ever replaces it:
    ///    `SessionRegistry::link_task_run`, named as the linker in this
    ///    module's header, does not exist in the codebase, and
    ///    `SessionRecord::transport_handle` is written once in
    ///    `SessionRegistry::start_inner` and never mutated. So the handle
    ///    names no live run even in principle.
    /// 3. Workflows are step-driven, not stream-driven — the same property
    ///    `write_input` above refuses on. Their observable output is
    ///    structured step/progress events emitted through the Tauri event and
    ///    WS surfaces, not a terminal byte stream; there is no ordered chunk
    ///    sequence for the output pipe to coalesce.
    ///
    /// Synthesising one would put fabricated bytes into coord's transcript
    /// tiers, which is worse than the honest gap. Wiring real workflow output
    /// means giving the executor a byte-stream surface first — a separate
    /// piece of work, not a `tap_output` impl.
    fn tap_output(
        &self,
        _handle: &TransportHandle,
    ) -> Option<tokio::sync::broadcast::Receiver<String>> {
        None
    }
}
