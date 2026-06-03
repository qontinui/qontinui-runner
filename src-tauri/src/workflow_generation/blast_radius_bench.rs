//! Offline value benchmark for the resolved-import `Ξ_AST` work (Phases 1–2).
//!
//! Replays the OLD `String::contains` `blast_radius` heuristic against the NEW
//! exact `resolved_target` lookup over real repositories, and adjudicates every
//! disagreement using the resolved edges themselves. Also reports resolver
//! coverage (how much of each repo's internal import graph the deterministic
//! resolver actually binds — the direct measurement Q1 deferred).
//!
//! This is gated `#[cfg(test)]` + `#[ignore]` so it never runs in CI. Run it by
//! pointing it at real checkouts:
//!
//! ```text
//! QONTINUI_BENCH_REPOS="D:/qontinui-root/qontinui-runner,D:/qontinui-root/qontinui-coord" \
//!   cargo test --bin qontinui-runner blast_radius_value_bench -- --ignored --nocapture
//! ```
//!
//! Methodology: each file F is treated as the single changed file. `directly_affected`
//! is computed two ways and the importer sets compared:
//!   * OLD — `cf.contains(module) || module.contains(cf_stem)` over `to_module`.
//!   * NEW — `edge.resolved_target == Some(F)`.
//! Disagreements are bucketed:
//!   * old-only + a triggering edge that resolves to a *different* file or is `External`
//!     => **definite false positive** of the old heuristic (it linked F's importer to F,
//!     but that importer's actual import goes elsewhere / is third-party).
//!   * old-only where every triggering edge is `Unresolved` => **ambiguous** (old guessed;
//!     the resolver couldn't bind it — a coverage gap, not a proven old-correct link).
//!   * new-only => a **real resolved edge the substring heuristic missed** (classic
//!     tsconfig-alias / index import whose specifier shares no substring with the path).

use super::code_graph::{CodeGraph, ResolutionKind};
use std::collections::{HashMap, HashSet};
use std::path::Path;

fn norm_module(to_module: &str) -> String {
    to_module.replace('.', "/")
}

fn cf_stem(cf: &str) -> &str {
    cf.trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".js")
        .trim_end_matches(".py")
        .trim_end_matches(".rs")
}

/// Faithful port of the pre-merge old heuristic match test for one (edge, changed_file).
fn old_matches(norm: &str, cf: &str) -> bool {
    if norm.is_empty() {
        // The old code's `cf.contains("")` was always true; preserve that behavior so
        // the benchmark reflects the heuristic's real failure modes.
        return true;
    }
    cf.contains(norm) || norm.contains(cf_stem(cf))
}

struct RepoReport {
    repo: String,
    files: usize,
    imports: usize,
    // resolution coverage
    kind_counts: HashMap<&'static str, usize>,
    internal_bound: usize,
    unresolved: usize,
    external: usize,
    // blast-radius delta (summed over all sampled files-as-single-changed)
    sampled_files: usize,
    old_links: usize,
    new_links: usize,
    agree: usize,
    definite_fp: usize,
    ambiguous_old_only: usize,
    new_only: usize,
}

fn kind_str(k: ResolutionKind) -> &'static str {
    match k {
        ResolutionKind::Relative => "relative",
        ResolutionKind::TsconfigPath => "tsconfig_path",
        ResolutionKind::PackageIndex => "package_index",
        ResolutionKind::PythonModule => "python_module",
        ResolutionKind::RustMod => "rust_mod",
        ResolutionKind::External => "external",
        ResolutionKind::Unresolved => "unresolved",
    }
}

fn analyze_repo(
    repo: &str,
    fp_examples: &mut Vec<String>,
    miss_examples: &mut Vec<String>,
) -> RepoReport {
    let graph = CodeGraph::build(Path::new(repo));

    // ---- resolution coverage ----
    let mut kind_counts: HashMap<&'static str, usize> = HashMap::new();
    let (mut internal_bound, mut unresolved, mut external) = (0usize, 0usize, 0usize);
    for e in &graph.imports {
        *kind_counts.entry(kind_str(e.resolution)).or_insert(0) += 1;
        match e.resolution {
            ResolutionKind::External => external += 1,
            ResolutionKind::Unresolved => unresolved += 1,
            _ => internal_bound += 1,
        }
    }

    // ---- precompute ----
    let norms: Vec<String> = graph
        .imports
        .iter()
        .map(|e| norm_module(&e.to_module))
        .collect();
    // reverse map: resolved_target -> set of importer files (for NEW directly_affected)
    let mut rev: HashMap<&str, HashSet<&str>> = HashMap::new();
    for e in &graph.imports {
        if let Some(t) = e.resolved_target.as_deref() {
            rev.entry(t).or_default().insert(e.from_file.as_str());
        }
    }

    // Bound the work so a 4k-file repo stays a few seconds. Sample evenly if needed.
    let max_files: usize = std::env::var("BENCH_MAX_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2500);
    let all: Vec<&str> = graph.files.iter().map(|f| f.path.as_str()).collect();
    let step = (all.len() / max_files).max(1);
    let sample: Vec<&str> = all.iter().copied().step_by(step).collect();

    let (mut old_links, mut new_links, mut agree) = (0usize, 0usize, 0usize);
    let (mut definite_fp, mut ambiguous_old_only, mut new_only) = (0usize, 0usize, 0usize);

    for &f in &sample {
        // NEW importer set: exact resolved edges pointing at f.
        let new_set: HashSet<&str> = rev
            .get(f)
            .map(|s| s.iter().copied().filter(|&imp| imp != f).collect())
            .unwrap_or_default();

        // OLD importer set: any edge of importer substring-matches f.
        let mut old_set: HashSet<&str> = HashSet::new();
        for (i, e) in graph.imports.iter().enumerate() {
            if e.from_file == f {
                continue;
            }
            if old_matches(&norms[i], f) {
                old_set.insert(e.from_file.as_str());
            }
        }

        old_links += old_set.len();
        new_links += new_set.len();
        agree += old_set.intersection(&new_set).count();

        // old-only adjudication
        for &imp in old_set.difference(&new_set) {
            // triggering edges: edges of `imp` that substring-matched f
            let mut saw_external_or_other = false;
            let mut saw_unresolved = false;
            let mut example_mod = "";
            let mut example_target: Option<&str> = None;
            let mut example_kind = ResolutionKind::Unresolved;
            for (i, e) in graph.imports.iter().enumerate() {
                if e.from_file == imp && old_matches(&norms[i], f) {
                    match e.resolution {
                        ResolutionKind::External => {
                            saw_external_or_other = true;
                            example_mod = &e.to_module;
                            example_kind = e.resolution;
                            example_target = None;
                        }
                        ResolutionKind::Unresolved => {
                            saw_unresolved = true;
                            if example_mod.is_empty() {
                                example_mod = &e.to_module;
                            }
                        }
                        _ => {
                            // resolved to a real file (necessarily != f, else it'd be in new_set)
                            saw_external_or_other = true;
                            example_mod = &e.to_module;
                            example_kind = e.resolution;
                            example_target = e.resolved_target.as_deref();
                        }
                    }
                }
            }
            if saw_external_or_other {
                definite_fp += 1;
                if fp_examples.len() < 8 {
                    let actual = match example_kind {
                        ResolutionKind::External => "→ EXTERNAL".to_string(),
                        _ => format!("→ resolves to `{}`", example_target.unwrap_or("?")),
                    };
                    fp_examples.push(format!(
                        "- `{}` old-linked to `{}` via specifier `{}` ({}), but that import actually {} — **false positive**",
                        imp, f, example_mod, kind_str(example_kind), actual
                    ));
                }
            } else if saw_unresolved {
                ambiguous_old_only += 1;
            }
        }

        // new-only: real resolved edges the old heuristic missed
        for &imp in new_set.difference(&old_set) {
            new_only += 1;
            if miss_examples.len() < 8 {
                // find the edge of imp resolving to f, report its kind + raw specifier
                if let Some(e) = graph
                    .imports
                    .iter()
                    .find(|e| e.from_file == imp && e.resolved_target.as_deref() == Some(f))
                {
                    miss_examples.push(format!(
                        "- `{}` imports `{}` via specifier `{}` ({}) — substring heuristic **missed** it",
                        imp, f, e.to_module, kind_str(e.resolution)
                    ));
                }
            }
        }
    }

    RepoReport {
        repo: repo.to_string(),
        files: graph.files.len(),
        imports: graph.imports.len(),
        kind_counts,
        internal_bound,
        unresolved,
        external,
        sampled_files: sample.len(),
        old_links,
        new_links,
        agree,
        definite_fp,
        ambiguous_old_only,
        new_only,
    }
}

#[test]
#[ignore = "value benchmark — run explicitly with QONTINUI_BENCH_REPOS set"]
fn blast_radius_value_bench() {
    let repos_env = std::env::var("QONTINUI_BENCH_REPOS")
        .expect("set QONTINUI_BENCH_REPOS=comma,separated,repo,paths");
    let repos: Vec<&str> = repos_env
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let mut fp_examples: Vec<String> = Vec::new();
    let mut miss_examples: Vec<String> = Vec::new();
    let mut reports: Vec<RepoReport> = Vec::new();
    for repo in &repos {
        reports.push(analyze_repo(repo, &mut fp_examples, &mut miss_examples));
    }

    println!("\n\n# Resolved-import Ξ_AST — offline value benchmark\n");

    println!("## 1. Resolver coverage (does it bind real code?)\n");
    println!("| Repo | files | imports | internal-bound | unresolved | external | coverage* |");
    println!("|---|---|---|---|---|---|---|");
    for r in &reports {
        let denom = r.internal_bound + r.unresolved;
        let cov = if denom == 0 {
            0.0
        } else {
            100.0 * r.internal_bound as f64 / denom as f64
        };
        println!(
            "| {} | {} | {} | {} | {} | {} | {:.1}% |",
            r.repo.rsplit(['/', '\\']).next().unwrap_or(&r.repo),
            r.files,
            r.imports,
            r.internal_bound,
            r.unresolved,
            r.external,
            cov
        );
    }
    println!("\n*coverage = internal-bound / (internal-bound + unresolved) — fraction of *internal-looking* imports the resolver bound (external excluded; they are honestly external).\n");

    println!("### Resolution-kind breakdown\n");
    println!("| Repo | relative | tsconfig_path | package_index | python_module | rust_mod | external | unresolved |");
    println!("|---|---|---|---|---|---|---|---|");
    for r in &reports {
        let g = |k: &str| r.kind_counts.get(k).copied().unwrap_or(0);
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            r.repo.rsplit(['/', '\\']).next().unwrap_or(&r.repo),
            g("relative"),
            g("tsconfig_path"),
            g("package_index"),
            g("python_module"),
            g("rust_mod"),
            g("external"),
            g("unresolved")
        );
    }

    println!("\n## 2. blast_radius accuracy — old substring vs new resolved\n");
    println!("Per repo, summed over sampled files-as-single-changed:\n");
    println!("| Repo | sampled | old links | new links | agree | old-only DEFINITE FP | old-only ambiguous | new-only (old missed) |");
    println!("|---|---|---|---|---|---|---|---|");
    let (mut t_old, mut t_new, mut t_agree, mut t_fp, mut t_amb, mut t_miss) = (0, 0, 0, 0, 0, 0);
    for r in &reports {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            r.repo.rsplit(['/', '\\']).next().unwrap_or(&r.repo),
            r.sampled_files,
            r.old_links,
            r.new_links,
            r.agree,
            r.definite_fp,
            r.ambiguous_old_only,
            r.new_only
        );
        t_old += r.old_links;
        t_new += r.new_links;
        t_agree += r.agree;
        t_fp += r.definite_fp;
        t_amb += r.ambiguous_old_only;
        t_miss += r.new_only;
    }
    println!(
        "| **TOTAL** | | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** |",
        t_old, t_new, t_agree, t_fp, t_amb, t_miss
    );

    let old_precision = if t_old == 0 {
        0.0
    } else {
        100.0 * (t_old - t_fp - t_amb) as f64 / t_old as f64
    };
    println!("\n**Old-heuristic precision lower bound:** {:.1}% of old `directly_affected` links are definite false positives or unresolvable-ambiguous ({} FP + {} ambiguous of {} old links).", 100.0 - old_precision, t_fp, t_amb, t_old);
    println!(
        "**Edges the old heuristic silently missed (new-only, real resolved imports):** {}.",
        t_miss
    );

    println!("\n## 3. Examples — old false positives (substring collisions)\n");
    if fp_examples.is_empty() {
        println!("_(none found)_");
    } else {
        for e in &fp_examples {
            println!("{e}");
        }
    }
    println!("\n## 4. Examples — edges the substring heuristic missed (alias/index)\n");
    if miss_examples.is_empty() {
        println!("_(none found)_");
    } else {
        for e in &miss_examples {
            println!("{e}");
        }
    }
    println!("\n# END BENCHMARK\n");
}
