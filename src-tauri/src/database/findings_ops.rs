//! Findings operations (wrapper methods for findings::storage).
//!
//! Contains all CheckpointDb methods related to findings.

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Findings Operations (wrapper methods for findings::storage)
    // ========================================================================

    /// Get a finding by ID.
    pub fn get_finding(&self, id: &str) -> Result<Option<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_finding(&conn, id)
    }

    /// Get all findings for a task run.
    pub fn get_findings_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_findings_for_task(&conn, task_run_id)
    }

    /// Get findings by status for a task run.
    pub fn get_findings_by_status(
        &self,
        task_run_id: &str,
        status: &crate::findings::FindingStatus,
    ) -> Result<Vec<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_findings_by_status(&conn, task_run_id, status)
    }

    /// Update finding status.
    pub fn update_finding_status(
        &self,
        id: &str,
        status: &crate::findings::FindingStatus,
        resolution: Option<&str>,
        session_num: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        crate::findings::storage::update_finding_status(&conn, id, status, resolution, session_num)
    }

    /// Set user response for a finding.
    pub fn set_finding_user_response(&self, id: &str, response: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        crate::findings::storage::set_user_response(&conn, id, response)
    }

    /// Get summary statistics for a task run.
    pub fn get_finding_summary(
        &self,
        task_run_id: &str,
    ) -> Result<crate::findings::FindingSummary, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_finding_summary(&conn, task_run_id)
    }

    /// Format findings for inclusion in a continuation prompt.
    ///
    /// This creates a structured section showing resolved, outstanding,
    /// and needs_input findings to provide context for continuation sessions.
    pub fn format_findings_for_continuation_prompt(
        &self,
        task_run_id: &str,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::format_findings_for_continuation_prompt(&conn, task_run_id)
    }
}
