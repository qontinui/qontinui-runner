//! Tool-policy → Claude CLI spawn-argument translation (Phase 3).
//!
//! Pure helpers that translate a [`ToolPolicy`] into the CLI flags and the
//! ephemeral `--settings` JSON block the runner uses to enforce tool gating
//! under `--permission-mode bypassPermissions`.
//!
//! ## Empirically-verified carrier shapes (claude 2.1.167, under bypass)
//! - Tool-NAME allow:  `--allowedTools <t1> <t2> ...`            (honored)
//! - Tool-NAME deny:   `--disallowedTools <n1> <n2> ...`         (honored)
//! - Command-ARG deny: a `permissions.deny` rule `Tool(<prefix>:*)` written
//!   into a `--settings <file>` JSON block. A bare `--disallowedTools
//!   "Bash(...)"` specifier is IGNORED under bypass, so the settings carrier
//!   is the only thing that works for argument-scoped denies.
//!
//! Because the `--settings` carrier can always be constructed, an arg-scoped
//! deny never silently evaporates: node validation only rejects a *malformed*
//! deny token, never "no constructible carrier".

use crate::workflow::dag_schema::{DenyEntry, ToolPolicy};

/// Translate a [`ToolPolicy`] into Claude CLI spawn arguments under
/// `bypassPermissions`.
///
/// Returns `(extra_cli_args, optional_settings_json_string)`:
/// - `allow`               → `["--allowedTools", t1, t2, ...]`
/// - tool-name denies      → `["--disallowedTools", n1, n2, ...]`
/// - arg-scoped denies     → `permissions.deny` rules `"<Tool>(<prefix>:*)"` in
///   the returned settings JSON string (`None` when there are no arg-scoped
///   denies).
///
/// An empty / `None` allow contributes no `--allowedTools`; a policy with no
/// denies and no allow yields `(vec![], None)` — a true no-op.
pub fn build_tool_policy_cli(policy: &ToolPolicy) -> (Vec<String>, Option<String>) {
    let mut args: Vec<String> = Vec::new();

    // ── Allow-list → --allowedTools t1 t2 ... ────────────────────────────
    if let Some(allow) = policy.allow.as_ref() {
        let tools: Vec<String> = allow
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect();
        if !tools.is_empty() {
            args.push("--allowedTools".to_string());
            args.extend(tools);
        }
    }

    // ── Denies: split tool-name vs command-arg-scoped via classified_denies ──
    let mut tool_name_denies: Vec<String> = Vec::new();
    let mut deny_rules: Vec<String> = Vec::new();
    for entry in policy.classified_denies() {
        match entry {
            DenyEntry::ToolName(name) => tool_name_denies.push(name),
            DenyEntry::CommandScoped {
                tool,
                command_prefix,
            } => {
                // Canonical settings grammar: Tool(<prefix>:*)
                deny_rules.push(format!("{}({}:*)", tool, command_prefix));
            }
        }
    }

    if !tool_name_denies.is_empty() {
        args.push("--disallowedTools".to_string());
        args.extend(tool_name_denies);
    }

    let settings = if deny_rules.is_empty() {
        None
    } else {
        let settings_json = serde_json::json!({
            "permissions": { "deny": deny_rules }
        });
        Some(settings_json.to_string())
    };

    (args, settings)
}

/// Decide whether an observed tool name violates the policy (the pure core of
/// the FailNode detector). A tool violates the policy iff:
/// - it is NOT permitted by the allow-list (when an allow-list is present and
///   non-empty, mirroring [`ToolGuard`] semantics — empty allow = unrestricted), OR
/// - it matches a tool-name deny entry.
///
/// Command-arg-scoped denies are NOT checked here: under bypass the CLI's
/// `--settings permissions.deny` carrier already blocks them natively (the
/// detector is defense-in-depth for the tool-NAME layer). Tool names are
/// compared case-sensitively, matching the Claude CLI's own tool identifiers.
pub fn tool_violates_policy(policy: &ToolPolicy, tool_name: &str) -> bool {
    // Allow-list check (empty/None ⇒ unrestricted).
    if let Some(allow) = policy.allow.as_ref() {
        let allowed: Vec<&str> = allow
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        if !allowed.is_empty() && !allowed.contains(&tool_name) {
            return true;
        }
    }

    // Tool-name deny check.
    policy
        .classified_denies()
        .iter()
        .any(|e| matches!(e, DenyEntry::ToolName(name) if name == tool_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::dag_schema::ViolationAction;

    fn policy(allow: Option<Vec<&str>>, deny: Option<Vec<&str>>) -> ToolPolicy {
        ToolPolicy {
            allow: allow.map(|v| v.into_iter().map(String::from).collect()),
            deny: deny.map(|v| v.into_iter().map(String::from).collect()),
            on_violation: ViolationAction::default(),
        }
    }

    // ─────────────── build_tool_policy_cli: allow / deny / mixed / empty ───────

    // Plan-test (a): a node denying "Write" yields --disallowedTools Write.
    #[test]
    fn build_cli_deny_write_yields_disallowed_tools() {
        let (args, settings) = build_tool_policy_cli(&policy(None, Some(vec!["Write"])));
        assert_eq!(args, vec!["--disallowedTools", "Write"]);
        assert!(settings.is_none());
    }

    #[test]
    fn build_cli_allow_only() {
        let (args, settings) = build_tool_policy_cli(&policy(Some(vec!["Bash", "Read"]), None));
        assert_eq!(args, vec!["--allowedTools", "Bash", "Read"]);
        assert!(settings.is_none());
    }

    #[test]
    fn build_cli_deny_only_multiple_tool_names() {
        let (args, settings) = build_tool_policy_cli(&policy(None, Some(vec!["Write", "Edit"])));
        assert_eq!(args, vec!["--disallowedTools", "Write", "Edit"]);
        assert!(settings.is_none());
    }

    // Plan-test (b): allow:[Bash] + deny:["Bash:git push --force"] ⇒
    // --allowedTools Bash AND a Bash(git push --force:*) settings deny rule.
    #[test]
    fn build_cli_command_scoped_deny_goes_to_settings() {
        let (args, settings) = build_tool_policy_cli(&policy(
            Some(vec!["Bash"]),
            Some(vec!["Bash:git push --force"]),
        ));
        assert_eq!(args, vec!["--allowedTools", "Bash"]);
        let settings = settings.expect("settings JSON expected for arg-scoped deny");
        let v: serde_json::Value = serde_json::from_str(&settings).expect("valid JSON");
        let deny = v["permissions"]["deny"]
            .as_array()
            .expect("permissions.deny array");
        assert_eq!(deny.len(), 1);
        assert_eq!(deny[0], "Bash(git push --force:*)");
    }

    #[test]
    fn build_cli_mixed_tool_name_and_command_scoped() {
        let (args, settings) = build_tool_policy_cli(&policy(
            Some(vec!["Bash", "Read"]),
            Some(vec!["Write", "Bash:rm -rf"]),
        ));
        assert_eq!(
            args,
            vec![
                "--allowedTools",
                "Bash",
                "Read",
                "--disallowedTools",
                "Write",
            ]
        );
        let settings = settings.expect("settings JSON expected");
        let v: serde_json::Value = serde_json::from_str(&settings).unwrap();
        assert_eq!(v["permissions"]["deny"][0], "Bash(rm -rf:*)");
    }

    // Back-compat: empty / None policy ⇒ zero args + no settings.
    #[test]
    fn build_cli_empty_policy_is_noop() {
        let (args, settings) = build_tool_policy_cli(&policy(None, None));
        assert!(args.is_empty(), "expected zero cli args, got {:?}", args);
        assert!(settings.is_none());
    }

    #[test]
    fn build_cli_empty_allow_vec_is_noop_for_allow() {
        let (args, settings) = build_tool_policy_cli(&policy(Some(vec![]), None));
        assert!(args.is_empty());
        assert!(settings.is_none());
    }

    // ─────────────── tool_violates_policy (FailNode detector core) ────────────

    // Plan-test (a): an observed Write under a deny:[Write] policy is a violation.
    #[test]
    fn violation_detected_for_denied_tool_name() {
        let p = policy(None, Some(vec!["Write"]));
        assert!(tool_violates_policy(&p, "Write"));
        assert!(!tool_violates_policy(&p, "Read"));
    }

    #[test]
    fn violation_detected_for_tool_outside_allow_list() {
        let p = policy(Some(vec!["Bash", "Read"]), None);
        assert!(tool_violates_policy(&p, "Write"));
        assert!(!tool_violates_policy(&p, "Bash"));
        assert!(!tool_violates_policy(&p, "Read"));
    }

    #[test]
    fn empty_allow_is_unrestricted() {
        let p = policy(Some(vec![]), None);
        assert!(!tool_violates_policy(&p, "Write"));
    }

    #[test]
    fn command_scoped_deny_is_not_a_tool_name_violation() {
        // Bash is allowed; the arg-scoped deny is enforced by the CLI settings
        // carrier, not the tool-name detector — so Bash itself is not a violation.
        let p = policy(Some(vec!["Bash"]), Some(vec!["Bash:git push --force"]));
        assert!(!tool_violates_policy(&p, "Bash"));
    }
}
