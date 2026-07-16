//! Bundled built-in playbooks + the spawn/relaunch/nudge prompt builders.
//!
//! Product decision (merge-shepherd-fixer-PLAN, Open item 2, resolved
//! 2026-07-08): the product home for playbooks is **qontinui-web-served
//! records** (the Phase-2 `looping_agents` authoring surface); the built-in
//! playbooks ship as **runner-bundled defaults** that seed the registry and
//! serve as the offline fallback — web wins when present. Phase 1's
//! hard-coded shepherd uses the bundled copy below (`include_str!`, the same
//! vendoring pattern as `fleet_commands.rs`).
//!
//! The prompt builders encode the Tier-0 loop contract: the spawned claude
//! runs **ONE cycle then stops and waits** — the supervisor owns the cadence
//! (idle-nudge), the relaunch policy, and death-respawn. State lives in the
//! journal, so a fresh relaunch just re-reads it (NO `--resume`, NO
//! summarization).

/// The bundled Tier-1 Merge Shepherd playbook (vendored from
/// `qontinui-dev-notes/playbooks/merge-shepherd-playbook.md`).
pub const MERGE_SHEPHERD_PLAYBOOK: &str = include_str!("merge-shepherd-playbook.md");

/// Resolve a bundled playbook by `playbook_ref`. Phase 1 knows exactly one;
/// Phase 2 layers web-served records over this as the fallback.
pub fn playbook_for(playbook_ref: &str) -> Option<&'static str> {
    match playbook_ref {
        "merge-shepherd" => Some(MERGE_SHEPHERD_PLAYBOOK),
        _ => None,
    }
}

/// The one-cycle contract appended to every spawn/relaunch prompt.
const ONE_CYCLE_CONTRACT: &str = "Run ONE cycle now, then STOP and wait — do not loop, sleep, or \
     schedule anything yourself; the runner's supervisor will prompt you to \
     run your next cycle. Your context may be discarded between cycles at \
     any time, so persist everything that matters to your journal.";

/// Render the agent's repo scope for the prompt.
fn repos_line(repos: &[String]) -> String {
    if repos.is_empty() {
        "all repos this tenant owns".to_string()
    } else {
        repos.join(", ")
    }
}

/// Shared config block: journal path + repo scope.
fn config_block(journal_path: &str, repos: &[String]) -> String {
    format!(
        "## Deployment config\n\
         - `journal_path`: {journal_path}\n\
         - `repos`: {}\n",
        repos_line(repos)
    )
}

/// Initial prompt for a first-ever spawn: full playbook + config + the
/// one-cycle contract.
pub fn initial_prompt(playbook: &str, journal_path: &str, repos: &[String]) -> String {
    format!(
        "{playbook}\n\n---\n\n{}\n{ONE_CYCLE_CONTRACT}\n\nJournal every action and outcome to \
         {journal_path} (create it if absent).",
        config_block(journal_path, repos)
    )
}

/// Prompt for a fresh relaunch (context-low / K-cycle recycle / death-
/// respawn): same playbook + config, plus the explicit "you are a fresh
/// relaunch — the journal is your memory" framing.
pub fn relaunch_prompt(playbook: &str, journal_path: &str, repos: &[String]) -> String {
    format!(
        "{playbook}\n\n---\n\n{}\nYou are a FRESH RELAUNCH of a long-running looping agent — a \
         previous session of you already ran some cycles. Your conversation memory is gone by \
         design; your journal at {journal_path} is your memory. Read it first, then continue \
         from where it left off. {ONE_CYCLE_CONTRACT}",
        config_block(journal_path, repos)
    )
}

/// The cadence nudge submitted into the live idle tab.
pub fn nudge_prompt(journal_path: &str) -> String {
    format!(
        "Run your next cycle now. Re-derive state from ground truth (your journal at \
         {journal_path} + live sources) — do not trust conversation memory. When the cycle is \
         done, journal it, then stop and wait for the next nudge."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_shepherd_playbook_is_the_real_one() {
        assert!(MERGE_SHEPHERD_PLAYBOOK.contains("# Merge Shepherd"));
        // Core contract lines the loop depends on.
        assert!(MERGE_SHEPHERD_PLAYBOOK.contains("stateless per iteration"));
        assert!(MERGE_SHEPHERD_PLAYBOOK.contains("No admin merges, ever"));
        assert!(playbook_for("merge-shepherd") == Some(MERGE_SHEPHERD_PLAYBOOK));
        assert!(playbook_for("no-such-playbook").is_none());
    }

    #[test]
    fn initial_prompt_carries_playbook_config_and_contract() {
        let p = initial_prompt(
            "PLAYBOOK BODY",
            "C:/x/journal.jsonl",
            &["qontinui/qontinui-runner".to_string()],
        );
        assert!(p.starts_with("PLAYBOOK BODY"));
        assert!(p.contains("C:/x/journal.jsonl"));
        assert!(p.contains("qontinui/qontinui-runner"));
        assert!(p.contains("Run ONE cycle now"));
        assert!(p.contains("supervisor will prompt you"));
        assert!(p.contains("create it if absent"));
    }

    #[test]
    fn empty_repos_means_all_tenant_repos() {
        let p = initial_prompt("PB", "C:/j.jsonl", &[]);
        assert!(p.contains("all repos this tenant owns"));
    }

    #[test]
    fn relaunch_prompt_frames_journal_as_memory() {
        let p = relaunch_prompt("PB", "C:/j.jsonl", &[]);
        assert!(p.starts_with("PB"));
        assert!(p.contains("FRESH RELAUNCH"));
        assert!(p.contains("your journal at C:/j.jsonl is your memory"));
        assert!(p.contains("Read it first"));
        assert!(p.contains("Run ONE cycle now"));
    }

    #[test]
    fn nudge_prompt_is_one_cycle_and_journal_grounded() {
        let p = nudge_prompt("C:/j.jsonl");
        assert!(p.contains("Run your next cycle now"));
        assert!(p.contains("C:/j.jsonl"));
        assert!(p.contains("stop and wait"));
    }
}
